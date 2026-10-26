use std::fs;

mod support;

#[test]
fn version_runs_without_provider_credentials() {
    let output = support::jani()
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
    let output = support::jani()
        .arg("--help")
        .output()
        .expect("execute jani");
    assert!(output.status.success());
    let out = String::from_utf8_lossy(&output.stdout);
    assert!(out.contains("--allow-write"));
    assert!(out.contains("--allow-exec"));
    assert!(out.contains("--write-path"));
    assert!(out.contains("--exec-program"));
    assert!(out.contains("jani repl"));
    assert!(out.contains("Interactive terminal"));
}

#[test]
fn recovery_cli_reports_unknown_then_completed_without_model_access() {
    use jani_core::journal::{EventInput, Journal};
    use jani_core::types::Classification;
    let root = settings_workspace(r#"{"version":1}"#);
    let state_dir = root.join("state");
    let mut journal = Journal::open(&state_dir).unwrap();
    let mut record = |decision: &str| {
        journal.record(EventInput {
            tenant_id: "tenant-1",
            task_id: "task-test",
            run_id: "run-test",
            invocation_id: "run",
            actor_id: "operator",
            worker_id: "operator",
            action: "agent.run",
            resource_digest: "digest",
            decision,
            reason_code: decision,
            policy_revision: "runtime",
            approval_id: None,
            sandbox_receipt_id: None,
            data_classification: Classification::Internal,
        }).unwrap();
    };
    let root_str = root.to_str().unwrap().to_string();
    let state_str = state_dir.to_str().unwrap().to_string();
    let invoke = |command: &[&str]| {
        support::jani()
            .args(command)
            .args(["--repo", root_str.as_str(), "--state-dir", state_str.as_str()])
            .env_remove("JANI_API_KEY")
            .env_remove("OPENAI_API_KEY")
            .output()
            .unwrap()
    };
    record("started");
    let uncertain = invoke(&["recovery", "status"]);
    assert!(uncertain.status.success(), "{}", String::from_utf8_lossy(&uncertain.stderr));
    let output = String::from_utf8_lossy(&uncertain.stdout);
    assert!(output.contains("unknown_requires_reconciliation"), "{output}");
    assert!(output.contains("\"safeToAutoReplay\": false"), "{output}");

    record("completed");
    drop(journal);
    let completed = invoke(&["recovery", "status"]);
    assert!(completed.status.success(), "{}", String::from_utf8_lossy(&completed.stderr));
    assert!(String::from_utf8_lossy(&completed.stdout).contains("recorded_model_complete"));
    assert!(invoke(&["inspect", "run-test"]).status.success());
    let evidence = invoke(&["evidence", "run-test"]);
    assert!(evidence.status.success(), "{}", String::from_utf8_lossy(&evidence.stderr));
    let receipt: serde_json::Value = serde_json::from_slice(&evidence.stdout).unwrap();
    assert_eq!(receipt["journalChainVerified"], true);
    assert_eq!(receipt["outcomeVerified"], false);
    assert_eq!(receipt["safeToAutoReplay"], false);

    use std::io::Write;
    let mut file = fs::OpenOptions::new()
        .append(true)
        .open(state_dir.join("journal.jsonl"))
        .unwrap();
    writeln!(file, "{{}}").unwrap();
    drop(file);
    let corrupted = invoke(&["recovery", "status"]);
    assert!(!corrupted.status.success());
    assert!(!invoke(&["inspect", "run-test"]).status.success());
    assert!(!invoke(&["evidence", "run-test"]).status.success());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn repl_rejects_a_fixed_run_id() {
    let output = support::jani()
        .args(["repl", "--run", "run-fixed"])
        .env_remove("JANI_API_KEY")
        .env_remove("OPENAI_API_KEY")
        .output()
        .expect("execute jani");
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("--run is not supported with repl"));
}

#[test]
fn bare_invocation_prints_help_without_a_terminal() {
    let output = support::jani()
        .env_remove("JANI_MODE")
        .env_remove("JANI_POLICY")
        .env_remove("JANI_IDENTITY")
        .env_remove("JANI_CONTRACT")
        .output()
        .expect("execute jani");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("USAGE:"), "{stdout}");
    assert!(
        !stdout.contains("Jani interactive terminal"),
        "bare invocation with piped stdin must not enter the terminal: {stdout}"
    );
}

#[test]
fn print_does_not_accept_an_option_as_prompt() {
    let output = support::jani()
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
    let output = support::jani()
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
    let output = support::jani()
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
        support::jani()
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
    let output = support::jani()
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

fn empty_workspace() -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!(
        "jani-cli-empty-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(root.join(".jani")).unwrap();
    root
}

fn user_settings_file(body: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!(
        "jani-cli-user-settings-{}-{}.json",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::write(&path, body).unwrap();
    path
}

const USER_SETTINGS: &str = r#"{
    "version": 1,
    "model": "user-profile",
    "models": {
        "user-profile": {"model": "user-model", "apiBase": "https://user.test/v1"}
    },
    "maxSteps": 11
}"#;

fn doctor_for(root: &std::path::Path, user: &std::path::Path, extra: &[&str]) -> String {
    let state = root.join(".jani");
    let output = support::jani_with_user_settings(user.to_path_buf())
        .arg("doctor")
        .args(["--repo", root.to_str().unwrap(), "--state-dir"])
        .arg(&state)
        .args(extra)
        .env_remove("JANI_API_KEY")
        .env_remove("OPENAI_API_KEY")
        .env_remove("JANI_MODEL")
        .env_remove("OPENAI_MODEL")
        .env_remove("JANI_API_BASE")
        .env_remove("JANI_SETTINGS")
        .output()
        .expect("execute jani");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn user_settings_load_when_the_worktree_has_none() {
    let root = empty_workspace();
    let user = user_settings_file(USER_SETTINGS);
    let stdout = doctor_for(&root, &user, &[]);
    assert!(
        stdout.contains(&format!(
            "Settings: {} (profile user-profile)",
            user.display()
        )),
        "{stdout}"
    );
    assert!(stdout.contains("Model: user-model"), "{stdout}");
    assert!(
        stdout.contains("Model API: openai https://user.test/v1"),
        "{stdout}"
    );
    fs::remove_dir_all(root).unwrap();
    fs::remove_file(user).unwrap();
}

#[test]
fn project_settings_shadow_user_settings_without_merging() {
    let root = settings_workspace(SETTINGS);
    let user = user_settings_file(USER_SETTINGS);
    let stdout = doctor_for(&root, &user, &[]);
    assert!(
        stdout.contains(&format!(
            "Settings: {} (profile fast)",
            root.join(".jani").join("settings.json").display()
        )),
        "{stdout}"
    );
    assert!(stdout.contains("Model: profile-model"), "{stdout}");
    assert!(!stdout.contains("user-model"), "{stdout}");
    fs::remove_dir_all(root).unwrap();
    fs::remove_file(user).unwrap();
}

#[test]
fn explicit_settings_shadow_project_and_user_settings() {
    let root = empty_workspace();
    fs::write(
        root.join("custom.json"),
        r#"{"version":1,"model":"explicit-model","apiBase":"https://explicit.test/v1"}"#,
    )
    .unwrap();
    let user = user_settings_file(USER_SETTINGS);
    let stdout = doctor_for(&root, &user, &["--settings", "custom.json"]);
    assert!(
        stdout.contains(&format!("Settings: {}", root.join("custom.json").display())),
        "{stdout}"
    );
    assert!(stdout.contains("Model: explicit-model"), "{stdout}");
    assert!(
        stdout.contains("Model API: openai https://explicit.test/v1"),
        "{stdout}"
    );
    fs::remove_dir_all(root).unwrap();
    fs::remove_file(user).unwrap();
}

#[test]
fn settings_profiles_supply_model_with_env_and_cli_overrides() {
    let root = settings_workspace(SETTINGS);
    let repo = root.to_str().unwrap().to_string();
    let state = root.join(".jani").to_str().unwrap().to_string();
    let invoke = |extra: &[&str], env_model: Option<&str>| {
        let mut command = support::jani();
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
fn settings_inline_credentials_are_accepted_and_diagnosed_without_echo() {
    let root = settings_workspace(
        r#"{"version":1,"models":{"fast":{"model":"m","apiKey":"fixture-inline-credential"}},"defaultModel":"fast"}"#,
    );
    let repo = root.to_str().unwrap().to_string();
    let output = support::jani()
        .arg("doctor")
        .args(["--repo", repo.as_str()])
        .env_remove("JANI_API_KEY")
        .env_remove("OPENAI_API_KEY")
        .env_remove("JANI_SETTINGS")
        .output()
        .expect("execute jani");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Model key: SET (settings (inline))"),
        "{stdout}"
    );
    assert!(stdout.contains("Settings warning:"), "{stdout}");
    assert!(!stdout.contains("fixture-inline-credential"), "{stdout}");
    assert!(!String::from_utf8_lossy(&output.stderr).contains("fixture-inline-credential"));

    let help = support::jani()
        .args(["--help", "--repo", repo.as_str()])
        .output()
        .expect("execute jani");
    assert!(help.status.success());
    assert!(String::from_utf8_lossy(&help.stdout).contains("USAGE:"));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn settings_profile_credential_binding_beats_a_generic_environment_key() {
    let root = settings_workspace(
        r#"{"version":1,"models":{"fast":{"model":"m","apiKeyEnv":"PROFILE_TEST_KEY"}},"defaultModel":"fast"}"#,
    );
    let repo = root.to_str().unwrap().to_string();
    let output = support::jani()
        .arg("doctor")
        .args(["--repo", repo.as_str()])
        .env("PROFILE_TEST_KEY", "profile-key")
        .env("JANI_API_KEY", "generic-key")
        .env_remove("OPENAI_API_KEY")
        .env_remove("JANI_SETTINGS")
        .output()
        .expect("execute jani");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Model key: SET (env PROFILE_TEST_KEY)"),
        "{stdout}"
    );
    assert!(!stdout.contains("profile-key"), "{stdout}");
    assert!(!stdout.contains("generic-key"), "{stdout}");
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn settings_permissions_and_top_level_model_reach_doctor() {
    let root = settings_workspace(
        r#"{
            "$schema": "./settings.schema.json",
            "version": 1,
            "model": "claude-sonnet-5-5",
            "defaultModel": "fast",
            "models": {
                "fast": {
                    "model": "base-model",
                    "apiBase": "https://api.anthropic.com/v1",
                    "protocol": "anthropic"
                }
            },
            "permissions": {
                "defaultMode": "plan",
                "allow": ["Read(./**)"],
                "deny": ["Read(./.env)"]
            }
        }"#,
    );
    let repo = root.to_str().unwrap().to_string();
    let output = support::jani()
        .arg("doctor")
        .args(["--repo", repo.as_str()])
        .env_remove("JANI_API_KEY")
        .env_remove("OPENAI_API_KEY")
        .env_remove("JANI_MODEL")
        .env_remove("JANI_PROTOCOL")
        .env_remove("JANI_SETTINGS")
        .output()
        .expect("execute jani");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Model: claude-sonnet-5-5"), "{stdout}");
    assert!(
        stdout.contains("Model API: anthropic https://api.anthropic.com/v1"),
        "{stdout}"
    );
    assert!(
        stdout.contains("Permissions: mode=plan allow=1 ask=0 deny=1"),
        "{stdout}"
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn settings_raw_top_level_model_works_without_a_default_profile() {
    let root = settings_workspace(
        r#"{
            "version": 1,
            "model": "raw-claude",
            "apiBase": "https://api.anthropic.com/v1",
            "models": {"fast": {"model": "base-model"}}
        }"#,
    );
    let repo = root.to_str().unwrap().to_string();
    let run = |extra: &[&str]| {
        let mut command = support::jani();
        command
            .arg("doctor")
            .args(["--repo", repo.as_str()])
            .args(extra)
            .env_remove("JANI_API_KEY")
            .env_remove("OPENAI_API_KEY")
            .env_remove("JANI_MODEL")
            .env_remove("JANI_PROTOCOL")
            .env_remove("JANI_SETTINGS");
        command.output().expect("execute jani")
    };

    let raw = run(&[]);
    assert!(
        raw.status.success(),
        "{}",
        String::from_utf8_lossy(&raw.stderr)
    );
    let stdout = String::from_utf8_lossy(&raw.stdout);
    assert!(stdout.contains("Model: raw-claude"), "{stdout}");
    assert!(
        stdout.contains("Model API: anthropic https://api.anthropic.com/v1"),
        "{stdout}"
    );

    let profile = run(&["--model-profile", "fast"]);
    assert!(
        String::from_utf8_lossy(&profile.stdout).contains("Model: base-model"),
        "{}",
        String::from_utf8_lossy(&profile.stdout)
    );
    fs::remove_dir_all(root).unwrap();
}
