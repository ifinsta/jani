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
        .args(["mirror","workflows","--repo",root.to_str().unwrap(),
            "--state-dir",state.to_str().unwrap()])
        .env_remove("JANI_API_KEY")
        .env_remove("OPENAI_API_KEY")
        .env_remove("JANI_POLICY")
        .env_remove("JANI_IDENTITY")
        .output().expect("run Mirror");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["consideredRuns"],0);
    assert_eq!(v["version"],"mirror/v1");
    let bad = Command::new(env!("CARGO_BIN_EXE_jani"))
        .args(["mirror","inspect","missing-run","--repo",root.to_str().unwrap(),
            "--state-dir",state.to_str().unwrap()])
        .env_remove("JANI_POLICY")
        .env_remove("JANI_IDENTITY")
        .output().expect("inspect nonexistent run");
    assert!(!bad.status.success());
    assert!(String::from_utf8_lossy(&bad.stderr).contains("run not found"));
    std::fs::remove_dir_all(root).unwrap();
}
