//! Read-only crash triage from the existing hash-chained audit journal.
//! It never launches tools, retries an operation, or claims external rollback.
use crate::journal;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Default)]
struct RunSnapshot {
    last_decision: String,
    reason: String,
    event_count: usize,
    denied: usize,
    last_timestamp: u64,
    started: bool,
}

pub fn status(state_dir: &Path) -> Result<Value, String> {
    let chain = journal::verify(state_dir)?;
    if chain["ok"] != true {
        return Err("audit integrity check failed; recovery status cannot be trusted".into());
    }
    let events = journal::read_events(&state_dir.join(journal::JOURNAL_FILE))?;
    let mut grouped: BTreeMap<(String, String, String), RunSnapshot> = BTreeMap::new();
    for event in events {
        // Only runs that have an actual agent.run record are treated as tasks.
        let key = (event.tenant_id, event.task_id, event.run_id);
        let snapshot = grouped.entry(key).or_default();
        snapshot.event_count += 1;
        snapshot.last_timestamp = snapshot.last_timestamp.max(event.timestamp);
        if event.decision == "deny" {
            snapshot.denied += 1;
        }
        if event.action == "agent.run" {
            if event.decision == "started" {
                // A reused run identifier must not inherit an earlier completion.
                snapshot.started = true;
                snapshot.last_decision.clear();
                snapshot.reason.clear();
            }
            if event.decision == "completed" || event.decision == "failed" {
                snapshot.last_decision = event.decision;
                snapshot.reason = event.reason_code;
            }
        }
    }
    let mut uncertain = 0usize;
    let mut runs = Vec::new();
    for ((tenant_id, task_id, run_id), snapshot) in grouped {
        if !snapshot.started && snapshot.last_decision.is_empty() {
            // Some audit events (model switches, detached requests) are not runs.
            continue;
        }
        let outcome = match snapshot.last_decision.as_str() {
            "completed" => "recorded_model_complete",
            "failed" => "recorded_model_failed",
            _ => {
                uncertain += 1;
                "unknown_requires_reconciliation"
            }
        };
        runs.push(json!({
            "tenantId": tenant_id,
            "taskId": task_id,
            "runId": run_id,
            "outcome": outcome,
            "reason": snapshot.reason,
            "recordedEvents": snapshot.event_count,
            "deniedDecisions": snapshot.denied,
            "lastTimestamp": snapshot.last_timestamp,
            "safeToAutoReplay": false
        }));
    }
    Ok(json!({
        "schemaVersion": "jani.recovery-status/v1",
        "journalChainVerified": true,
        "unreconciledRuns": uncertain,
        "runs": runs,
        "note": "A recorded model completion is not independent task verification. Unknown effects require authoritative reconciliation; no automatic replay is performed."
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::journal::{EventInput, Journal};
    use crate::types::Classification;
    use std::fs;

    fn root() -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "jani-recovery-{}-{}",
            std::process::id(),
            crate::types::new_id("test")
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn record(journal: &mut Journal, decision: &str) {
        journal.record(EventInput {
            tenant_id: "tenant-a",
            task_id: "task-1",
            run_id: "run-1",
            invocation_id: "run",
            actor_id: "operator",
            worker_id: "operator",
            action: "agent.run",
            resource_digest: "test",
            decision,
            reason_code: decision,
            policy_revision: "runtime",
            approval_id: None,
            sandbox_receipt_id: None,
            data_classification: Classification::Internal,
        }).unwrap();
    }

    #[test]
    fn orphaned_run_is_unknown_not_safe_to_replay() {
        let dir = root();
        let mut journal = Journal::open(&dir).unwrap();
        record(&mut journal, "started");
        let result = status(&dir).unwrap();
        assert_eq!(result["unreconciledRuns"], 1);
        assert_eq!(result["runs"][0]["outcome"], "unknown_requires_reconciliation");
        assert_eq!(result["runs"][0]["safeToAutoReplay"], false);
        drop(journal);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn completion_is_only_a_recorded_model_outcome() {
        let dir = root();
        let mut journal = Journal::open(&dir).unwrap();
        record(&mut journal, "started");
        record(&mut journal, "completed");
        let result = status(&dir).unwrap();
        assert_eq!(result["unreconciledRuns"], 0);
        assert_eq!(result["runs"][0]["outcome"], "recorded_model_complete");
        assert_eq!(result["runs"][0]["safeToAutoReplay"], false);
        drop(journal);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn journal_tampering_blocks_recovery_report() {
        use std::io::Write;
        let dir = root();
        let mut journal = Journal::open(&dir).unwrap();
        record(&mut journal, "started");
        drop(journal);
        let mut output = fs::OpenOptions::new()
            .append(true)
            .open(dir.join(journal::JOURNAL_FILE))
            .unwrap();
        writeln!(output, "{{}}").unwrap();
        assert!(status(&dir).is_err());
        fs::remove_dir_all(dir).unwrap();
    }
}
