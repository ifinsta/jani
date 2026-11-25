use crate::budget::BudgetLedger;
use crate::config::{Config, JResult, ModelChoice};
use crate::data::{Governance, ModelSink};
use crate::model::{ChatModel, ModelDelta};
use serde_json::Value;
use std::sync::atomic::{AtomicBool, Ordering};

const PREVIEW_TEXT: &str = "assistant-stream";
const PREVIEW_REASONING: &str = "assistant-reasoning";

/// Buffers streamed text until it can be inspected. Only complete lines are
/// released, and an unterminated private-key block is held from its header, so a
/// partial secret is never shown before redaction can run.
#[derive(Default)]
struct Preview {
    raw: String,
    released: String,
    checked: usize,
}

impl Preview {
    fn push(&mut self, chunk: &str) {
        self.raw.push_str(chunk);
    }

    fn release(
        &mut self,
        inspect: impl Fn(&str) -> JResult<String>,
        final_flush: bool,
    ) -> JResult<Option<String>> {
        // Inspection scans the whole buffered answer, so defer it until a new
        // line can actually be released instead of re-scanning every token.
        if !final_flush && !self.raw[self.checked..].contains('\n') {
            return Ok(None);
        }
        self.checked = self.raw.len();
        let raw = match pem_hold_start(&self.raw) {
            Some(hold) => &self.raw[..hold],
            None => &self.raw,
        };
        let inspected = inspect(raw)?;
        if !inspected.starts_with(&self.released) {
            return Err("stream preview changed after release".into());
        }
        let emitted = self.released.len();
        if !final_flush && !inspected[emitted..].contains('\n') {
            return Ok(None);
        }
        let mut cut = if final_flush {
            inspected.len()
        } else {
            inspected.rfind('\n').map(|index| index + 1).unwrap_or(0)
        }
        .max(emitted)
        .min(inspected.len());
        while cut > emitted && !inspected.is_char_boundary(cut) {
            cut -= 1;
        }
        if let Some(hold) = pem_hold_start(&inspected[emitted..cut]) {
            cut = emitted + hold;
            while cut > emitted && !inspected.is_char_boundary(cut) {
                cut -= 1;
            }
        }
        if cut <= emitted {
            return Ok(None);
        }
        let chunk = inspected[emitted..cut].to_string();
        self.released.push_str(&chunk);
        Ok(Some(chunk))
    }
}

fn pem_hold_start(text: &str) -> Option<usize> {
    let mut cursor = 0;
    while let Some(offset) = text[cursor..].find(crate::data::PEM_BEGIN) {
        let begin = cursor + offset;
        let label_start = begin + crate::data::PEM_BEGIN.len();
        let header = text[label_start..].split('\n').next().unwrap_or_default();
        let Some(label_len) = header.find("-----") else {
            return Some(begin);
        };
        let label = &header[..label_len];
        cursor = label_start + label_len + 5;
        if label.ends_with("PRIVATE KEY")
            && label
                .bytes()
                .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b' ')
        {
            let end = format!("{}{}-----", crate::data::PEM_END, label);
            let Some(close) = text[cursor..].find(&end) else {
                return Some(begin);
            };
            cursor += close + end.len();
        }
    }
    None
}

/// The only component allowed to see provider credentials and to transmit model
/// context. Every request is redacted and authorized by the data governance kernel
/// before it leaves the process.
pub struct ModelGateway<'a> {
    model: ChatModel,
    governance: &'a Governance,
    sink: ModelSink,
}

fn provider_from_base(base: &str) -> String {
    let authority = crate::net::url_authority(base);
    if authority.is_empty() {
        "openai-compatible".into()
    } else {
        authority.to_string()
    }
}

impl<'a> ModelGateway<'a> {
    pub fn new(cfg: &Config, choice: &ModelChoice, governance: &'a Governance) -> JResult<Self> {
        let model = ChatModel::new(choice)?;
        let sink = ModelSink {
            provider: provider_from_base(&choice.api_base),
            model: choice.model.clone(),
            provider_class: cfg.provider_class.clone(),
            region: cfg.provider_region.clone(),
        };
        Ok(Self {
            model,
            governance,
            sink,
        })
    }

    pub fn complete(
        &self,
        messages: &[Value],
        tools: &[Value],
        budget: &mut BudgetLedger,
        cancelled: Option<&AtomicBool>,
    ) -> JResult<Value> {
        self.complete_choosing(messages, tools, "auto", budget, cancelled)
    }

    /// The closing handoff of a turn: same governance, budget and egress checks,
    /// but the model may not call tools.
    pub fn complete_without_tools(
        &self,
        messages: &[Value],
        tools: &[Value],
        budget: &mut BudgetLedger,
        cancelled: Option<&AtomicBool>,
    ) -> JResult<Value> {
        self.complete_choosing(messages, tools, "none", budget, cancelled)
    }

    fn inspect_reply_message(&self, message: &mut Value) -> JResult<()> {
        if let Some(content) = message.get("content").and_then(Value::as_str) {
            if pem_hold_start(content).is_some() {
                return Err("model output ended inside an unterminated private key block".into());
            }
            let inspected = self
                .governance
                .inspect_model_output(content, "assistant-message")?;
            if let Some(object) = message.as_object_mut() {
                object.insert("content".into(), Value::String(inspected));
            }
        }
        if let Some(calls) = message.get_mut("tool_calls").and_then(Value::as_array_mut) {
            for call in calls {
                if let Some(args) = call
                    .pointer("/function/arguments")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                {
                    if pem_hold_start(&args).is_some() {
                        return Err(
                            "tool call arguments ended inside an unterminated private key block"
                                .into(),
                        );
                    }
                    let inspected = self
                        .governance
                        .inspect_model_output(&args, "assistant-tool-arguments")?;
                    if let Some(func) = call.get_mut("function").and_then(Value::as_object_mut) {
                        func.insert("arguments".into(), Value::String(inspected));
                    }
                }
            }
        }
        Ok(())
    }

    fn complete_choosing(
        &self,
        messages: &[Value],
        tools: &[Value],
        tool_choice: &str,
        budget: &mut BudgetLedger,
        cancelled: Option<&AtomicBool>,
    ) -> JResult<Value> {
        if cancelled.is_some_and(|token| token.load(Ordering::SeqCst)) {
            return Err("MODEL_CANCELLED".into());
        }
        budget.check_model_budget()?;
        let (redacted, envelopes) = self.governance.prepare_model_request(messages)?;
        self.governance
            .authorize_model_egress(&envelopes, &self.sink)?;
        let reply = self
            .model
            .complete_with(&redacted, tools, tool_choice, cancelled)?;
        budget.record_usage(reply.prompt_tokens, reply.completion_tokens)?;
        let mut message = reply.message;
        self.inspect_reply_message(&mut message)?;
        Ok(message)
    }

    /// Streams inspected previews of the answer and reasoning while the model
    /// works. Only complete lines are released after output inspection, so a
    /// partial secret is never displayed; the final message is inspected whole.
    pub fn complete_streaming(
        &self,
        messages: &[Value],
        tools: &[Value],
        budget: &mut BudgetLedger,
        on_delta: &mut dyn FnMut(ModelDelta) -> JResult<()>,
        cancelled: Option<&AtomicBool>,
    ) -> JResult<Value> {
        if cancelled.is_some_and(|token| token.load(Ordering::SeqCst)) {
            return Err("MODEL_CANCELLED".into());
        }
        budget.check_model_budget()?;
        let (redacted, envelopes) = self.governance.prepare_model_request(messages)?;
        self.governance
            .authorize_model_egress(&envelopes, &self.sink)?;
        let mut text = Preview::default();
        let mut reasoning = Preview::default();
        let reply = self.model.complete_streaming(
            &redacted,
            tools,
            &mut |delta| {
                match delta {
                    ModelDelta::Text(chunk) => {
                        text.push(&chunk);
                        if let Some(released) = text.release(
                            |raw| self.governance.inspect_model_output(raw, PREVIEW_TEXT),
                            false,
                        )? {
                            on_delta(ModelDelta::Text(released))?;
                        }
                    }
                    ModelDelta::Reasoning(chunk) => {
                        reasoning.push(&chunk);
                        if let Some(released) = reasoning.release(
                            |raw| self.governance.inspect_model_output(raw, PREVIEW_REASONING),
                            false,
                        )? {
                            on_delta(ModelDelta::Reasoning(released))?;
                        }
                    }
                }
                Ok(())
            },
            cancelled,
        )?;
        if let Some(released) = text.release(
            |raw| self.governance.inspect_model_output(raw, PREVIEW_TEXT),
            true,
        )? {
            on_delta(ModelDelta::Text(released))?;
        }
        if let Some(released) = reasoning.release(
            |raw| self.governance.inspect_model_output(raw, PREVIEW_REASONING),
            true,
        )? {
            on_delta(ModelDelta::Reasoning(released))?;
        }
        budget.record_usage(reply.prompt_tokens, reply.completion_tokens)?;
        let mut message = reply.message;
        self.inspect_reply_message(&mut message)?;
        Ok(message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(raw: &str) -> JResult<String> {
        Ok(raw.to_string())
    }

    fn governance() -> Governance {
        Governance::new(
            jani_core::policy::DataPolicySpec {
                redact_secrets: true,
                ..Default::default()
            },
            "tenant-a",
            "human-1",
            "SOFTWARE_DEV",
        )
    }

    #[test]
    fn provider_is_derived_from_the_endpoint() {
        assert_eq!(
            provider_from_base("https://api.openai.com/v1"),
            "api.openai.com"
        );
        assert_eq!(
            provider_from_base("http://127.0.0.1:8080/v1"),
            "127.0.0.1:8080"
        );
    }

    #[test]
    fn preview_releases_only_complete_lines() {
        let mut preview = Preview::default();
        preview.push("one");
        assert_eq!(preview.release(plain, false).unwrap(), None);
        preview.push("\ntwo");
        assert_eq!(preview.release(plain, false).unwrap(), Some("one\n".into()));
        assert_eq!(preview.release(plain, true).unwrap(), Some("two".into()));
        assert_eq!(preview.release(plain, true).unwrap(), None);
    }

    #[test]
    fn preview_inspects_before_release() {
        let mut preview = Preview::default();
        preview.push("key sk-secret\nnext");
        let redact = |raw: &str| Ok(raw.replace("sk-secret", "[REDACTED]"));
        assert_eq!(
            preview.release(redact, false).unwrap(),
            Some("key [REDACTED]\n".into())
        );
        assert_eq!(preview.release(redact, true).unwrap(), Some("next".into()));
    }

    #[test]
    fn preview_holds_an_unterminated_private_key() {
        let mut preview = Preview::default();
        let begin = "-----BEGIN RSA PRIVATE KEY-----\nMIIE\n";
        preview.push(begin);
        assert_eq!(preview.release(plain, false).unwrap(), None);
        assert_eq!(preview.release(plain, true).unwrap(), None);
        preview.push("-----END RSA PRIVATE KEY-----\n");
        let redact = |raw: &str| {
            if raw.contains("-----END ") {
                Ok("[REDACTED:PRIVATE_KEY]\n".to_string())
            } else {
                Ok(raw.to_string())
            }
        };
        let released = preview.release(redact, false).unwrap().unwrap();
        assert!(!released.contains("MIIE"), "{released}");
        assert!(released.contains("REDACTED"), "{released}");
    }

    #[test]
    fn pem_hold_starts_at_the_first_unmatched_private_header() {
        let prefix = concat!(
            "ordinary\n-----BEGIN PRIVATE KEY-----\nfirst\n-----END PRIVATE KEY-----\n",
            "-----BEGIN PUBLIC KEY-----\npublic\n-----END PUBLIC KEY-----\n"
        );
        let incomplete = format!(
            "{prefix}-----BEGIN RSA PRIVATE KEY-----\nprivate\n\
             -----END EC PRIVATE KEY-----\n\
             -----BEGIN PUBLIC KEY-----\npublic\n-----END PUBLIC KEY-----\n\
             -----BEGIN EC PRIVATE KEY-----\nsecond\n-----END EC PRIVATE KEY-----\n"
        );
        assert_eq!(pem_hold_start(&incomplete), Some(prefix.len()));
        assert_eq!(
            pem_hold_start(&format!("{incomplete}-----END RSA PRIVATE KEY-----\n")),
            None
        );
        assert_eq!(pem_hold_start(prefix), None);
    }

    #[test]
    fn preview_redacts_chunked_json_secret_assignments() {
        let g = governance();
        let inspect = |raw: &str| g.inspect_model_output(raw, PREVIEW_TEXT);
        let input = concat!(
            "before\n",
            r#"{"password":"x", "token":"a b", "apiKey":"say \"hello\" now"}"#,
            "\nafter"
        );
        let expected = "before\n{password=[REDACTED], token=[REDACTED], apiKey=[REDACTED]}\nafter";
        let mut preview = Preview::default();
        let mut emitted = String::new();
        for character in input.chars() {
            preview.push(&character.to_string());
            if let Some(chunk) = preview.release(inspect, false).unwrap() {
                emitted.push_str(&chunk);
            }
            assert!(expected.starts_with(&emitted));
        }
        emitted.push_str(&preview.release(inspect, true).unwrap().unwrap());
        assert_eq!(emitted, expected);
        assert_eq!(inspect(&preview.raw).unwrap(), expected);
        assert_eq!(preview.release(inspect, true).unwrap(), None);
    }

    #[test]
    fn preview_does_not_leak_unfinished_quoted_credentials() {
        let g = governance();
        let inspect = |raw: &str| g.inspect_model_output(raw, PREVIEW_TEXT);
        let mut preview = Preview::default();
        preview.push("before\npassword=\"supersecret\n");
        assert_eq!(
            preview.release(inspect, false).unwrap(),
            Some("before\n".into())
        );
        preview.push("still-private\"\nafter");
        assert_eq!(
            preview.release(inspect, false).unwrap(),
            Some("password=[REDACTED]\n".into())
        );
        assert_eq!(
            preview.release(inspect, true).unwrap(),
            Some("after".into())
        );
        assert_eq!(preview.released, "before\npassword=[REDACTED]\nafter");
        let mut truncated = Preview::default();
        truncated.push(r#"{"password":"supersecret"#);
        assert_eq!(truncated.release(inspect, false).unwrap(), None);
        assert_eq!(
            truncated.release(inspect, true).unwrap(),
            Some("{password=[REDACTED]".into())
        );
        assert!(!truncated.released.contains("supersecret"));
    }

    #[test]
    fn preview_redacts_multiple_private_blocks_among_public_blocks() {
        let g = governance();
        let inspect = |raw: &str| g.inspect_model_output(raw, PREVIEW_TEXT);
        let public = "-----BEGIN PUBLIC KEY-----\npublic\n-----END PUBLIC KEY-----\n";
        let input = format!(
            "before\n{public}-----BEGIN RSA PRIVATE KEY-----\nfirst-private\n\
             -----END RSA PRIVATE KEY-----\n{public}\
             -----BEGIN EC PRIVATE KEY-----\nsecond-private\n-----END EC PRIVATE KEY-----\nafter"
        );
        let expected = format!(
            "before\n{public}[REDACTED:PRIVATE_KEY]\n{public}[REDACTED:PRIVATE_KEY]\nafter"
        );
        let mut preview = Preview::default();
        let mut emitted = String::new();
        for character in input.chars() {
            preview.push(&character.to_string());
            if let Some(chunk) = preview.release(inspect, false).unwrap() {
                emitted.push_str(&chunk);
            }
            assert!(expected.starts_with(&emitted));
            assert!(!emitted.contains("first-private"));
            assert!(!emitted.contains("second-private"));
            assert!(!emitted.contains("PRIVATE KEY-----"));
        }
        emitted.push_str(&preview.release(inspect, true).unwrap().unwrap());
        assert_eq!(emitted, expected);
        assert_eq!(inspect(&preview.raw).unwrap(), expected);
    }

    #[test]
    fn preview_holds_public_and_mismatched_private_blocks_inside_a_private_block() {
        let g = governance();
        let inspect = |raw: &str| g.inspect_model_output(raw, PREVIEW_TEXT);
        let mut preview = Preview::default();
        preview.push("before\n-----BEGIN RSA PRIVATE KEY-----\nouter-private\n");
        assert_eq!(
            preview.release(inspect, false).unwrap(),
            Some("before\n".into())
        );
        for chunk in [
            "-----BEGIN PUBLIC KEY-----\npublic\n-----END PUBLIC KEY-----\n",
            "-----END EC PRIVATE KEY-----\nouter-private-middle\n",
            "-----BEGIN EC PRIVATE KEY-----\ninner-private\n-----END EC PRIVATE KEY-----\n",
            "outer-private-tail\n-----END RSA PRIVATE",
        ] {
            preview.push(chunk);
            assert_eq!(preview.release(inspect, false).unwrap(), None);
            assert_eq!(preview.release(inspect, true).unwrap(), None);
            assert_eq!(preview.released, "before\n");
        }
        preview.push(" KEY-----");
        assert_eq!(preview.release(inspect, false).unwrap(), None);
        assert_eq!(
            preview.release(inspect, true).unwrap(),
            Some("[REDACTED:PRIVATE_KEY]".into())
        );
        assert_eq!(preview.released, "before\n[REDACTED:PRIVATE_KEY]");
        assert_eq!(inspect(&preview.raw).unwrap(), preview.released);
        assert_eq!(preview.release(inspect, true).unwrap(), None);
    }

    #[test]
    fn preview_final_flush_holds_an_incomplete_private_header() {
        let g = governance();
        let inspect = |raw: &str| g.inspect_model_output(raw, PREVIEW_TEXT);
        let mut preview = Preview::default();
        preview.push("before\n-----BEGIN RSA PRIVATE");
        assert_eq!(
            preview.release(inspect, false).unwrap(),
            Some("before\n".into())
        );
        assert_eq!(preview.release(inspect, true).unwrap(), None);
        preview.push(" KEY-----\nprivate-body");
        assert_eq!(preview.release(inspect, false).unwrap(), None);
        assert_eq!(preview.release(inspect, true).unwrap(), None);
        assert_eq!(preview.released, "before\n");
    }

    #[test]
    fn preview_preserves_ordinary_and_public_output_with_governance() {
        let g = governance();
        let inspect = |raw: &str| g.inspect_model_output(raw, PREVIEW_TEXT);
        let input =
            "ordinary café\n-----BEGIN PUBLIC KEY-----\npublic\n-----END PUBLIC KEY-----\ndone";
        let mut preview = Preview::default();
        let mut emitted = String::new();
        for character in input.chars() {
            preview.push(&character.to_string());
            if let Some(chunk) = preview.release(inspect, false).unwrap() {
                emitted.push_str(&chunk);
            }
            assert!(input.starts_with(&emitted));
        }
        emitted.push_str(&preview.release(inspect, true).unwrap().unwrap());
        assert_eq!(emitted, input);
    }

    #[test]
    fn preview_rejects_a_changed_prefix() {
        let mut preview = Preview::default();
        preview.push("safe\n");
        assert_eq!(
            preview.release(plain, false).unwrap(),
            Some("safe\n".into())
        );
        preview.push("more\n");
        for final_flush in [false, true] {
            let error = preview
                .release(|raw| Ok(format!("changed {raw}")), final_flush)
                .unwrap_err();
            assert!(error.contains("changed after release"), "{error}");
        }
    }
}
