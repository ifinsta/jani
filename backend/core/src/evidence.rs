//! Verified, read-only evidence summary. No model output or tool payload is trusted.
use crate::journal;
use crate::types::{self, AuditEvent, VerificationReceipt};
use serde_json::{json, Value};
use std::path::Path;

pub fn receipt(state_dir: &Path, run_id: &str) -> Result<Value, String> {
    receipt_with_verifier_keys(state_dir, run_id, &[], &[])
}

pub fn receipt_with_verifier_keys(
    state_dir: &Path,
    run_id: &str,
    verifier_keys: &[String],
    revoked_keys: &[String],
) -> Result<Value, String> {
    let events = journal::read_verified_events(&state_dir.join(journal::JOURNAL_FILE))?;
    let mut out = receipt_from_verified_events(&events, run_id)?;
    let Some(task_id) = out.get("taskId").and_then(Value::as_str).map(str::to_owned) else {
        return Ok(out);
    };
    let Some(tenant_id) = out
        .get("tenantId")
        .and_then(Value::as_str)
        .map(str::to_owned)
    else {
        return Ok(out);
    };
    let Ok(vpath) = types::verification_receipt_path(state_dir, &task_id) else {
        return Ok(out);
    };
    if !vpath.is_file() {
        return Ok(out);
    }
    let Some(map) = out.as_object_mut() else {
        return Ok(out);
    };
    match VerificationReceipt::load(&vpath) {
        Ok(vr) if vr.tenant_id == tenant_id && vr.task_id == task_id => {
            map.insert("verificationPresent".into(), json!(true));
            map.insert("verificationPassed".into(), json!(vr.passed));
            map.insert("verificationSigned".into(), json!(vr.signature.is_some()));
            map.insert("verificationReceipt".into(), json!(vr));
            if verifier_keys.is_empty() {
                map.insert("outcomeVerified".into(), json!(false));
                map.insert(
                    "verificationUnverifiedReason".into(),
                    json!("no --verifier-key configured to verify receipt signature"),
                );
            } else {
                match vr.verify_with_revoked(verifier_keys, revoked_keys) {
                    Ok(()) => {
                        let verified = vr.passed && vr.out_of_scope_files.is_empty();
                        map.insert("outcomeVerified".into(), json!(verified));
                    }
                    Err(err) => {
                        map.insert("outcomeVerified".into(), json!(false));
                        map.insert("verificationError".into(), json!(err));
                    }
                }
            }
        }
        Ok(_) => {
            map.insert("outcomeVerified".into(), json!(false));
            map.insert(
                "verificationError".into(),
                json!("verification receipt tenant or task does not match run"),
            );
        }
        Err(err) => {
            map.insert("outcomeVerified".into(), json!(false));
            map.insert("verificationError".into(), json!(err));
        }
    }
    Ok(out)
}

pub fn receipt_from_verified_events(events: &[AuditEvent], run_id: &str) -> Result<Value, String> {
    if run_id.is_empty() || run_id.len() > 256 || run_id.chars().any(char::is_control) {
        return Err("run ID must be 1..=256 printable bytes".into());
    }
    let events: Vec<&AuditEvent> = events
        .iter()
        .filter(|event| event.run_id == run_id)
        .collect();
    if events.is_empty() {
        return Err(format!("no audited events for run '{run_id}'"));
    }
    let mut model_status = "unknown_requires_reconciliation";
    let mut denials = 0usize;
    let mut invocations = std::collections::BTreeSet::new();
    for event in &events {
        if event.decision == crate::journal::decision::DENY {
            denials += 1;
        }
        if event.invocation_id != "run" && !event.invocation_id.is_empty() {
            invocations.insert(event.invocation_id.as_str());
        }
        if event.action == crate::tools::action::AGENT_RUN {
            model_status = match event.decision.as_str() {
                crate::journal::decision::STARTED => "unknown_requires_reconciliation",
                crate::journal::decision::COMPLETED => "recorded_model_complete",
                crate::journal::decision::FAILED => "recorded_model_failed",
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

    #[test]
    fn signed_passing_verification_receipt_sets_outcome_verified_when_key_matches() {
        use crate::types::{VerificationReceipt, VerificationSignature};
        use ed25519_dalek::{Signer, SigningKey};

        let root = std::env::temp_dir().join(format!("jani-evidence-ver-{}", new_id("test")));
        fs::create_dir_all(&root).unwrap();
        let mut journal = Journal::open(&root).unwrap();
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
                decision: "completed",
                reason_code: "final_answer",
                policy_revision: "runtime",
                approval_id: None,
                sandbox_receipt_id: None,
                data_classification: Classification::Internal,
            })
            .unwrap();
        drop(journal);

        let secret = [0x33u8; 32];
        let key = SigningKey::from_bytes(&secret);
        let pub_spec = format!("v1:{}", hex::encode(key.verifying_key().to_bytes()));
        let mut vr = VerificationReceipt {
            api_version: "jani.dev/v1alpha1".into(),
            kind: "VerificationReceipt".into(),
            verification_id: "ver-1".into(),
            tenant_id: "tenant".into(),
            task_id: "task".into(),
            contract_digest: "abc".into(),
            verified_at: 100,
            passed: true,
            verifier_id: "verifier".into(),
            checks: vec![],
            changed_files: vec![],
            out_of_scope_files: vec![],
            signature: None,
        };
        let sig = hex::encode(key.sign(&vr.signing_bytes().unwrap()).to_bytes());
        vr.signature = Some(VerificationSignature {
            key_id: "v1".into(),
            value: sig,
        });
        let vr_path = types::verification_receipt_path(&root, "task").unwrap();
        fs::create_dir_all(vr_path.parent().unwrap()).unwrap();
        fs::write(&vr_path, serde_json::to_string(&vr).unwrap()).unwrap();

        let no_keys = receipt(&root, "run").unwrap();
        assert_eq!(no_keys["outcomeVerified"], false);
        assert_eq!(no_keys["verificationPresent"], true);

        let verified = receipt_with_verifier_keys(&root, "run", &[pub_spec], &[]).unwrap();
        assert_eq!(verified["outcomeVerified"], true);
        assert_eq!(verified["verificationPassed"], true);
        let _ = fs::remove_dir_all(root);
    }
}
