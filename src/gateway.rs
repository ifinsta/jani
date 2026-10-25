use crate::budget::BudgetLedger;
use crate::config::{Config, JResult};
use crate::data::{Governance, ModelSink};
use crate::model::ChatModel;
use serde_json::Value;

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
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
