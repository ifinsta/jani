//! Linux journald collection with durable cursor semantics and bounded subprocesses.
//! No shell execution, no privileged actions, and no unbounded pipe reads.
use jani_jsmt::{Coverage, Log};
use std::collections::BTreeMap;

pub struct Capture {
    pub logs: Vec<Log>,
    pub coverage: Coverage,
    pub cursor: Option<String>,
}
fn covered(service: &str, source: &str, status: &str, reason: &str) -> Coverage {
    Coverage {
        service: service.to_owned(),
        source: source.to_owned(),
        status: status.to_owned(),
        reason: reason.to_owned(),
    }
}
fn run(unit: &str, previous: Option<&str>) -> std::io::Result<Vec<u8>> {
    let bin = super::which(&["/usr/bin/journalctl", "/bin/journalctl"]).ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::NotFound, "journalctl unavailable")
    })?;
    let after = previous.map(|cursor| format!("--after-cursor={cursor}"));
    let mut args: Vec<&str> = vec!["--unit", unit, "--output=json", "--no-pager", "--quiet"];
    match &after {
        Some(flag) => args.push(flag.as_str()),
        None => args.push("--lines=100"),
    }
    super::run_captured(
        &bin,
        &args,
        super::SUBPROCESS_DEADLINE,
        super::MAX_CAPTURE_BYTES,
    )
}
fn normalized(
    value: &serde_json::Value,
    service: &str,
    source: &str,
) -> Result<(Log, String), String> {
    let cursor = value
        .get("__CURSOR")
        .and_then(|v| v.as_str())
        .ok_or("missing journal cursor")?;
    if cursor.is_empty() || cursor.len() > 512 || cursor.contains('\n') || cursor.contains('\r') {
        return Err("invalid journal cursor".into());
    }
    let message = value
        .get("MESSAGE")
        .and_then(|v| v.as_str())
        .ok_or("missing journal MESSAGE")?;
    let priority = value
        .get("PRIORITY")
        .and_then(|v| v.as_str())
        .unwrap_or("6");
    let severity = match priority {
        "0" | "1" => "FATAL",
        "2" | "3" => "ERROR",
        "4" => "WARN",
        "5" | "6" => "INFO",
        _ => "DEBUG",
    };
    let micros = value
        .get("__REALTIME_TIMESTAMP")
        .and_then(|v| v.as_str())
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or_else(|| super::now_nanos() / 1000);
    let timestamp = micros.saturating_mul(1000);
    let mut attrs = BTreeMap::new();
    attrs.insert("jani.log.source".into(), source.chars().take(100).collect());
    Ok((
        Log {
            time_unix_nano: timestamp.max(0),
            severity_text: severity.into(),
            body: super::scrub(message),
            resource: BTreeMap::from([("service.name".into(), service.into())]),
            attributes: attrs,
        },
        cursor.into(),
    ))
}
pub fn capture(
    service: &str,
    unit: &str,
    previous: Option<&str>,
    max_logs: usize,
) -> Result<Capture, String> {
    let source = format!("journal:{unit}");
    if max_logs == 0 {
        return Err("journal log batch full; no cursor advance".into());
    }
    if unit.is_empty()
        || unit.len() > 100
        || !unit.ends_with(".service")
        || !unit
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'-' | b'_' | b'@'))
    {
        return Err("invalid systemd unit".into());
    }
    if let Some(previous) = previous {
        if previous.len() > 512 || previous.contains('\n') || previous.contains('\r') {
            return Err("invalid stored cursor".into());
        }
    }
    let bytes = match run(unit, previous) {
        Ok(bytes) => bytes,
        Err(_) => {
            return Ok(Capture {
                logs: vec![],
                cursor: None,
                coverage: covered(
                    service,
                    &source,
                    "unsupported",
                    "journalctl inaccessible, timed out or backlog too large",
                ),
            })
        }
    };
    let raw = std::str::from_utf8(&bytes).map_err(|_| "invalid journal text")?;
    let mut logs = Vec::new();
    let mut last = None;
    for line in raw.lines() {
        if line.trim().is_empty() {
            continue;
        }
        if logs.len() >= max_logs {
            return Err("journal event limit reached; cursor unchanged".into());
        }
        let value: serde_json::Value =
            serde_json::from_str(line).map_err(|_| "invalid journal JSON")?;
        let (log, cursor) = normalized(&value, service, &source)?;
        logs.push(log);
        last = Some(cursor);
    }
    let initial = previous.is_none();
    Ok(Capture {
        logs,
        cursor: last,
        coverage: covered(
            service,
            &source,
            if initial { "partial" } else { "covered" },
            if initial {
                "initial journal tail; earlier history not verified"
            } else {
                ""
            },
        ),
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parse_event_has_severity_and_redaction() {
        let event = serde_json::json!({"__CURSOR":"s=abc","MESSAGE":"password=sensitive","PRIORITY":"3",
            "__REALTIME_TIMESTAMP":"1000000"});
        let (log, cursor) = normalized(&event, "api", "journal:api.service").unwrap();
        assert_eq!(log.severity_text, "ERROR");
        assert_eq!(cursor, "s=abc");
        assert_eq!(log.time_unix_nano, 1_000_000_000);
        assert_eq!(log.body, "password=[REDACTED]");
    }
    #[test]
    fn invalid_cursor_is_rejected() {
        let event = serde_json::json!({"__CURSOR":"one\nother","MESSAGE":"hi"});
        assert!(normalized(&event, "svc", "journal:svc.service").is_err());
    }
}
