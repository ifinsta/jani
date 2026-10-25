use std::fs;
use std::process::Command;

#[test]
fn version_runs_without_provider_credentials() {
    let output = Command::new(env!("CARGO_BIN_EXE_jani"))
        .arg("--version")
        .env_remove("JANI_API_KEY")
        .env_remove("OPENAI_API_KEY")
        .output()
        .expect("execute jani");
    assert!(output.status.success());
    let expected = format!("jani {}", env!("CARGO_PKG_VERSION"));
    assert!(String::from_utf8_lossy(&output.stdout).contains(&expected));
}

#[test]
fn help_documents_permissions() {
    let output = Command::new(env!("CARGO_BIN_EXE_jani"))
        .arg("--help")
        .output()
        .expect("execute jani");
    assert!(output.status.success());
    let out = String::from_utf8_lossy(&output.stdout);
    assert!(out.contains("--allow-write"));
    assert!(out.contains("--allow-exec"));
    assert!(out.contains("--write-path"));
    assert!(out.contains("--exec-program"));
}

#[test]
fn print_does_not_accept_an_option_as_prompt() {
    let output = Command::new(env!("CARGO_BIN_EXE_jani"))
        .args(["--print", "--allow-write"])
        .output()
        .expect("execute jani");
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("requires a prompt"));
}

#[test]
fn shipped_work_contract_example_validates() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("docs")
        .join("schemas")
        .join("work-contract.example.json");
    let output = Command::new(env!("CARGO_BIN_EXE_jani"))
        .arg("contract")
        .arg("validate")
        .arg(&path)
        .output()
        .expect("execute jani");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("\"ok\": true"));
}

#[test]
fn unknown_subcommand_fails_instead_of_printing_help() {
    let output = Command::new(env!("CARGO_BIN_EXE_jani"))
        .arg("unsupported-command")
        .env_remove("JANI_API_KEY")
        .env_remove("OPENAI_API_KEY")
        .output()
        .expect("execute jani");
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("unknown command"));
    assert!(!String::from_utf8_lossy(&output.stdout).contains("USAGE:"));
}

#[test]
fn native_operation_named_run_is_not_the_agent_command() {
    let root = std::env::temp_dir().join(format!(
        "jani-cli-native-run-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&root).unwrap();
    fs::write(
        root.join("plan.json"),
        r#"{"nodes":[{"id":"inspect","deps":[],"status":"planned","acceptance":"ok","priority":1}]}"#,
    )
    .unwrap();
    let repo = root.to_str().unwrap().to_string();
    let state = root.join(".jani").to_str().unwrap().to_string();
    let invoke = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_jani"))
            .args(args)
            .args([
                "--repo",
                repo.as_str(),
                "--state-dir",
                state.as_str(),
                "--tenant",
                "team-1",
                "--task",
                "task-1",
            ])
            .env_remove("JANI_API_KEY")
            .env_remove("OPENAI_API_KEY")
            .env_remove("JANI_MODE")
            .env_remove("JANI_POLICY")
            .env_remove("JANI_IDENTITY")
            .output()
            .unwrap()
    };
    let plan = invoke(&["native", "planner", "create", "plan.json"]);
    assert!(
        plan.status.success(),
        "{}",
        String::from_utf8_lossy(&plan.stderr)
    );
    let simulated = invoke(&["native", "simulator", "run", "42"]);
    assert!(
        simulated.status.success(),
        "{}",
        String::from_utf8_lossy(&simulated.stderr)
    );
    assert!(String::from_utf8_lossy(&simulated.stdout).contains("\"actualExecutions\": 0"));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn version_ignores_unrelated_invalid_environment() {
    let output = Command::new(env!("CARGO_BIN_EXE_jani"))
        .arg("--version")
        .env("JANI_AGENT_NAME", "INVALID")
        .env("JANI_WRITE_PATHS", "../escape")
        .env("JANI_API_BASE", "ftp://invalid")
        .output()
        .expect("execute jani");
    assert!(output.status.success());
}
