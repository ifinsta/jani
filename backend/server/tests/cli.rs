use serde_json::{json, Value};
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

mod support;

fn unique_temp_suffix() -> String {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    format!(
        "t{}c{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    )
}

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
    let mut record = |run_id: &str, decision: &str| {
        journal
            .record(EventInput {
                tenant_id: "tenant-1",
                task_id: "task-test",
                run_id,
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
            })
            .unwrap();
    };
    let root_str = root.to_str().unwrap().to_string();
    let state_str = state_dir.to_str().unwrap().to_string();
    let invoke = |command: &[&str]| {
        support::jani()
            .args(command)
            .args([
                "--repo",
                root_str.as_str(),
                "--state-dir",
                state_str.as_str(),
            ])
            .env_remove("JANI_API_KEY")
            .env_remove("OPENAI_API_KEY")
            .output()
            .unwrap()
    };
    record("run-test", "started");
    let uncertain = invoke(&["recovery", "status"]);
    assert!(
        uncertain.status.success(),
        "{}",
        String::from_utf8_lossy(&uncertain.stderr)
    );
    let output = String::from_utf8_lossy(&uncertain.stdout);
    assert!(
        output.contains("unknown_requires_reconciliation"),
        "{output}"
    );
    assert!(output.contains("\"safeToAutoReplay\": false"), "{output}");

    record("run-other", "started");
    record("run-test", "completed");
    drop(journal);
    let completed = invoke(&["recovery", "status"]);
    assert!(
        completed.status.success(),
        "{}",
        String::from_utf8_lossy(&completed.stderr)
    );
    assert!(String::from_utf8_lossy(&completed.stdout).contains("recorded_model_complete"));
    let inspection = invoke(&["inspect", "run-test"]);
    assert!(inspection.status.success());
    let inspected: Vec<jani_core::types::AuditEvent> =
        serde_json::from_slice(&inspection.stdout).unwrap();
    assert_eq!(inspected.len(), 2);
    assert!(inspected.iter().all(|event| event.run_id == "run-test"));
    let evidence = invoke(&["evidence", "run-test"]);
    assert!(
        evidence.status.success(),
        "{}",
        String::from_utf8_lossy(&evidence.stderr)
    );
    let receipt: serde_json::Value = serde_json::from_slice(&evidence.stdout).unwrap();
    assert_eq!(receipt["journalChainVerified"], true);
    assert_eq!(receipt["outcomeVerified"], false);
    assert_eq!(receipt["safeToAutoReplay"], false);

    let path = state_dir.join("journal.jsonl");
    let valid = fs::read_to_string(&path).unwrap();
    fs::write(&path, valid.replacen("run-other", "damaged-run", 1)).unwrap();
    let refused = invoke(&["inspect", "run-test"]);
    assert!(!refused.status.success());
    assert!(refused.stdout.is_empty());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("line 2: event digest mismatch"));
    fs::write(&path, valid).unwrap();

    use std::io::Write;
    let mut file = fs::OpenOptions::new().append(true).open(path).unwrap();
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
        unique_temp_suffix()
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
        unique_temp_suffix()
    ));
    fs::create_dir_all(root.join(".jani")).unwrap();
    fs::write(root.join(".jani").join("settings.json"), settings).unwrap();
    root
}

fn empty_workspace() -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!(
        "jani-cli-empty-{}-{}",
        std::process::id(),
        unique_temp_suffix()
    ));
    fs::create_dir_all(root.join(".jani")).unwrap();
    root
}

fn user_settings_file(body: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!(
        "jani-cli-user-settings-{}-{}.json",
        std::process::id(),
        unique_temp_suffix()
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
    let root = settings_workspace(
        r#"{"version":1,"defaultModel":"fast","models":{"fast":{"model":"profile-model"}},"maxSteps":30}"#,
    );
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
    assert!(!stdout.contains("https://user.test/v1"), "{stdout}");
    fs::remove_dir_all(root).unwrap();
    fs::remove_file(user).unwrap();
}

#[test]
fn explicit_settings_shadow_project_and_user_settings() {
    let root = settings_workspace(SETTINGS);
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
            .args(["--settings", ".jani/settings.json"])
            .args(extra)
            .env_remove("JANI_API_BASE")
            .env_remove("JANI_PROTOCOL")
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
        .args(["--repo", repo.as_str(), "--settings", ".jani/settings.json"])
        .env_remove("JANI_API_BASE")
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
        .args(["--repo", repo.as_str(), "--settings", ".jani/settings.json"])
        .env_remove("JANI_API_BASE")
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
            .args(["--repo", repo.as_str(), "--settings", ".jani/settings.json"])
            .args(extra)
            .env_remove("JANI_API_BASE")
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

const DUMMY_JANI_KEY: &str = "fixture-settings-jani-key";
const DUMMY_OPENAI_KEY: &str = "fixture-settings-openai-key";
const DUMMY_PROFILE_KEY: &str = "fixture-settings-profile-key";
const DUMMY_KEY_ENV: &str = "JANI_CLI_SETTINGS_TEST_KEY";
const ROUTING_FIELDS: [&str; 6] = [
    "apiBase",
    "apiKeyEnv",
    "models.fast.apiBase",
    "models.fast.apiKeyEnv",
    "models.unused.apiBase",
    "models.unused.apiKeyEnv",
];

fn settings_security_command(root: &Path, mut command: Command) -> Command {
    // Keep loopback tests isolated from developer settings, credentials and proxies.
    for (name, _) in std::env::vars_os() {
        let upper = name.to_string_lossy().to_ascii_uppercase();
        if upper.starts_with("JANI_") && upper != "JANI_USER_SETTINGS" {
            command.env_remove(name);
        }
    }
    command
        .env_remove("JANI_API_BASE")
        .env_remove("JANI_SETTINGS");
    for name in [
        "OPENAI_MODEL",
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "ALL_PROXY",
        "http_proxy",
        "https_proxy",
        "all_proxy",
    ] {
        command.env_remove(name);
    }
    command
        .arg("--repo")
        .arg(root)
        .arg("--state-dir")
        .arg(root.join("state"))
        .args(["--protocol", "openai"])
        .env("NO_PROXY", "127.0.0.1,localhost")
        .env("no_proxy", "127.0.0.1,localhost")
        .env("JANI_API_KEY", DUMMY_JANI_KEY)
        .env("OPENAI_API_KEY", DUMMY_OPENAI_KEY)
        .env(DUMMY_KEY_ENV, DUMMY_PROFILE_KEY)
        .stdin(Stdio::null());
    command
}

fn assert_no_dummy_keys(output: &Output) {
    for key in [DUMMY_JANI_KEY, DUMMY_OPENAI_KEY, DUMMY_PROFILE_KEY] {
        assert!(
            !String::from_utf8_lossy(&output.stdout).contains(key),
            "dummy credential exposed on stdout"
        );
        assert!(
            !String::from_utf8_lossy(&output.stderr).contains(key),
            "dummy credential exposed on stderr"
        );
    }
}

fn settings_mock_listener() -> (TcpListener, String) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}/v1", listener.local_addr().unwrap());
    listener.set_nonblocking(true).unwrap();
    (listener, base)
}

struct SettingsRequest {
    request_line: String,
    authorization: String,
    body: Value,
}

fn read_settings_request(stream: &mut TcpStream) -> SettingsRequest {
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut reader = BufReader::new(stream);
    let mut request_line = String::new();
    assert!(reader.read_line(&mut request_line).unwrap() > 0);
    let mut length = None;
    let mut authorization = None;
    loop {
        let mut line = String::new();
        assert!(reader.read_line(&mut line).unwrap() > 0);
        if line.trim().is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            if name.eq_ignore_ascii_case("content-length") {
                length = Some(value.trim().parse::<usize>().unwrap());
            }
            if name.eq_ignore_ascii_case("authorization") {
                let value = value.trim();
                assert!(
                    [DUMMY_JANI_KEY, DUMMY_OPENAI_KEY, DUMMY_PROFILE_KEY]
                        .iter()
                        .any(|key| value == format!("Bearer {key}")),
                    "the mock may only capture dummy Authorization credentials"
                );
                authorization = Some(value.to_string());
            }
        }
    }
    let mut body = vec![0; length.expect("HTTP request content length")];
    reader.read_exact(&mut body).unwrap();
    for key in [DUMMY_JANI_KEY, DUMMY_OPENAI_KEY, DUMMY_PROFILE_KEY] {
        assert!(
            !String::from_utf8_lossy(&body).contains(key),
            "dummy credential exposed in the model request body"
        );
    }
    SettingsRequest {
        request_line,
        authorization: authorization.expect("dummy Authorization header"),
        body: serde_json::from_slice(&body).unwrap(),
    }
}

fn run_settings_mock(
    mut command: Command,
    listener: TcpListener,
    responses: Vec<(u16, String)>,
) -> (Output, usize, Vec<SettingsRequest>) {
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("execute jani");
    // Drain both pipes while serving HTTP so failures cannot deadlock the test.
    let mut stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let stdout = thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout.read_to_end(&mut bytes).unwrap();
        bytes
    });
    let stderr = thread::spawn(move || {
        let mut bytes = Vec::new();
        stderr.read_to_end(&mut bytes).unwrap();
        bytes
    });
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut responses = responses.into_iter();
    let mut connections = 0;
    let mut requests = Vec::new();
    let status = loop {
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("settings CLI run exceeded 60 seconds");
        }
        let exited = child.try_wait().unwrap();
        // Check the backlog even after exit: count every TCP connection, not
        // merely successful HTTP requests. No fixed no-network sleep is needed.
        match listener.accept() {
            Ok((mut stream, _)) => {
                connections += 1;
                stream.set_nonblocking(false).unwrap();
                requests.push(read_settings_request(&mut stream));
                let (status, body) = responses.next().unwrap_or_else(|| (400, "{}".into()));
                let response = format!(
                    "HTTP/1.1 {status} Mock\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream.write_all(response.as_bytes()).unwrap();
                stream.flush().unwrap();
                let _ = stream.shutdown(Shutdown::Both);
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                if let Some(status) = exited {
                    break status;
                }
                thread::sleep(Duration::from_millis(10));
            }
            Err(error) => panic!("accept mock connection: {error}"),
        }
    };
    let output = Output {
        status,
        stdout: stdout.join().unwrap(),
        stderr: stderr.join().unwrap(),
    };
    assert_no_dummy_keys(&output);
    (output, connections, requests)
}

fn settings_text_reply(text: &str) -> (u16, String) {
    (
        200,
        json!({
            "choices": [{"message": {"role": "assistant", "content": text}}],
            "usage": {"prompt_tokens": 10, "completion_tokens": 4}
        })
        .to_string(),
    )
}

fn tool_call_reply(tool: &str, args: Value, id: &str) -> (u16, String) {
    (
        200,
        json!({
            "choices": [{"message": {"role": "assistant", "content": null, "tool_calls": [{
                "id": id, "type": "function",
                "function": {"name": tool, "arguments": args.to_string()}
            }]}}],
            "usage": {"prompt_tokens": 10, "completion_tokens": 4}
        })
        .to_string(),
    )
}

fn recorded_tool_result(requests: &[SettingsRequest]) -> Value {
    let tool = requests[1].body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|message| message["role"] == "tool")
        .expect("tool result");
    serde_json::from_str(tool["content"].as_str().unwrap()).unwrap()
}

fn routing_settings(field: &str, base: &str) -> String {
    let mut settings = json!({"version": 1, "model": "settings-boundary-model"});
    let value = if field.ends_with("apiBase") {
        base
    } else {
        DUMMY_KEY_ENV
    };
    if let Some(profile_field) = field.strip_prefix("models.") {
        let (profile, field) = profile_field.split_once('.').unwrap();
        settings = json!({
            "version": 1,
            "defaultModel": "fast",
            "models": {"fast": {"model": "settings-boundary-model"}}
        });
        if profile == "unused" {
            settings["models"][profile] = json!({"model": "inactive-model"});
        }
        settings["models"][profile][field] = json!(value);
    } else {
        settings[field] = json!(value);
    }
    settings.to_string()
}

fn print_routing_command(root: &Path, field: &str, base: &str, command: Command) -> Command {
    let mut command = settings_security_command(root, command);
    command.args(["--print", "settings boundary prompt"]);
    if field != "apiBase" && field != "models.fast.apiBase" {
        // apiKeyEnv-only and inactive-profile fixtures still must never reach
        // a real provider if the startup refusal regresses.
        command.args(["--api-base", base]);
    }
    command
}

fn assert_automatic_routing_is_rejected(field: &str) {
    let (listener, base) = settings_mock_listener();
    let root = settings_workspace(&routing_settings(field, &base));
    let user = root.join("trusted-user.json");
    fs::write(
        &user,
        json!({
            "version": 1, "model": "trusted-fallback-model",
            "apiBase": base, "apiKeyEnv": DUMMY_KEY_ENV
        })
        .to_string(),
    )
    .unwrap();
    let command =
        print_routing_command(&root, field, &base, support::jani_with_user_settings(user));
    let (output, connections, requests) = run_settings_mock(
        command,
        listener,
        vec![settings_text_reply("must not reach a model")],
    );
    assert!(
        !output.status.success(),
        "automatic routing must fail at startup"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("worktree settings file may not choose"),
        "{stderr}"
    );
    assert!(stderr.contains(field), "{stderr}");
    assert!(stderr.contains("--settings"), "{stderr}");
    assert!(stderr.contains("JANI_SETTINGS"), "{stderr}");
    assert_eq!(
        connections, 0,
        "startup refusal must precede any TCP connection"
    );
    assert!(requests.is_empty());
    assert!(
        !root.join("state").exists(),
        "startup refusal must precede state creation"
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn automatic_top_level_api_base_fails_before_http() {
    assert_automatic_routing_is_rejected("apiBase");
}

#[test]
fn automatic_top_level_api_key_env_fails_before_http() {
    assert_automatic_routing_is_rejected("apiKeyEnv");
}

#[test]
fn automatic_selected_profile_api_base_fails_before_http() {
    assert_automatic_routing_is_rejected("models.fast.apiBase");
}

#[test]
fn automatic_selected_profile_api_key_env_fails_before_http() {
    assert_automatic_routing_is_rejected("models.fast.apiKeyEnv");
}

#[test]
fn automatic_inactive_profile_api_base_fails_before_http() {
    assert_automatic_routing_is_rejected("models.unused.apiBase");
}

#[test]
fn automatic_inactive_profile_api_key_env_fails_before_http() {
    assert_automatic_routing_is_rejected("models.unused.apiKeyEnv");
}

fn assert_explicit_routing_runs(use_env: bool) {
    for field in ROUTING_FIELDS {
        let (listener, base) = settings_mock_listener();
        let root = settings_workspace(&routing_settings(field, &base));
        let mut command = print_routing_command(&root, field, &base, support::jani());
        if use_env {
            command.env("JANI_SETTINGS", ".jani/settings.json");
        } else {
            command.args(["--settings", ".jani/settings.json"]);
        }
        let (output, connections, requests) = run_settings_mock(
            command,
            listener,
            vec![settings_text_reply("settings boundary answer")],
        );
        assert!(
            output.status.success(),
            "{field}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("settings boundary answer"));
        assert_eq!(connections, 1, "{field}");
        assert_eq!(requests.len(), 1, "{field}");
        assert_eq!(
            requests[0].request_line.trim(),
            "POST /v1/chat/completions HTTP/1.1"
        );
        assert_eq!(requests[0].body["model"], "settings-boundary-model");
        let key = if field == "apiKeyEnv" || field == "models.fast.apiKeyEnv" {
            DUMMY_PROFILE_KEY
        } else {
            DUMMY_JANI_KEY
        };
        assert_eq!(requests[0].authorization, format!("Bearer {key}"));
        fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn explicit_cli_settings_accept_all_routing_fields_and_call_the_mock_model() {
    assert_explicit_routing_runs(false);
}

#[test]
fn jani_settings_accepts_all_routing_fields_and_calls_the_mock_model() {
    assert_explicit_routing_runs(true);
}

#[test]
fn user_settings_remain_trusted_with_relative_repo_from_home() {
    let (listener, base) = settings_mock_listener();
    let root = settings_workspace(
        &json!({
            "version": 1, "model": "trusted-user-model",
            "apiBase": base, "apiKeyEnv": DUMMY_KEY_ENV
        })
        .to_string(),
    );
    let mut command = settings_security_command(
        &root,
        support::jani_with_user_settings(root.join(".jani").join("settings.json")),
    );
    command
        .current_dir(&root)
        .args(["--repo", ".", "--print", "settings boundary prompt"])
        .env("HOME", &root)
        .env("USERPROFILE", &root);
    let (output, connections, requests) = run_settings_mock(
        command,
        listener,
        vec![settings_text_reply("trusted user answer")],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("trusted user answer"));
    assert_eq!(connections, 1);
    assert_eq!(requests[0].body["model"], "trusted-user-model");
    assert_eq!(
        requests[0].authorization,
        format!("Bearer {DUMMY_PROFILE_KEY}")
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn automatic_non_routing_settings_preserve_max_steps_and_permissions_end_to_end() {
    let (listener, base) = settings_mock_listener();
    let root = settings_workspace(
        r#"{
        "version": 1, "model": "non-routing-model", "maxSteps": 1,
        "permissions": {
            "defaultMode": "plan",
            "allow": ["Read(./**)"],
            "deny": ["Read(./secret.txt)"]
        }
    }"#,
    );
    fs::write(root.join("secret.txt"), "fixture-file-must-not-be-read").unwrap();
    let mut doctor = settings_security_command(&root, support::jani());
    let output = doctor
        .arg("doctor")
        .args(["--api-base", &base])
        .output()
        .unwrap();
    assert_no_dummy_keys(&output);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Permissions: mode=plan allow=1 ask=0 deny=1"),
        "{stdout}"
    );
    let tool_reply = (
        200,
        json!({
            "choices": [{"message": {"role": "assistant", "content": null, "tool_calls": [{
                "id": "read-denied", "type": "function",
                "function": {"name": "read", "arguments": "{\"path\":\"secret.txt\"}"}
            }]}}],
            "usage": {"prompt_tokens": 10, "completion_tokens": 4}
        })
        .to_string(),
    );
    let mut command = settings_security_command(&root, support::jani());
    command.args(["--print", "read secret.txt", "--api-base", &base]);
    let (output, connections, requests) = run_settings_mock(
        command,
        listener,
        vec![
            tool_reply,
            settings_text_reply("Read denied; nothing changed."),
        ],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Checkpoint: used the 1 model turns"),
        "{stdout}"
    );
    assert!(stdout.contains("Read denied; nothing changed."), "{stdout}");
    assert_eq!(connections, 2);
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].body["model"], "non-routing-model");
    assert_eq!(requests[0].body["tool_choice"], "auto");
    assert_eq!(requests[1].body["tool_choice"], "none");
    let tool = requests[1].body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|message| message["role"] == "tool")
        .expect("read tool result");
    let result: Value = serde_json::from_str(tool["content"].as_str().unwrap()).unwrap();
    assert_eq!(result["status"], "denied");
    let error = result["error"]
        .as_str()
        .expect("settings permission refusal");
    assert!(
        error.contains("denied by settings permission rule"),
        "{error}"
    );
    assert!(error.contains("Read(./secret.txt)"), "{error}");
    for request in &requests {
        assert!(!request
            .body
            .to_string()
            .contains("fixture-file-must-not-be-read"));
    }
    assert_eq!(
        fs::read_to_string(root.join("secret.txt")).unwrap(),
        "fixture-file-must-not-be-read"
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn explicit_settings_http_failure_occurs_only_after_opt_in_without_key_echo() {
    let (listener, base) = settings_mock_listener();
    let root = settings_workspace(&routing_settings("models.fast.apiKeyEnv", &base));
    let mut command = print_routing_command(&root, "models.fast.apiKeyEnv", &base, support::jani());
    command.args(["--settings", ".jani/settings.json"]);
    let (output, connections, requests) = run_settings_mock(
        command,
        listener,
        vec![(401, r#"{"error":"fixture denied"}"#.into())],
    );
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("model HTTP 401"));
    assert_eq!(connections, 1);
    assert_eq!(
        requests[0].authorization,
        format!("Bearer {DUMMY_PROFILE_KEY}")
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn explicit_settings_with_an_empty_key_binding_fail_before_http_without_ambient_fallback() {
    let (listener, base) = settings_mock_listener();
    let root = settings_workspace(&routing_settings("models.fast.apiKeyEnv", &base));
    let mut command = print_routing_command(&root, "models.fast.apiKeyEnv", &base, support::jani());
    // Even deliberate opt-in must not substitute either ambient dummy key for
    // the credential variable that the selected profile explicitly requires.
    command
        .args(["--settings", ".jani/settings.json"])
        .env_remove(DUMMY_KEY_ENV);
    let (output, connections, requests) = run_settings_mock(command, listener, vec![]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("set JANI_API_KEY"), "{stderr}");
    assert!(
        !stderr.contains("worktree settings file may not choose"),
        "{stderr}"
    );
    assert_eq!(connections, 0);
    assert!(requests.is_empty());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn permission_path_aliases_and_root_listings_are_enforced_end_to_end() {
    let mut cases = vec![
        ("read", json!({"path":"src//private/key.txt"})),
        ("list", json!({"directory":""})),
        ("list", json!({"directory":"."})),
    ];
    if cfg!(windows) {
        cases.push(("read", json!({"path":"SRC/PRIVATE/KEY.TXT"})));
    }
    for (tool_name, args) in cases {
        let (listener, base) = settings_mock_listener();
        let root = settings_workspace(
            r#"{"version":1,"permissions":{"deny":["Read(./src/private/**)"]}}"#,
        );
        fs::create_dir_all(root.join("src/private")).unwrap();
        fs::write(
            root.join("src/private/key.txt"),
            "fixture-private-file-content",
        )
        .unwrap();
        fs::write(root.join("src/public.txt"), "public").unwrap();
        let mut command = settings_security_command(&root, support::jani());
        command.args(["--print", "inspect permitted files", "--api-base", &base]);
        let reply = (200, json!({
            "choices": [{"message": {"role":"assistant", "content":null, "tool_calls":[{
                "id":"permission-path", "type":"function", "function":{"name":tool_name,"arguments":args.to_string()}
            }]}}], "usage":{"prompt_tokens":10,"completion_tokens":4}
        }).to_string());
        let (output, connections, requests) = run_settings_mock(
            command,
            listener,
            vec![reply, settings_text_reply("Permission checks completed.")],
        );
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(connections, 2);
        let tool = requests[1].body["messages"]
            .as_array()
            .unwrap()
            .iter()
            .find(|message| message["role"] == "tool")
            .unwrap();
        let result: Value = serde_json::from_str(tool["content"].as_str().unwrap()).unwrap();
        if tool_name == "read" {
            assert_eq!(result["status"], "denied", "{result}");
            assert!(
                result["error"]
                    .as_str()
                    .unwrap()
                    .contains("settings permission rule"),
                "{result}"
            );
        } else {
            let listing = result.to_string();
            assert!(listing.contains("public.txt"), "{listing}");
            assert!(!listing.contains("private"), "{listing}");
            assert!(!listing.contains("key.txt"), "{listing}");
        }
        for request in requests {
            assert!(!request
                .body
                .to_string()
                .contains("fixture-private-file-content"));
        }
        fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn native_and_repository_skill_reads_obey_deny_and_ask_end_to_end() {
    for section in ["deny", "ask"] {
        let (listener, base) = settings_mock_listener();
        let mut settings = json!({"version": 1});
        settings["permissions"] = json!({section: ["Read(./private/**)"]});
        let root = settings_workspace(&settings.to_string());
        fs::create_dir_all(root.join("private")).unwrap();
        fs::write(
            root.join("private/plan.json"),
            r#"{"nodes":[{"id":"private","deps":[],"acceptance":"native-private-plan"}]}"#,
        )
        .unwrap();
        let mut command = settings_security_command(&root, support::jani());
        command.args([
            "--print",
            "create the private plan",
            "--api-base",
            &base,
            "--tenant",
            "team-1",
            "--task",
            "task-1",
        ]);
        let (output, connections, requests) = run_settings_mock(
            command,
            listener,
            vec![
                tool_call_reply(
                    "native_tool",
                    json!({"tool": "planner", "operation": "create", "args": ["private/plan.json"]}),
                    "native-create-call",
                ),
                settings_text_reply("Native permission check completed."),
            ],
        );
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(connections, 2);
        let result = recorded_tool_result(&requests);
        assert_eq!(
            result["status"].as_str(),
            Some(if section == "deny" {
                "denied"
            } else {
                "approval_required"
            }),
            "{result}"
        );
        if section == "deny" {
            assert!(result["error"]
                .as_str()
                .unwrap()
                .contains("Read(./private/**)"));
        } else {
            assert_eq!(result["data"]["approvalClass"], "settings-permission");
        }
        let inspect = settings_security_command(&root, support::jani())
            .args([
                "native", "planner", "inspect", "--tenant", "team-1", "--task", "task-1",
            ])
            .output()
            .unwrap();
        assert!(!inspect.status.success(), "private plan was created");
        for request in &requests {
            assert!(!request.body.to_string().contains("native-private-plan"));
        }
        fs::remove_dir_all(root).unwrap();
    }

    for section in ["deny", "ask"] {
        let (listener, base) = settings_mock_listener();
        let mut settings = json!({"version": 1});
        settings["permissions"] = json!({section: ["Read(./skills/private/**)"]});
        let root = settings_workspace(&settings.to_string());
        fs::create_dir_all(root.join("skills/private")).unwrap();
        fs::write(
            root.join("skills/private/SKILL.md"),
            "---\nname: private-skill\ndescription: private fixture\n---\nskill-private-instructions\n",
        )
        .unwrap();
        let mut command = settings_security_command(&root, support::jani());
        command.args([
            "--print",
            "read the private skill",
            "--api-base",
            &base,
            "--tenant",
            "team-1",
            "--task",
            "task-1",
        ]);
        let (output, connections, requests) = run_settings_mock(
            command,
            listener,
            vec![
                tool_call_reply(
                    "skill_read",
                    json!({"name": "private-skill"}),
                    "private-skill-read",
                ),
                settings_text_reply("Skill permission check completed."),
            ],
        );
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(connections, 2);
        let result = recorded_tool_result(&requests);
        assert_eq!(
            result["status"].as_str(),
            Some(if section == "deny" {
                "denied"
            } else {
                "approval_required"
            }),
            "{result}"
        );
        for request in &requests {
            assert!(!request
                .body
                .to_string()
                .contains("skill-private-instructions"));
        }
        fs::remove_dir_all(root).unwrap();
    }

    let (listener, base) = settings_mock_listener();
    let root = settings_workspace(r#"{"version":1,"permissions":{"deny":["Read(./private/**)"]}}"#);
    fs::create_dir_all(root.join("private")).unwrap();
    fs::write(
        root.join("private/secret.rs"),
        "pub fn native_private_symbol() {}\n// native-private-source-body\n",
    )
    .unwrap();
    let setup = settings_security_command(&root, support::jani())
        .args([
            "native", "index", "build", "--tenant", "team-1", "--task", "task-1",
        ])
        .output()
        .unwrap();
    assert!(
        setup.status.success(),
        "{}",
        String::from_utf8_lossy(&setup.stderr)
    );
    let mut command = settings_security_command(&root, support::jani());
    command.args([
        "--print",
        "search the existing index",
        "--api-base",
        &base,
        "--tenant",
        "team-1",
        "--task",
        "task-1",
    ]);
    let (output, connections, requests) = run_settings_mock(
        command,
        listener,
        vec![
            tool_call_reply(
                "native_tool",
                json!({
                    "tool": "index", "operation": "search",
                    "args": ["native_private_symbol"]
                }),
                "native-private-search",
            ),
            settings_text_reply("Filtered index search completed."),
        ],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(connections, 2);
    let result = recorded_tool_result(&requests);
    assert_eq!(result["data"]["totalMatches"], 0, "{result}");
    assert!(!result.to_string().contains("private/secret.rs"));
    for request in &requests {
        assert!(!request
            .body
            .to_string()
            .contains("native-private-source-body"));
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn smart_fixed_list_builds_require_consent_without_a_sandbox_end_to_end() {
    for subcommand in ["test", "check", "clippy"] {
        let (listener, base) = settings_mock_listener();
        let root = empty_workspace();
        let mut command = settings_security_command(&root, support::jani());
        command.args([
            "--print",
            "run the build",
            "--api-base",
            &base,
            "--exec",
            "smart",
        ]);
        let tool_reply = (200, json!({
            "choices": [{"message": {"role": "assistant", "content": null, "tool_calls": [{
                "id": "unconfined-build", "type": "function", "function": {
                    "name": "command", "arguments": json!({"argv": ["cargo", subcommand], "timeout_secs": 10}).to_string()
                }
            }]}}],
            "usage": {"prompt_tokens": 10, "completion_tokens": 4}
        }).to_string());
        let (output, connections, requests) = run_settings_mock(
            command,
            listener,
            vec![
                tool_reply,
                settings_text_reply("Build requires operator consent."),
            ],
        );
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(connections, 2);
        let tool = requests[1].body["messages"]
            .as_array()
            .unwrap()
            .iter()
            .find(|message| message["role"] == "tool")
            .unwrap();
        let result: Value = serde_json::from_str(tool["content"].as_str().unwrap()).unwrap();
        assert_eq!(result["status"], "denied", "{subcommand}: {result}");
        assert!(
            result["error"]
                .as_str()
                .unwrap()
                .contains("no sandbox confines"),
            "{result}"
        );
        assert!(!root.join("target").exists());
        fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn ambiguous_api_urls_are_rejected_by_the_real_cli() {
    let root = empty_workspace();
    for base in [
        "http://localhost:80@attacker.example/v1",
        "https://user:password@provider.example/v1",
        "http://localhost:80\\@attacker.example/v1",
        "http://localhost:bad/v1",
        "http://[::1]:65536/v1",
    ] {
        let output = settings_security_command(&root, support::jani())
            .args(["doctor", "--api-base", base])
            .output()
            .unwrap();
        assert!(!output.status.success(), "{base}");
        assert_no_dummy_keys(&output);
        assert!(String::from_utf8_lossy(&output.stderr).contains("API base"));
    }
    assert!(!root.join("state").exists());
    fs::remove_dir_all(root).unwrap();
}
