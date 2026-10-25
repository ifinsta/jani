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

const SETTINGS: &str = r#"{
    "version": 1,
    "defaultModel": "fast",
    "models": {
        "fast": {"model": "profile-model", "apiBase": "https://settings.test/v1"},
        "deep": {"model": "deep-model", "apiBase": "https://settings.test/v1"}
    },
    "maxSteps": 30
}"#;

fn settings_workspace(settings: &str) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!(
        "jani-cli-settings-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(root.join(".jani")).unwrap();
    fs::write(root.join(".jani").join("settings.json"), settings).unwrap();
    root
}

#[test]
fn settings_profiles_supply_model_with_env_and_cli_overrides() {
    let root = settings_workspace(SETTINGS);
    let repo = root.to_str().unwrap().to_string();
    let state = root.join(".jani").to_str().unwrap().to_string();
    let invoke = |extra: &[&str], env_model: Option<&str>| {
        let mut command = Command::new(env!("CARGO_BIN_EXE_jani"));
        command
            .arg("doctor")
            .args(["--repo", repo.as_str(), "--state-dir", state.as_str()])
            .args(extra)
            .env_remove("JANI_API_KEY")
            .env_remove("OPENAI_API_KEY")
            .env_remove("JANI_MODE")
            .env_remove("JANI_SETTINGS");
        if let Some(model) = env_model {
            command.env("JANI_MODEL", model).env_remove("OPENAI_MODEL");
        } else {
            command.env_remove("JANI_MODEL").env_remove("OPENAI_MODEL");
        }
        command.output().expect("execute jani")
    };

    let profile = invoke(&[], None);
    assert!(
        profile.status.success(),
        "{}",
        String::from_utf8_lossy(&profile.stderr)
    );
    let stdout = String::from_utf8_lossy(&profile.stdout);
    assert!(stdout.contains("Model: profile-model"), "{stdout}");
    assert!(stdout.contains("(profile fast)"), "{stdout}");

    let env = invoke(&[], Some("env-model"));
    assert!(
        String::from_utf8_lossy(&env.stdout).contains("Model: env-model"),
        "{}",
        String::from_utf8_lossy(&env.stdout)
    );

    let selected = invoke(&["--model-profile", "deep"], None);
    let stdout = String::from_utf8_lossy(&selected.stdout);
    assert!(stdout.contains("Model: deep-model"), "{stdout}");
    assert!(stdout.contains("(profile deep)"), "{stdout}");

    let cli = invoke(&["--model", "cli-model"], None);
    assert!(
        String::from_utf8_lossy(&cli.stdout).contains("Model: cli-model"),
        "{}",
        String::from_utf8_lossy(&cli.stdout)
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn settings_reject_inline_credentials_and_help_still_works() {
    let root = settings_workspace(
        r#"{"version":1,"models":{"fast":{"model":"m","apiKey":"fixture-inline-credential"}},"defaultModel":"fast"}"#,
    );
    let repo = root.to_str().unwrap().to_string();
    let output = Command::new(env!("CARGO_BIN_EXE_jani"))
        .arg("doctor")
        .args(["--repo", repo.as_str()])
        .env_remove("JANI_API_KEY")
        .env_remove("OPENAI_API_KEY")
        .output()
        .expect("execute jani");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("apiKeyEnv"), "{stderr}");
    assert!(!stderr.contains("fixture-inline-credential"), "{stderr}");

    let help = Command::new(env!("CARGO_BIN_EXE_jani"))
        .args(["--help", "--repo", repo.as_str()])
        .output()
        .expect("execute jani");
    assert!(help.status.success());
    assert!(String::from_utf8_lossy(&help.stdout).contains("USAGE:"));
    fs::remove_dir_all(root).unwrap();
}
