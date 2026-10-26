use crate::budget::BudgetLedger;
use crate::config::{Config, JResult};
use crate::data::{Governance, ModelSink};
use crate::model::{ChatModel, ModelDelta};
use serde_json::Value;

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
        let inspected = inspect(&self.raw)?;
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
    let begin = text.rfind("-----BEGIN ")?;
    let tail = &text[begin..];
    if tail.contains("PRIVATE KEY-----") && !tail.contains("-----END ") {
        return Some(begin);
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
    base.trim_start_matches("https://")
        .trim_start_matches("http://")
        .split('/')
        .next()
        .filter(|host| !host.is_empty())
        .unwrap_or("openai-compatible")
        .to_string()
}

impl<'a> ModelGateway<'a> {
    pub fn new(cfg: &Config, governance: &'a Governance) -> JResult<Self> {
        let model = ChatModel::new(cfg)?;
        let sink = ModelSink {
            provider: provider_from_base(&cfg.api_base),
            model: cfg.model.clone(),
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
    ) -> JResult<Value> {
        budget.check_model_budget()?;
        let (redacted, envelopes) = self.governance.prepare_model_request(messages)?;
        self.governance
            .authorize_model_egress(&envelopes, &self.sink)?;
        let reply = self.model.complete(&redacted, tools)?;
        budget.record_usage(reply.prompt_tokens, reply.completion_tokens)?;
        let mut message = reply.message;
        if let Some(content) = message.get("content").and_then(Value::as_str) {
            let inspected = self
                .governance
                .inspect_model_output(content, "assistant-message")?;
            if let Some(object) = message.as_object_mut() {
                object.insert("content".into(), Value::String(inspected));
            }
        }
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
    ) -> JResult<Value> {
        budget.check_model_budget()?;
        let (redacted, envelopes) = self.governance.prepare_model_request(messages)?;
        self.governance
            .authorize_model_egress(&envelopes, &self.sink)?;
        let mut text = Preview::default();
        let mut reasoning = Preview::default();
        let reply = self
            .model
            .complete_streaming(&redacted, tools, &mut |delta| {
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
            })?;
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
        if let Some(content) = message.get("content").and_then(Value::as_str) {
            let inspected = self
                .governance
                .inspect_model_output(content, "assistant-message")?;
            if pem_hold_start(&inspected).is_some() {
                return Err("model output ended inside an unterminated private key block".into());
            }
            if let Some(object) = message.as_object_mut() {
                object.insert("content".into(), Value::String(inspected));
            }
        }
        Ok(message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(raw: &str) -> JResult<String> {
        Ok(raw.to_string())
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
    fn preview_rejects_a_changed_prefix() {
        let mut preview = Preview::default();
        preview.push("safe\n");
        assert_eq!(
            preview.release(plain, false).unwrap(),
            Some("safe\n".into())
        );
        preview.push("more\n");
        let error = preview
            .release(|raw| Ok(format!("changed {raw}")), false)
            .unwrap_err();
        assert!(error.contains("changed after release"), "{error}");
    }
}
