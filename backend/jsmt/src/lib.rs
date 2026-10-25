//! Jani Secure Machine Transport JSMT/1 shared contract.
//! Experimental open protocol. There is no remote execution message type.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

pub const VERSION: &str = "jani-jsmt/1";
pub const MAX_BODY: usize = 262_144;
pub const MAX_METRICS: usize = 256;
pub const MAX_LOGS: usize = 200;
pub const MAX_COVERAGE: usize = 256;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Envelope {
    pub version: String,
    pub tenant_id: String,
    pub device_id: String,
    pub sequence: i64,
    pub observed_at: String,
    pub metrics: Vec<Metric>,
    pub logs: Vec<Log>,
    pub coverage: Vec<Coverage>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Metric {
    pub name: String,
    pub unit: String,
    pub value: f64,
    pub attributes: BTreeMap<String, String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Log {
    pub time_unix_nano: i64,
    pub severity_text: String,
    pub body: String,
    pub resource: BTreeMap<String, String>,
    pub attributes: BTreeMap<String, String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Coverage {
    pub service: String,
    pub source: String,
    pub status: String,
    pub reason: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Ack {
    pub version: String,
    pub ack_sequence: i64,
    pub payload_sha256: String,
    pub duplicate: bool,
}
pub fn identity_ok(v: &str) -> bool {
    !v.is_empty() && v.len() <= 96 && v.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
}
pub fn metric_unit(name: &str) -> Option<&'static str> {
    Some(match name {
        "system.uptime" | "system.cpu.time" => "s",
        "system.memory.usage" | "jani.filesystem.used" => "By",
        "system.process.count" => "{process}",
        "jani.service.up" | "jani.service.log.covered" => "1",
        _ => return None,
    })
}
pub fn canonical(value: &Value) -> Result<Vec<u8>, String> {
    // serde_json Value maps are lexically ordered when preserve_order is disabled.
    // JSMT/1: Python json.dumps(sort_keys=True,separators=(',',':'),ensure_ascii=False).
    // Numeric formats outside the official interop vectors are not guaranteed
    // equivalent across languages; see SPEC.md for protocol v2 remediation.
    serde_json::to_vec(value).map_err(|e| e.to_string())
}
pub fn digest(body: &[u8]) -> String {
    hex::encode(Sha256::digest(body))
}
pub fn validate(body: &[u8]) -> Result<(Envelope, Vec<u8>, String), String> {
    if body.is_empty() || body.len() > MAX_BODY {
        return Err("payload_bounds".into());
    }
    let raw: Value = serde_json::from_slice(body).map_err(|_| "invalid_json")?;
    let env: Envelope = serde_json::from_value(raw.clone()).map_err(|_| "invalid_envelope")?;
    if env.version != VERSION || !identity_ok(&env.tenant_id) || !identity_ok(&env.device_id)
        || env.sequence < 1 || env.observed_at.is_empty() || env.observed_at.len() > 64
        || env.metrics.len() > MAX_METRICS || env.logs.len() > MAX_LOGS
        || env.coverage.len() > MAX_COVERAGE
    {
        return Err("invalid_envelope".into());
    }
    for metric in &env.metrics {
        if metric_unit(&metric.name) != Some(metric.unit.as_str())
            || !metric.value.is_finite() || !labels_ok(&metric.attributes) {
            return Err("invalid_metric".into());
        }
    }
    for log in &env.logs {
        if log.time_unix_nano < 0 || !matches!(log.severity_text.as_str(), "TRACE" | "DEBUG" | "INFO" | "WARN" | "ERROR" | "FATAL")
            || log.body.chars().count() > 4096 || !redacted(&log.body)
            || log.resource.len() != 1
            || !log.resource.get("service.name").is_some_and(|s| !s.is_empty() && s.len() <= 100
                && s.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_' | b'@')))
            || !labels_ok(&log.attributes)
        {
            return Err("invalid_log".into());
        }
    }
    for item in &env.coverage {
        if !matches!(item.status.as_str(), "covered" | "partial" | "missing" | "permission_denied" | "unsupported")
            || item.service.len() > 160 || item.service.is_empty() || item.source.len() > 160
            || item.reason.len() > 160 {
            return Err("invalid_coverage".into());
        }
    }
    let serialized = canonical(&raw)?;
    if serialized.len() > MAX_BODY {
        return Err("payload_bounds".into());
    }
    let hash = digest(&serialized);
    Ok((env, serialized, hash))
}
pub fn labels_ok(v: &BTreeMap<String, String>) -> bool {
    v.len() <= 8 && v.iter().all(|(k, val)| !k.is_empty() && k.len() <= 100 && val.len() <= 100)
}
pub fn redacted(v: &str) -> bool {
    let lower = v.to_ascii_lowercase();
    // Defense-in-depth only: use full privacy filtering before ingesting real logs.
    for marker in ["password=", "password:", "token=", "token:", "secret=", "secret:",
        "api_key=", "apikey=", "authorization: bearer ", "authorization=bearer "] {
        let mut remaining = lower.as_str();
        while let Some(at) = remaining.find(marker) {
            let following = &remaining[at + marker.len()..];
            if !following.trim_start().starts_with("[redacted]") { return false; }
            remaining = &following[following.find("[redacted]").unwrap_or(0) + "[redacted]".len()..];
        }
    }
    true
}
pub fn create_envelope(tenant: &str, device: &str, sequence: i64, observed_at: String,
    metrics: Vec<Metric>, logs: Vec<Log>, coverage: Vec<Coverage>) -> Result<(Vec<u8>, String), String> {
    let v = serde_json::to_value(Envelope {
        version: VERSION.into(), tenant_id: tenant.into(), device_id: device.into(),
        sequence, observed_at, metrics, logs, coverage,
    }).map_err(|e| e.to_string())?;
    let bytes = canonical(&v)?;
    let (_, canonical_bytes, hash) = validate(&bytes)?;
    Ok((canonical_bytes, hash))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reject_unknown_fields() {
        let raw = br#"{"version":"jani-jsmt/1","tenant_id":"t","device_id":"d","sequence":1,"observed_at":"now","metrics":[],"logs":[],"coverage":[],"execute":"sh"}"#;
        assert!(validate(raw).is_err());
    }
    #[test]
    fn reject_secret_and_invalid_unit() {
        let mut log = Log { time_unix_nano: 1, severity_text: "ERROR".into(), body: "password=hi".into(),
            resource: BTreeMap::from([("service.name".into(), "api".into())]), attributes: BTreeMap::new() };
        let metric = Metric { name: "system.uptime".into(), unit: "By".into(), value: 5.0, attributes: BTreeMap::new() };
        assert!(create_envelope("t","d",1,"now".into(),vec![metric],vec![],vec![]).is_err());
        assert!(create_envelope("t","d",1,"now".into(),vec![],vec![log.clone()],vec![]).is_err());
        log.body = "redacted".into();
        assert!(create_envelope("t","d",1,"now".into(),vec![],vec![log],vec![]).is_ok());
    }
    #[test]
    fn interop_vector_matches_python_reference() {
        let v = include_bytes!("../../../protocol/jsmt/v1/vectors/basic.json");
        let (_, _, hash) = validate(v).unwrap();
        assert_eq!(hash, "ee12c79dc0c57a15ca58bdb663ff2c7d1c1bd72acef2cee173a609d6de0bf405");
    }
    #[test]
    fn digest_is_consistent_for_key_order() {
        let a = br#"{"version":"jani-jsmt/1","tenant_id":"t","device_id":"d","sequence":1,"observed_at":"now","metrics":[],"logs":[],"coverage":[]}"#;
        let b = br#"{"coverage":[],"logs":[],"metrics":[],"observed_at":"now","sequence":1,"device_id":"d","tenant_id":"t","version":"jani-jsmt/1"}"#;
        assert_eq!(validate(a).unwrap().2, validate(b).unwrap().2);
    }
}
