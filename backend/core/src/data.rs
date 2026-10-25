use crate::policy::DataPolicySpec;
use crate::types::{new_id, sha256_hex, Classification, DataEnvelope, Provenance};
use regex::Regex;
use serde_json::Value;
use std::sync::OnceLock;

struct SecretRule {
    pattern: Regex,
    replacement: &'static str,
}

fn secret_rules() -> &'static Vec<SecretRule> {
    static RULES: OnceLock<Vec<SecretRule>> = OnceLock::new();
    RULES.get_or_init(|| {
        let specs: &[(&str, &str)] = &[
            (
                r"(?s)-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----.*?-----END [A-Z0-9 ]*PRIVATE KEY-----",
                "[REDACTED:PRIVATE_KEY]",
            ),
            (r"\bAKIA[0-9A-Z]{16}\b", "[REDACTED:AWS_KEY]"),
            (r"\bgh[pousr]_[A-Za-z0-9]{20,}\b", "[REDACTED:GITHUB_TOKEN]"),
            (
                r"\bsk-(?:proj-|ant-)?[A-Za-z0-9_\-]{20,}\b",
                "[REDACTED:API_KEY]",
            ),
            (
                r"\bxox[baprs]-[A-Za-z0-9-]{10,}\b",
                "[REDACTED:SLACK_TOKEN]",
            ),
            (
                r"\beyJ[A-Za-z0-9_\-]{8,}\.[A-Za-z0-9_\-]{8,}\.[A-Za-z0-9_\-]{8,}\b",
                "[REDACTED:JWT]",
            ),
            (
                r#"(?i)\b(password|passwd|pwd|secret|api[_-]?key|(?:access[_-]?)?token)\b\s*[:=]\s*["']?[^\s"'\[\]]{8,}["']?"#,
                "$1=[REDACTED]",
            ),
        ];
        specs
            .iter()
            .map(|(pattern, replacement)| SecretRule {
                pattern: Regex::new(pattern).expect("valid secret regex"),
                replacement,
            })
            .collect()
    })
}

fn email_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"[A-Za-z0-9._%+\-]+@[A-Za-z0-9.\-]+\.[A-Za-z]{2,}").expect("valid email regex")
    })
}

fn ssn_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\b\d{3}-\d{2}-\d{4}\b").expect("valid SSN regex"))
}

fn card_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\b(?:\d[ -]?){13,19}\b").expect("valid card regex"))
}

fn luhn_valid(digits: &str) -> bool {
    let mut sum = 0u32;
    let mut double = false;
    for c in digits.chars().rev() {
        let Some(d) = c.to_digit(10) else { continue };
        let mut value = d;
        if double {
            value *= 2;
            if value > 9 {
                value -= 9;
            }
        }
        sum += value;
        double = !double;
    }
    sum.is_multiple_of(10)
}

fn contains_card(text: &str) -> bool {
    card_regex().find_iter(text).any(|m| {
        let digits: String = m.as_str().chars().filter(|c| c.is_ascii_digit()).collect();
        (13..=19).contains(&digits.len()) && luhn_valid(&digits)
    })
}

#[derive(Clone, Debug)]
pub struct Classified {
    pub original: Classification,
    pub effective: Classification,
    pub tags: Vec<String>,
    pub content: String,
    pub secrets_found: usize,
}

#[derive(Clone, Debug)]
pub struct ModelSink {
    pub provider: String,
    pub model: String,
    pub provider_class: String,
    pub region: String,
}

impl ModelSink {
    pub fn describe(&self) -> String {
        format!(
            "provider={} model={} class={} region={}",
            self.provider, self.model, self.provider_class, self.region
        )
    }
}

pub struct Governance {
    pub policy: DataPolicySpec,
    pub tenant_id: String,
    pub principal_id: String,
    pub purpose: String,
}

/// An omitted allowlist denies every value; `"*"` is the only explicit wildcard.
fn allowlisted(allowlist: &[String], value: &str) -> bool {
    allowlist.iter().any(|entry| entry == "*" || entry == value)
}

impl Governance {
    pub fn new(
        policy: DataPolicySpec,
        tenant_id: impl Into<String>,
        principal_id: impl Into<String>,
        purpose: impl Into<String>,
    ) -> Self {
        Self {
            policy,
            tenant_id: tenant_id.into(),
            principal_id: principal_id.into(),
            purpose: purpose.into(),
        }
    }

    pub fn classify(&self, source: &Provenance, content: &str) -> Classified {
        let mut classification = self.policy.source_default;
        let mut tags = vec![format!("PURPOSE:{}", self.purpose)];
        let mut text = content.to_string();
        let mut secrets_found = 0usize;
        let redact_secrets = self.policy.redact_secrets;

        for rule in secret_rules() {
            let matches = rule.pattern.find_iter(&text).count();
            if matches > 0 {
                secrets_found += matches;
                if redact_secrets {
                    text = rule
                        .pattern
                        .replace_all(&text, rule.replacement)
                        .into_owned();
                }
            }
        }
        if secrets_found > 0 {
            tags.push("SECRET_DETECTED".into());
            if redact_secrets {
                tags.push("REDACTED_SECRET".into());
            }
        }

        if email_regex().is_match(content) {
            tags.push("PII".into());
            classification = classification.max(Classification::Confidential);
        }
        if ssn_regex().is_match(content) || contains_card(content) {
            tags.push("PII".into());
            tags.push("REGULATED".into());
            classification = classification.max(Classification::Restricted);
            if !tags.iter().any(|t| t == "PCI") {
                tags.push("PCI".into());
            }
        }

        let effective = if secrets_found > 0 {
            if redact_secrets {
                classification.max(Classification::Confidential)
            } else {
                Classification::Secret
            }
        } else {
            classification
        };
        let original = if secrets_found > 0 {
            Classification::Secret
        } else {
            classification
        };
        tags.push(format!("SOURCE:{}", source.kind));
        Classified {
            original,
            effective,
            tags,
            content: text,
            secrets_found,
        }
    }

    pub fn envelope(&self, source: Provenance, classified: &Classified) -> DataEnvelope {
        DataEnvelope {
            data_id: new_id("data"),
            tenant_id: self.tenant_id.clone(),
            owner_principal: self.principal_id.clone(),
            source,
            classification: classified.effective,
            tags: classified.tags.clone(),
            purposes: vec![self.purpose.clone()],
            content_digest: sha256_hex(classified.content.as_bytes()),
            lineage: vec![],
            redacted: classified.secrets_found > 0,
        }
    }

    pub fn authorize_model_egress(
        &self,
        envelopes: &[DataEnvelope],
        sink: &ModelSink,
    ) -> Result<(), String> {
        if !allowlisted(&self.policy.allowed_purposes, &self.purpose) {
            return Err(format!(
                "purpose '{}' is not approved for model egress",
                self.purpose
            ));
        }
        if !allowlisted(&self.policy.allowed_providers, &sink.provider) {
            return Err(format!(
                "provider '{}' is not an approved sink",
                sink.provider
            ));
        }
        if !allowlisted(&self.policy.allowed_regions, &sink.region) {
            return Err(format!(
                "provider region '{}' is not approved for this data",
                sink.region
            ));
        }
        for envelope in envelopes {
            if envelope.tenant_id != self.tenant_id {
                return Err("cross-tenant data transfer denied".into());
            }
            if envelope.classification == Classification::Secret {
                return Err("SECRET data must never be sent to a model provider".into());
            }
            if !self
                .policy
                .allowed_model_classes
                .contains(&envelope.classification)
            {
                return Err(format!(
                    "{} data is not permitted to leave for model sink {}",
                    envelope.classification,
                    sink.describe()
                ));
            }
            if envelope.classification == Classification::Restricted
                && !self.policy.allow_restricted_egress
            {
                return Err("RESTRICTED data requires an explicit policy exception".into());
            }
            if envelope.purposes.iter().any(|p| p != &self.purpose) {
                return Err("data purpose does not match the active task purpose".into());
            }
        }
        Ok(())
    }

    /// Redacts secrets in a model request and returns the redacted messages plus
    /// envelopes describing what is about to leave the process.
    pub fn prepare_model_request(
        &self,
        messages: &[Value],
    ) -> Result<(Vec<Value>, Vec<DataEnvelope>), String> {
        let mut redacted = Vec::with_capacity(messages.len());
        let mut envelopes = Vec::new();
        for (index, message) in messages.iter().enumerate() {
            let role = message
                .get("role")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            let source = Provenance {
                kind: "model-context".into(),
                resource: format!("messages[{index}]:{role}"),
                revision: None,
            };
            let original = json_strings(message);
            let classified = self.classify(&source, &original);
            if classified.secrets_found > 0 && !self.policy.redact_secrets {
                return Err("secret detected in model context and redaction is disabled".into());
            }
            let mut clone = message.clone();
            if classified.secrets_found > 0 {
                if redact_json(&mut clone) == 0 {
                    return Err("secret redaction failed closed".into());
                }
                // Verify independently that no secret pattern remains.
                let text = json_strings(&clone);
                if secret_rules()
                    .iter()
                    .any(|rule| rule.pattern.is_match(&text))
                {
                    return Err("secret redaction failed closed".into());
                }
            }
            envelopes.push(self.envelope(source, &classified));
            redacted.push(clone);
        }
        Ok((redacted, envelopes))
    }

    pub fn inspect_model_output(&self, content: &str, resource: &str) -> Result<String, String> {
        let source = Provenance {
            kind: "model-output".into(),
            resource: resource.into(),
            revision: None,
        };
        let classified = self.classify(&source, content);
        if classified.secrets_found > 0 && !self.policy.redact_secrets {
            return Err("model output contains secrets and redaction is disabled".into());
        }
        if self.policy.inspect_outputs && classified.effective >= Classification::Restricted {
            return Err(format!(
                "model output classified {} exceeds the output inspection policy",
                classified.effective
            ));
        }
        Ok(classified.content)
    }

    pub fn classify_tool_output(
        &self,
        source_kind: &str,
        resource: &str,
        content: &str,
    ) -> Classified {
        let source = Provenance {
            kind: source_kind.into(),
            resource: resource.into(),
            revision: None,
        };
        self.classify(&source, content)
    }
}

/// Flatten every string value so secret and PII patterns see decoded text
/// instead of JSON-escaped text, which can hide quoted values.
pub fn json_strings(value: &Value) -> String {
    fn walk(value: &Value, out: &mut String) {
        match value {
            Value::String(text) => {
                out.push_str(text);
                out.push('\n');
            }
            Value::Array(items) => {
                for item in items {
                    walk(item, out);
                }
            }
            Value::Object(map) => {
                for item in map.values() {
                    walk(item, out);
                }
            }
            _ => {}
        }
    }
    let mut out = String::new();
    walk(value, &mut out);
    out
}

pub fn redact_json(value: &mut Value) -> usize {
    match value {
        Value::String(text) => {
            let (redacted, count) = redact_text(text);
            *text = redacted;
            count
        }
        Value::Array(items) => items.iter_mut().map(redact_json).sum(),
        Value::Object(map) => map.values_mut().map(redact_json).sum(),
        _ => 0,
    }
}

pub fn redact_text(text: &str) -> (String, usize) {
    let mut out = text.to_string();
    let mut count = 0usize;
    for rule in secret_rules() {
        let matches = rule.pattern.find_iter(&out).count();
        if matches > 0 {
            count += matches;
            out = rule
                .pattern
                .replace_all(&out, rule.replacement)
                .into_owned();
        }
    }
    (out, count)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn governance(classes: Vec<Classification>) -> Governance {
        Governance::new(
            DataPolicySpec {
                source_default: Classification::Internal,
                allowed_model_classes: classes,
                allowed_purposes: vec!["SOFTWARE_DEV".into()],
                redact_secrets: true,
                ..Default::default()
            },
            "tenant-a",
            "human-1",
            "SOFTWARE_DEV",
        )
    }

    fn source() -> Provenance {
        Provenance {
            kind: "repo".into(),
            resource: "repo:src/main.rs".into(),
            revision: None,
        }
    }

    #[test]
    fn secrets_are_detected_and_redacted() {
        let g = governance(vec![Classification::Internal]);
        let content = "const KEY: &str = \"sk-abcdefghijklmnopqrstuvwxyz123456\";";
        let classified = g.classify(&source(), content);
        assert!(classified.secrets_found > 0);
        assert!(!classified
            .content
            .contains("sk-abcdefghijklmnopqrstuvwxyz123456"));
        assert_eq!(classified.original, Classification::Secret);
        assert_eq!(classified.effective, Classification::Confidential);
    }

    #[test]
    fn secret_never_leaves_even_when_redaction_is_off() {
        let g = Governance::new(
            DataPolicySpec {
                source_default: Classification::Internal,
                allowed_model_classes: vec![
                    Classification::Public,
                    Classification::Internal,
                    Classification::Confidential,
                    Classification::Restricted,
                ],
                redact_secrets: false,
                allow_restricted_egress: true,
                ..Default::default()
            },
            "tenant-a",
            "human-1",
            "SOFTWARE_DEV",
        );
        let classified = g.classify(&source(), "password = 'correct-horse-battery'");
        assert_eq!(classified.original, Classification::Secret);
        let envelope = g.envelope(source(), &classified);
        let sink = ModelSink {
            provider: "approved".into(),
            model: "m".into(),
            provider_class: "enterprise".into(),
            region: "local".into(),
        };
        assert!(g.authorize_model_egress(&[envelope], &sink).is_err());
    }

    #[test]
    fn egress_matrix_denies_unapproved_classes() {
        let g = governance(vec![Classification::Public, Classification::Internal]);
        let classified = g.classify(&source(), "contact alice@example.com about the release");
        assert_eq!(classified.effective, Classification::Confidential);
        let envelope = g.envelope(source(), &classified);
        let sink = ModelSink {
            provider: "approved".into(),
            model: "m".into(),
            provider_class: "enterprise".into(),
            region: "local".into(),
        };
        assert!(g.authorize_model_egress(&[envelope], &sink).is_err());
    }

    #[test]
    fn egress_allowlists_fail_closed_and_accept_only_explicit_wildcards() {
        let classified =
            governance(vec![Classification::Internal]).classify(&source(), "release notes");
        let sink = |provider: &str, region: &str| ModelSink {
            provider: provider.into(),
            model: "m".into(),
            provider_class: "enterprise".into(),
            region: region.into(),
        };

        let omitted = governance(vec![Classification::Internal]);
        let envelope = omitted.envelope(source(), &classified);
        assert!(omitted
            .authorize_model_egress(std::slice::from_ref(&envelope), &sink("approved", "eu"))
            .is_err());

        let wildcard = Governance::new(
            DataPolicySpec {
                allowed_model_classes: vec![Classification::Internal],
                allowed_providers: vec!["*".into()],
                allowed_regions: vec!["*".into()],
                ..Default::default()
            },
            "tenant-a",
            "human-1",
            "SOFTWARE_DEV",
        );
        assert!(wildcard
            .authorize_model_egress(std::slice::from_ref(&envelope), &sink("any", "any"))
            .is_ok());

        let scoped = Governance::new(
            DataPolicySpec {
                allowed_model_classes: vec![Classification::Internal],
                allowed_providers: vec!["approved".into()],
                allowed_regions: vec!["eu".into()],
                ..Default::default()
            },
            "tenant-a",
            "human-1",
            "SOFTWARE_DEV",
        );
        assert!(scoped
            .authorize_model_egress(std::slice::from_ref(&envelope), &sink("approved", "eu"))
            .is_ok());
        assert!(scoped
            .authorize_model_egress(std::slice::from_ref(&envelope), &sink("approved", "us"))
            .is_err());
        assert!(scoped
            .authorize_model_egress(&[envelope], &sink("other", "eu"))
            .is_err());
    }

    #[test]
    fn model_request_redaction_round_trips() {
        let g = governance(vec![Classification::Confidential]);
        let messages = vec![
            serde_json::json!({"role":"system","content":"You are Jani."}),
            serde_json::json!({"role":"user","content":"token=abcdefghijklmnop"}),
        ];
        let (redacted, envelopes) = g.prepare_model_request(&messages).unwrap();
        assert_eq!(envelopes.len(), 2);
        let text = serde_json::to_string(&redacted).unwrap();
        assert!(!text.contains("abcdefghijklmnop"));
    }

    #[test]
    fn output_inspection_fails_closed_for_restricted_content() {
        let strict = governance(vec![Classification::Restricted]);
        assert!(strict
            .inspect_model_output("card 4111 1111 1111 1111", "assistant-message")
            .is_err());

        let permissive = Governance::new(
            DataPolicySpec {
                allowed_model_classes: vec![Classification::Restricted],
                inspect_outputs: false,
                ..Default::default()
            },
            "tenant-a",
            "human-1",
            "SOFTWARE_DEV",
        );
        assert!(permissive
            .inspect_model_output("card 4111 1111 1111 1111", "assistant-message")
            .is_ok());
    }

    #[test]
    fn luhn_detects_valid_cards_only() {
        assert!(luhn_valid("4111111111111111"));
        assert!(!luhn_valid("4111111111111112"));
        assert!(contains_card("card 4111 1111 1111 1111"));
    }
}
