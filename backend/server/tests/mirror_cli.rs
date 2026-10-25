use std::process::Command;

#[test]
fn mirror_is_offline_and_reports_empty_workflow_without_a_journal() {
    let root = std::env::temp_dir().join(format!(
        "jani-mirror-cli-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let state = root.join(".jani");
    let out = Command::new(env!("CARGO_BIN_EXE_jani"))
        .args([
            "mirror",
            "workflows",
            "--repo",
            root.to_str().unwrap(),
            "--state-dir",
            state.to_str().unwrap(),
        ])
        .env_remove("JANI_API_KEY")
        .env_remove("OPENAI_API_KEY")
        .env_remove("JANI_POLICY")
        .env_remove("JANI_IDENTITY")
        .output()
        .expect("run Mirror");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["consideredRuns"], 0);
    assert_eq!(v["version"], "mirror/v1");
    let bad = Command::new(env!("CARGO_BIN_EXE_jani"))
        .args([
            "mirror",
            "inspect",
            "missing-run",
            "--repo",
            root.to_str().unwrap(),
            "--state-dir",
            state.to_str().unwrap(),
        ])
        .env_remove("JANI_POLICY")
        .env_remove("JANI_IDENTITY")
        .output()
        .expect("inspect nonexistent run");
    assert!(!bad.status.success());
    assert!(String::from_utf8_lossy(&bad.stderr).contains("run not found"));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn mirror_reads_verified_run_and_records_proposal_idempotently() {
    use jani_core::journal::{EventInput, Journal};
    use jani_core::types::Classification;
    let root = std::env::temp_dir().join(format!(
        "jani-mirror-e2e-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let state = root.join(".jani");
    {
        let mut j = Journal::open(&state).unwrap();
        for (action, decision, reason) in [
            ("agent.run", "started", "run_started"),
            ("repo.read", "permit", "ok"),
            ("agent.run", "failed", "step_limit"),
        ] {
            j.record(EventInput {
                tenant_id: "test-tenant",
                task_id: "task-1",
                run_id: "test-run",
                invocation_id: "inv-1",
                actor_id: "local",
                worker_id: "local",
                action,
                resource_digest: "hidden-resource-digest",
                decision,
                reason_code: reason,
                policy_revision: "test",
                approval_id: None,
                sandbox_receipt_id: None,
                data_classification: Classification::Internal,
            })
            .unwrap();
        }
    }
    let invoke = |sub: &str| {
        Command::new(env!("CARGO_BIN_EXE_jani"))
            .args([
                "mirror",
                sub,
                "test-run",
                "--repo",
                root.to_str().unwrap(),
                "--state-dir",
                state.to_str().unwrap(),
                "--tenant",
                "test-tenant",
            ])
            .env_remove("JANI_API_KEY")
            .env_remove("OPENAI_API_KEY")
            .env_remove("JANI_POLICY")
            .env_remove("JANI_IDENTITY")
            .output()
            .unwrap()
    };
    let review = invoke("critique");
    assert!(
        review.status.success(),
        "{}",
        String::from_utf8_lossy(&review.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&review.stdout).unwrap();
    assert_eq!(report["outcome"], "failed");
    assert_eq!(report["findings"][0]["code"], "step_budget_exhausted");
    assert!(!String::from_utf8_lossy(&review.stdout).contains("hidden-resource-digest"));

    let saved = invoke("record");
    assert!(
        saved.status.success(),
        "{}",
        String::from_utf8_lossy(&saved.stderr)
    );
    let record: serde_json::Value = serde_json::from_slice(&saved.stdout).unwrap();
    assert_eq!(record["recorded"], 1);
    let duplicate: serde_json::Value = serde_json::from_slice(&invoke("record").stdout).unwrap();
    assert_eq!(duplicate["recorded"], 0);

    let history = Command::new(env!("CARGO_BIN_EXE_jani"))
        .args([
            "mirror",
            "history",
            "--repo",
            root.to_str().unwrap(),
            "--state-dir",
            state.to_str().unwrap(),
            "--tenant",
            "test-tenant",
        ])
        .env_remove("JANI_POLICY")
        .env_remove("JANI_IDENTITY")
        .output()
        .unwrap();
    assert!(
        history.status.success(),
        "{}",
        String::from_utf8_lossy(&history.stderr)
    );
    let stored: serde_json::Value = serde_json::from_slice(&history.stdout).unwrap();
    assert_eq!(stored["recommendations"].as_array().unwrap().len(), 1);

    let other = Command::new(env!("CARGO_BIN_EXE_jani"))
        .args([
            "mirror",
            "inspect",
            "test-run",
            "--repo",
            root.to_str().unwrap(),
            "--state-dir",
            state.to_str().unwrap(),
            "--tenant",
            "other-tenant",
        ])
        .env_remove("JANI_POLICY")
        .env_remove("JANI_IDENTITY")
        .output()
        .unwrap();
    assert!(!other.status.success());
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn mirror_refuses_unverified_audit_data() {
    let root = std::env::temp_dir().join(format!(
        "jani-mirror-corrupt-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let state = root.join(".jani");
    std::fs::create_dir_all(&state).unwrap();
    std::fs::write(state.join("journal.jsonl"), "{broken\n").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_jani"))
        .args([
            "mirror",
            "workflows",
            "--repo",
            root.to_str().unwrap(),
            "--state-dir",
            state.to_str().unwrap(),
        ])
        .env_remove("JANI_POLICY")
        .env_remove("JANI_IDENTITY")
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "invalid audit data must fail closed"
    );
    std::fs::remove_dir_all(root).unwrap();
}
