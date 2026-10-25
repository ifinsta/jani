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
