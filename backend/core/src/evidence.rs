//! Verified, read-only evidence summary. No model output or tool payload is trusted.
use crate::journal;
use serde_json::{json, Value};
use std::path::Path;

pub fn receipt(state_dir: &Path, run_id: &str) -> Result<Value, String> {
    if run_id.is_empty() || run_id.len() > 256 || run_id.chars().any(char::is_control) {
        return Err("run ID must be 1..=256 printable bytes".into());
    }
    let integrity = journal::verify(state_dir)?;
    if integrity["ok"] != true {
        return Err("audit journal failed integrity verification".into());
    }
    let events = journal::read_run(&state_dir.join(journal::JOURNAL_FILE), run_id)?;
    if events.is_empty() {
        return Err(format!("no audited events for run '{run_id}'"));
    }
    let mut model_status = "unknown_requires_reconciliation";
    let mut denials = 0usize;
    let mut invocations = std::collections::BTreeSet::new();
    for event in &events {
        if event.decision == "deny" {
            denials += 1;
        }
        if event.invocation_id != "run" && !event.invocation_id.is_empty() {
            invocations.insert(event.invocation_id.as_str());
        }
        if event.action == "agent.run" {
            model_status = match event.decision.as_str() {
                "started" => "unknown_requires_reconciliation",
                "completed" => "recorded_model_complete",
                "failed" => "recorded_model_failed",
                _ => model_status,
            };
        }
    }
    let first = events.first().expect("nonempty verified run");
    let last = events.last().expect("nonempty verified run");
    if events
        .iter()
        .any(|event| event.tenant_id != first.tenant_id || event.task_id != first.task_id)
    {
        return Err("run ID was reused across tenant/task boundaries".into());
    }
    Ok(json!({
        "schemaVersion": "jani.run-evidence/v1",
        "tenantId": first.tenant_id,
        "taskId": first.task_id,
        "runId": run_id,
        "modelStatus": model_status,
        "journalChainVerified": true,
        "auditEventCount": events.len(),
        "decisionDenials": denials,
        "observedInvocationIds": invocations.len(),
        "lastRunEventDigest": last.event_digest,
        "outcomeVerified": false,
        "effectsReconciled": false,
        "safeToAutoReplay": false,
        "caveat": "Audit integrity and a model completion are not independent test verification, side-effect reconciliation or a signed attestation."
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::journal::{EventInput, Journal};
    use crate::types::{new_id, Classification};
    use std::fs;

    #[test]
    fn receipt_is_audit_integrity_only_not_a_claim_of_verified_work() {
        let root = std::env::temp_dir().join(format!("jani-evidence-{}", new_id("test")));
        fs::create_dir_all(&root).unwrap();
        let mut journal = Journal::open(&root).unwrap();
        for decision in ["started", "completed"] {
            journal
                .record(EventInput {
                    tenant_id: "tenant",
                    task_id: "task",
                    run_id: "run",
                    invocation_id: "run",
                    actor_id: "operator",
                    worker_id: "operator",
                    action: "agent.run",
                    resource_digest: "digest",
                    decision,
                    reason_code: "test",
                    policy_revision: "runtime",
                    approval_id: None,
                    sandbox_receipt_id: None,
                    data_classification: Classification::Internal,
                })
                .unwrap();
        }
        let proof = receipt(&root, "run").unwrap();
        assert_eq!(proof["journalChainVerified"], true);
        assert_eq!(proof["modelStatus"], "recorded_model_complete");
        assert_eq!(proof["outcomeVerified"], false);
        assert_eq!(proof["effectsReconciled"], false);
        assert_eq!(proof["safeToAutoReplay"], false);
        drop(journal);
        fs::remove_dir_all(root).unwrap();
    }
}
