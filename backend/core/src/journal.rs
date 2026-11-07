use crate::types::{new_id, now_unix, sha256_hex, AuditEvent, Classification};
use serde_json::{json, Value};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;

pub const JOURNAL_FILE: &str = "journal.jsonl";

/// Canonical `EventInput.decision` values. Producers across four crates write
/// these and mirror/evidence/recovery match them, so the vocabulary lives
/// beside the event record rather than being re-typed at every call site.
/// `EventInput.policy_revision` marker for records produced by the runtime
/// itself rather than evaluated against a signed policy bundle.
pub const RUNTIME_REVISION: &str = "runtime";

pub mod decision {
    pub const STARTED: &str = "started";
    pub const COMPLETED: &str = "completed";
    pub const FAILED: &str = "failed";
    pub const DENY: &str = "deny";
    pub const PERMIT: &str = "permit";
    pub const EXECUTED: &str = "executed";
    /// Internal informational events (e.g. `agent.model` profile switches)
    /// that are neither permits nor denials of a governed action.
    pub const ALLOWED: &str = "allowed";
    /// Recorded when execution pauses on an unsigned approval grant.
    pub const APPROVAL_REQUIRED: &str = "approval_required";
}

/// Canonical `EventInput.reason_code` values for codes produced inside the
/// workspace. Codes produced by the policy engine live in `policy` itself.
pub mod reason {
    pub const RUN_STARTED: &str = "run_started";
    pub const CANCELLED_BY_OPERATOR: &str = "cancelled_by_operator";
    pub const MODEL_OR_GOVERNANCE_FAILURE: &str = "model_or_governance_failure";
    pub const EMPTY_ANSWER: &str = "empty_answer";
    pub const FINAL_ANSWER: &str = "final_answer";
    pub const STEP_LIMIT: &str = "step_limit";
    pub const OK: &str = "ok";
    pub const BUDGET_EXHAUSTED: &str = "budget_exhausted";
    pub const EXECUTION_FAILED: &str = "execution_failed";
    pub const TOOL_REPORTED_ERROR: &str = "tool_reported_error";
    pub const CONTRACT_DATA_CLASSIFICATION_DENIED: &str = "contract_data_classification_denied";
    /// Recorded on `agent.model` events when the operator switches profile.
    pub const MODEL_SWITCHED: &str = "model_switched";
    /// Broker-side rejections before a call plan exists.
    pub const UNKNOWN_TOOL: &str = "unknown_tool";
    pub const INVALID_TOOL_ARGUMENTS: &str = "invalid_tool_arguments";
    pub const INVALID_TOOL_REQUEST: &str = "invalid_tool_request";
    /// Synthetic reason attached to deny-audit fallbacks when no policy code
    /// exists yet (malformed arguments, unknown tools).
    pub const INVALID_REQUEST: &str = "invalid_request";
}

pub struct Journal {
    file: File,
    previous_digest: String,
}

pub struct EventInput<'a> {
    pub tenant_id: &'a str,
    pub task_id: &'a str,
    pub run_id: &'a str,
    pub invocation_id: &'a str,
    pub actor_id: &'a str,
    pub worker_id: &'a str,
    pub action: &'a str,
    pub resource_digest: &'a str,
    pub decision: &'a str,
    pub reason_code: &'a str,
    pub policy_revision: &'a str,
    pub approval_id: Option<&'a str>,
    pub sandbox_receipt_id: Option<&'a str>,
    pub data_classification: Classification,
}

impl Journal {
    pub fn open(state_dir: &Path) -> Result<Self, String> {
        fs::create_dir_all(state_dir)
            .map_err(|e| format!("cannot create state directory {}: {e}", state_dir.display()))?;
        let path = state_dir.join(JOURNAL_FILE);
        let previous_digest = last_digest(&path)?;
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|e| format!("cannot open audit journal {}: {e}", path.display()))?;
        Ok(Self {
            file,
            previous_digest,
        })
    }

    pub fn record(&mut self, input: EventInput<'_>) -> Result<AuditEvent, String> {
        let mut event = AuditEvent {
            event_version: "1.0".into(),
            event_id: new_id("evt"),
            tenant_id: input.tenant_id.into(),
            task_id: input.task_id.into(),
            run_id: input.run_id.into(),
            invocation_id: input.invocation_id.into(),
            actor_id: input.actor_id.into(),
            worker_id: input.worker_id.into(),
            action: input.action.into(),
            resource_digest: input.resource_digest.into(),
            decision: input.decision.into(),
            reason_code: input.reason_code.into(),
            policy_revision: input.policy_revision.into(),
            approval_id: input.approval_id.map(str::to_string),
            sandbox_receipt_id: input.sandbox_receipt_id.map(str::to_string),
            data_classification: input.data_classification,
            timestamp: now_unix(),
            previous_event_digest: self.previous_digest.clone(),
            event_digest: String::new(),
        };
        event.event_digest = digest_for(&event)?;
        let line = serde_json::to_string(&event).map_err(|e| e.to_string())?;
        self.file
            .write_all(format!("{line}\n").as_bytes())
            .and_then(|_| self.file.sync_data())
            .map_err(|e| format!("cannot append audit event: {e}"))?;
        self.previous_digest = event.event_digest.clone();
        Ok(event)
    }
}

pub fn read_run(path: &Path, run_id: &str) -> Result<Vec<AuditEvent>, String> {
    let events = read_events(path)?;
    Ok(events
        .into_iter()
        .filter(|event| event.run_id == run_id)
        .collect())
}

fn event_bytes_without_digest(event: &AuditEvent) -> Result<Vec<u8>, String> {
    let mut clone = event.clone();
    clone.event_digest = String::new();
    serde_json::to_vec(&clone).map_err(|e| e.to_string())
}

fn digest_for(event: &AuditEvent) -> Result<String, String> {
    let mut bytes = event.previous_event_digest.clone().into_bytes();
    bytes.extend(event_bytes_without_digest(event)?);
    Ok(sha256_hex(&bytes))
}

fn last_digest(path: &Path) -> Result<String, String> {
    if !path.is_file() {
        return Ok(String::new());
    }
    let file = File::open(path).map_err(|e| e.to_string())?;
    let mut last = None;
    for line in BufReader::new(file).lines() {
        let line = line.map_err(|e| e.to_string())?;
        if !line.trim().is_empty() {
            last = Some(line);
        }
    }
    match last {
        None => Ok(String::new()),
        Some(line) => {
            let event: AuditEvent =
                serde_json::from_str(&line).map_err(|e| format!("journal tail is invalid: {e}"))?;
            Ok(event.event_digest)
        }
    }
}

pub fn read_events(path: &Path) -> Result<Vec<AuditEvent>, String> {
    if !path.is_file() {
        return Err(format!("journal not found: {}", path.display()));
    }
    let file = File::open(path).map_err(|e| e.to_string())?;
    let mut events = Vec::new();
    for (index, line) in BufReader::new(file).lines().enumerate() {
        let line = line.map_err(|e| e.to_string())?;
        if line.trim().is_empty() {
            continue;
        }
        let event: AuditEvent = serde_json::from_str(&line)
            .map_err(|e| format!("journal line {} is invalid: {e}", index + 1))?;
        events.push(event);
    }
    Ok(events)
}

pub fn verify(state_dir: &Path) -> Result<Value, String> {
    let path = state_dir.join(JOURNAL_FILE);
    let events = read_events(&path)?;
    let mut previous = String::new();
    for (index, event) in events.iter().enumerate() {
        if event.previous_event_digest != previous {
            return Ok(json!({
                "ok": false,
                "events": events.len(),
                "failure_line": index + 1,
                "reason": "previous_event_digest does not match the preceding event",
            }));
        }
        let expected = digest_for(event)?;
        if event.event_digest != expected {
            return Ok(json!({
                "ok": false,
                "events": events.len(),
                "failure_line": index + 1,
                "reason": "event digest mismatch",
            }));
        }
        previous = event.event_digest.clone();
    }
    Ok(json!({
        "ok": true,
        "events": events.len(),
        "last_digest": previous,
        "path": path.display().to_string(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir() -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("jani-journal-{}", crate::process::unique_id()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn input<'a>(run: &'a str, action: &'a str) -> EventInput<'a> {
        EventInput {
            tenant_id: "tenant-a",
            task_id: "task-1",
            run_id: run,
            invocation_id: "inv-1",
            actor_id: "human",
            worker_id: "worker-1",
            action,
            resource_digest: "sha256:abc",
            decision: "permit",
            reason_code: "ok",
            policy_revision: "pol-1",
            approval_id: None,
            sandbox_receipt_id: None,
            data_classification: Classification::Internal,
        }
    }

    #[test]
    fn journal_hash_chain_verifies_and_detects_tampering() {
        let dir = temp_dir();
        let mut journal = Journal::open(&dir).unwrap();
        journal.record(input("run-1", "repo.read")).unwrap();
        journal.record(input("run-1", "repo.write")).unwrap();
        drop(journal);
        assert_eq!(verify(&dir).unwrap()["ok"], true);

        let path = dir.join(JOURNAL_FILE);
        let mut lines = fs::read_to_string(&path).unwrap();
        lines = lines.replace("repo.write", "repo.read");
        fs::write(&path, lines).unwrap();
        assert_eq!(verify(&dir).unwrap()["ok"], false);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn journal_continues_the_chain_across_reopen() {
        let dir = temp_dir();
        let mut journal = Journal::open(&dir).unwrap();
        journal.record(input("run-1", "repo.read")).unwrap();
        drop(journal);
        let mut journal = Journal::open(&dir).unwrap();
        journal.record(input("run-2", "repo.read")).unwrap();
        drop(journal);
        let result = verify(&dir).unwrap();
        assert_eq!(result["ok"], true);
        assert_eq!(result["events"], 2);
        let _ = fs::remove_dir_all(dir);
    }
}
