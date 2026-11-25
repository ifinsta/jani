use crate::policy::DataPolicySpec;
use crate::types::{new_id, sha256_hex, Classification, DataEnvelope, Provenance};
use regex::Regex;
use serde_json::Value;
use std::sync::OnceLock;

// Stream withholding and redaction must recognize the same PEM markers.
pub const PEM_BEGIN: &str = "-----BEGIN ";
pub const PEM_END: &str = "-----END ";
pub const PEM_PRIVATE_KEY_TAIL: &str = "PRIVATE KEY-----";

struct SecretRule {
    pattern: Regex,
    replacement: &'static str,
}

fn secret_rules() -> &'static Vec<SecretRule> {
    static RULES: OnceLock<Vec<SecretRule>> = OnceLock::new();
    RULES.get_or_init(|| {
        let specs: &[(&str, &str)] = &[
            (r"\b(?:AKIA|ASIA)[0-9A-Z]{16}\b", "[REDACTED:AWS_KEY]"),
            (
                r"\b(?:gh[pousr]_[A-Za-z0-9]{20,}|github_pat_[A-Za-z0-9_]{20,})\b",
                "[REDACTED:GITHUB_TOKEN]",
            ),
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
                r"(?i)\bauthorization\b\s*[:=]\s*bearer\s+[^\s,;\]\}]{8,}",
                "authorization=[REDACTED]",
            ),
            (
                r#"(?i)["']?\b([A-Za-z0-9_-]*(?:password|passwd|pwd|secret|api[_-]?key|(?:access[_-]?|auth[_-]?|refresh[_-]?)?token))\b["']?\s*[:=]\s*(?:"(?:\\[\s\S]|[^"\\])*(?:"|\\?$)|'(?:\\[\s\S]|''|[^'\\])*(?:'|\\?$)|[^\s"'\[\]]+)"#,
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

fn is_code_type_annotation(matched: &str) -> bool {
    let Some((lhs, rhs)) = matched.split_once(':') else {
        return false;
    };
    if lhs.contains('=') || lhs.trim_end().ends_with(['"', '\'']) {
        return false;
    }
    let rhs = rhs.trim().trim_end_matches([',', ';', ')', '}', ']', '>']);
    matches!(
        rhs,
        "bool"
            | "boolean"
            | "str"
            | "String"
            | "usize"
            | "isize"
            | "u8"
            | "u16"
            | "u32"
            | "u64"
            | "u128"
            | "i8"
            | "i16"
            | "i32"
            | "i64"
            | "i128"
            | "f32"
            | "f64"
            | "number"
            | "string"
            | "object"
            | "void"
            | "never"
            | "unknown"
            | "any"
            | "null"
            | "undefined"
            | "Option"
            | "Result"
            | "Vec"
    )
}

fn redact_private_keys(text: &str) -> (String, usize) {
    static HEADER: OnceLock<Regex> = OnceLock::new();
    let header = HEADER.get_or_init(|| {
        Regex::new(&format!(
            r"{}([A-Z0-9 ]*{})",
            regex::escape(PEM_BEGIN),
            PEM_PRIVATE_KEY_TAIL
        ))
        .expect("valid private key header regex")
    });
    let mut out = String::new();
    let mut cursor = 0;
    let mut count = 0;
    for captures in header.captures_iter(text) {
        let begin = captures.get(0).unwrap();
        if begin.start() < cursor {
            continue;
        }
        let end = format!("{}{}", PEM_END, &captures[1]);
        let Some(close) = text[begin.end()..].find(&end) else {
            out.push_str(&text[cursor..begin.start()]);
            out.push_str("[REDACTED:PRIVATE_KEY]");
            cursor = text.len();
            count += 1;
            break;
        };
        out.push_str(&text[cursor..begin.start()]);
        out.push_str("[REDACTED:PRIVATE_KEY]");
        cursor = begin.end() + close + end.len();
        count += 1;
    }
    out.push_str(&text[cursor..]);
    (out, count)
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

fn looks_like_unix_timestamp(candidate: &str) -> bool {
    let (minimum, maximum) = match candidate.len() {
        13 => (946_684_800_000, 4_102_444_800_000),
        16 => (946_684_800_000_000, 4_102_444_800_000_000),
        19 => (946_684_800_000_000_000, 4_102_444_800_000_000_000),
        _ => return false,
    };
    candidate
        .parse::<u64>()
        .is_ok_and(|value| (minimum..=maximum).contains(&value))
}

fn looks_like_card_iin(digits: &str) -> bool {
    let len = digits.len();
    // Major network IIN prefixes + lengths (Visa, Mastercard, Amex, Discover, Diners, JCB, UnionPay).
    let bytes = digits.as_bytes();
    if bytes[0] == b'4' && matches!(len, 13 | 16 | 19) {
        return true;
    }
    if len == 15 && (digits.starts_with("34") || digits.starts_with("37")) {
        return true;
    }
    if len == 16 {
        if let Ok(p2) = digits[..2].parse::<u32>() {
            if (51..=55).contains(&p2) || p2 == 65 {
                return true;
            }
        }
        if let Ok(p4) = digits[..4].parse::<u32>() {
            if (2221..=2720).contains(&p4) || p4 == 6011 || (3528..=3589).contains(&p4) {
                return true;
            }
        }
        if let Ok(p3) = digits[..3].parse::<u32>() {
            if (644..=649).contains(&p3) {
                return true;
            }
        }
    }
    if len == 14
        && (digits.starts_with("36")
            || digits.starts_with("38")
            || (digits.starts_with("30") && matches!(bytes[2], b'0'..=b'5')))
    {
        return true;
    }
    digits.starts_with("62") && (16..=19).contains(&len)
}

fn contains_card(text: &str) -> bool {
    card_regex().find_iter(text).any(|m| {
        let raw = m.as_str();
        let digits: String = raw.chars().filter(|c| c.is_ascii_digit()).collect();
        let has_separators = raw.contains(' ') || raw.contains('-');
        (13..=19).contains(&digits.len())
            && luhn_valid(&digits)
            && (has_separators || looks_like_card_iin(&digits))
            // Bare epoch milliseconds, microseconds and nanoseconds overlap card
            // lengths; their 2000..2100 range is not evidence of payment data.
            && !(raw.bytes().all(|b| b.is_ascii_digit())
                && looks_like_unix_timestamp(&digits))
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

#[derive(Clone, Debug)]
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
        let (mut text, mut secrets_found) = redact_private_keys(content);
        let redact_secrets = self.policy.redact_secrets;
        if !redact_secrets {
            text = content.to_string();
        }

        for rule in secret_rules() {
            let matches = rule
                .pattern
                .find_iter(&text)
                .filter(|m| !is_code_type_annotation(m.as_str()))
                .count();
            if matches > 0 {
                secrets_found += matches;
                if redact_secrets {
                    text = rule
                        .pattern
                        .replace_all(&text, |caps: &regex::Captures<'_>| {
                            let full = caps.get(0).map_or("", |m| m.as_str());
                            if is_code_type_annotation(full) {
                                full.to_string()
                            } else if let Some(key) = caps.get(1) {
                                format!("{}=[REDACTED]", key.as_str())
                            } else {
                                rule.replacement.to_string()
                            }
                        })
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
        let is_ssn = ssn_regex().is_match(content);
        let is_card = contains_card(content);
        if is_ssn || is_card {
            if !tags.iter().any(|t| t == "PII") {
                tags.push("PII".into());
            }
            tags.push("REGULATED".into());
            classification = classification.max(Classification::Restricted);
            if is_card && !tags.iter().any(|t| t == "PCI") {
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
                if redact_text(&text).1 > 0 {
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

/// Flatten every string value and object key—recursing into JSON-encoded
/// string payloads—so secret and PII patterns see decoded text instead of
/// escaped JSON, which can hide quoted values.
pub fn json_strings(value: &Value) -> String {
    fn walk(value: &Value, out: &mut String, depth: usize) {
        match value {
            Value::String(text) => {
                out.push_str(text);
                out.push('\n');
                if depth < 4 && (text.starts_with('{') || text.starts_with('[')) {
                    if let Ok(nested) = serde_json::from_str::<Value>(text) {
                        walk(&nested, out, depth + 1);
                    }
                }
            }
            Value::Array(items) => {
                for item in items {
                    walk(item, out, depth);
                }
            }
            Value::Object(map) => {
                for (key, item) in map {
                    out.push_str(key);
                    out.push('\n');
                    walk(item, out, depth);
                }
            }
            _ => {}
        }
    }
    let mut out = String::new();
    walk(value, &mut out, 0);
    out
}

pub fn redact_json(value: &mut Value) -> usize {
    fn walk_mut(value: &mut Value, depth: usize) -> usize {
        match value {
            Value::String(text) => {
                let mut total = 0usize;
                if depth < 4 && (text.starts_with('{') || text.starts_with('[')) {
                    if let Ok(mut nested) = serde_json::from_str::<Value>(text) {
                        let inner_count = walk_mut(&mut nested, depth + 1);
                        if inner_count > 0 {
                            total += inner_count;
                            if let Ok(serialized) = serde_json::to_string(&nested) {
                                *text = serialized;
                            }
                        }
                    }
                }
                let (redacted, count) = redact_text(text);
                *text = redacted;
                total + count
            }
            Value::Array(items) => items.iter_mut().map(|item| walk_mut(item, depth)).sum(),
            Value::Object(map) => {
                let mut count = 0usize;
                let mut rebuilt = serde_json::Map::with_capacity(map.len());
                let old = std::mem::take(map);
                for (key, mut val) in old {
                    let (redacted_key, key_count) = redact_text(&key);
                    count += key_count;
                    count += walk_mut(&mut val, depth);
                    rebuilt.insert(redacted_key, val);
                }
                *map = rebuilt;
                count
            }
            _ => 0,
        }
    }
    walk_mut(value, 0)
}

pub fn redact_text(text: &str) -> (String, usize) {
    let (mut out, mut count) = redact_private_keys(text);
    for rule in secret_rules() {
        let matches = rule
            .pattern
            .find_iter(&out)
            .filter(|m| !is_code_type_annotation(m.as_str()))
            .count();
        if matches > 0 {
            count += matches;
            out = rule
                .pattern
                .replace_all(&out, |caps: &regex::Captures<'_>| {
                    let full = caps.get(0).map_or("", |m| m.as_str());
                    if is_code_type_annotation(full) {
                        full.to_string()
                    } else if let Some(key) = caps.get(1) {
                        format!("{}=[REDACTED]", key.as_str())
                    } else {
                        rule.replacement.to_string()
                    }
                })
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
    fn secret_assignments_redact_complete_quoted_and_unquoted_values() {
        let cases = [
            (r#"{"password":"x"}"#, "{password=[REDACTED]}"),
            (r#"{"token":"short"}"#, "{token=[REDACTED]}"),
            (
                r#"{"apiKey":"say \"hello\" then leave"}"#,
                "{apiKey=[REDACTED]}",
            ),
            (
                r#"{"password":"correct horse battery", "token":"a b", "apiKey":"abc"}"#,
                "{password=[REDACTED], token=[REDACTED], apiKey=[REDACTED]}",
            ),
            (
                r#""password": "correct horse battery""#,
                "password=[REDACTED]",
            ),
            ("'api-key': 'can''t share this'", "api-key=[REDACTED]"),
            (r"secret = 'say \'hello\' now'", "secret=[REDACTED]"),
            ("passwd = correct-horse-battery", "passwd=[REDACTED]"),
            ("pwd=x", "pwd=[REDACTED]"),
            (r#"{"password":"supersecret"#, "{password=[REDACTED]"),
            (r#"password="supersecret\"#, "password=[REDACTED]"),
            ("password=\"first\nsecond\"", "password=[REDACTED]"),
            ("password=\"first\n", "password=[REDACTED]"),
            ("access_token: abc", "access_token=[REDACTED]"),
            ("api_key=abcdefghi", "api_key=[REDACTED]"),
            ("password=abc,def;ghi{jkl}mno", "password=[REDACTED]"),
        ];
        let g = governance(vec![Classification::Confidential]);
        for (input, expected) in cases {
            let (redacted, count) = redact_text(input);
            assert_eq!(redacted, expected);
            assert!(count > 0);
            assert_eq!(redact_text(&redacted), (redacted.clone(), 0));
            let classified = g.classify(&source(), input);
            assert_eq!(classified.content, expected);
            assert_eq!(classified.original, Classification::Secret);
            assert_eq!(classified.effective, Classification::Confidential);
            assert_eq!(g.inspect_model_output(input, "test").unwrap(), expected);
        }
    }

    #[test]
    fn json_secret_assignments_are_redacted_in_model_requests() {
        let g = governance(vec![Classification::Confidential]);
        let content = r#"{"password":"tiny", "token":"a b", "apiKey":"say \"hello\" now"}"#;
        let messages = vec![serde_json::json!({"role": "user", "content": content})];
        let (redacted, envelopes) = g.prepare_model_request(&messages).unwrap();
        assert_eq!(
            redacted[0]["content"],
            "{password=[REDACTED], token=[REDACTED], apiKey=[REDACTED]}"
        );
        assert!(envelopes[0].redacted);
        assert_eq!(envelopes[0].classification, Classification::Confidential);

        let mut no_redaction = governance(vec![Classification::Confidential]);
        no_redaction.policy.redact_secrets = false;
        assert!(no_redaction.prepare_model_request(&messages).is_err());
        assert!(no_redaction.inspect_model_output(content, "test").is_err());
    }

    #[test]
    fn private_key_redaction_requires_the_corresponding_end_label() {
        let incomplete = concat!(
            "before\n-----BEGIN RSA PRIVATE KEY-----\nouter-private\n",
            "-----BEGIN PUBLIC KEY-----\npublic\n-----END PUBLIC KEY-----\n",
            "-----BEGIN EC PRIVATE KEY-----\ninner-private\n-----END EC PRIVATE KEY-----\n",
            "outer-private-tail\n"
        );
        assert_eq!(
            redact_text(incomplete),
            ("before\n[REDACTED:PRIVATE_KEY]".into(), 1)
        );
        let g = governance(vec![Classification::Confidential]);
        assert_eq!(
            g.inspect_model_output(incomplete, "test").unwrap(),
            "before\n[REDACTED:PRIVATE_KEY]"
        );
        let (request, _) = g
            .prepare_model_request(&[serde_json::json!({"role":"user", "content":incomplete})])
            .unwrap();
        assert_eq!(request[0]["content"], "before\n[REDACTED:PRIVATE_KEY]");
        let complete = format!("{incomplete}-----END RSA PRIVATE KEY-----\nafter");
        let expected = "before\n[REDACTED:PRIVATE_KEY]\nafter";
        assert_eq!(redact_text(&complete), (expected.into(), 1));
        let g = governance(vec![Classification::Confidential]);
        assert_eq!(g.inspect_model_output(&complete, "test").unwrap(), expected);
        let classified = g.classify(&source(), &complete);
        assert_eq!(classified.secrets_found, 1);
        assert_eq!(classified.content, expected);
        let messages = vec![serde_json::json!({"role": "user", "content": complete})];
        let (redacted, _) = g.prepare_model_request(&messages).unwrap();
        assert_eq!(redacted[0]["content"], expected);

        let mut no_redaction = governance(vec![Classification::Confidential]);
        no_redaction.policy.redact_secrets = false;
        assert!(no_redaction
            .inspect_model_output(&complete, "test")
            .is_err());
    }

    #[test]
    fn private_key_redaction_preserves_public_blocks_and_ordinary_output() {
        let public = "-----BEGIN PUBLIC KEY-----\npublic\n-----END PUBLIC KEY-----\n";
        let input = format!(
            "before\n-----BEGIN PRIVATE KEY-----\nfirst-private\n-----END PRIVATE KEY-----\n{public}\
             -----BEGIN ENCRYPTED PRIVATE KEY-----\nsecond-private\n-----END ENCRYPTED PRIVATE KEY-----\nafter"
        );
        let expected =
            format!("before\n[REDACTED:PRIVATE_KEY]\n{public}[REDACTED:PRIVATE_KEY]\nafter");
        assert_eq!(redact_text(&input), (expected.clone(), 2));
        let g = governance(vec![Classification::Confidential]);
        assert_eq!(g.inspect_model_output(&input, "test").unwrap(), expected);
        let ordinary = format!("ordinary output with passwordless login\n{public}done");
        assert_eq!(redact_text(&ordinary), (ordinary.clone(), 0));
        assert_eq!(g.inspect_model_output(&ordinary, "test").unwrap(), ordinary);
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
        assert!(contains_card("4111111111111111"));
    }

    #[test]
    fn epoch_timestamps_do_not_trigger_card_classification() {
        fn complete_luhn(prefix: &str) -> String {
            let mut sum = 0u32;
            let mut double = true;
            for c in prefix.chars().rev() {
                let mut value = c.to_digit(10).unwrap();
                if double {
                    value *= 2;
                    if value > 9 {
                        value -= 9;
                    }
                }
                sum += value;
                double = !double;
            }
            format!("{prefix}{}", (10 - sum % 10) % 10)
        }

        let g = governance(vec![Classification::Internal]);
        for prefix in ["176722560000", "176722560000000", "176722560000000000"] {
            let timestamp = complete_luhn(prefix);
            assert!(luhn_valid(&timestamp));
            assert!(!contains_card(&timestamp));
            let classified = g.classify(&source(), &format!("timestamp={timestamp}"));
            assert_eq!(classified.effective, Classification::Internal);
            assert!(!classified.tags.iter().any(|tag| tag == "PCI"));
        }
        let classified = g.classify(&source(), "card=4111111111111111");
        assert_eq!(classified.effective, Classification::Restricted);
        assert!(classified.tags.iter().any(|tag| tag == "PCI"));
    }

    #[test]
    fn skips_code_types_and_redacts_nested_json_and_keys() {
        let g = governance(vec![Classification::Internal]);
        let code = g.classify(&source(), "fn f(token: usize, secret: bool) {}");
        assert!(!code.tags.iter().any(|tag| tag == "SECRET_DETECTED"));

        let mut nested = serde_json::json!({
            "payload": "{\"aws\":\"ASIA1234567890ABCDEF\",\"gh\":\"github_pat_1234567890abcdefghij\"}",
            "db_password=supersecretval": "ok"
        });
        let found = g.classify(&source(), &json_strings(&nested));
        assert!(found.tags.iter().any(|tag| tag == "SECRET_DETECTED"));
        let count = redact_json(&mut nested);
        assert!(count >= 3);
        let serialized = nested.to_string();
        assert!(!serialized.contains("ASIA1234567890ABCDEF"));
        assert!(!serialized.contains("github_pat_1234567890abcdefghij"));
        assert!(!serialized.contains("supersecretval"));
    }
}
