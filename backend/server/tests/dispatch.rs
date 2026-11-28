//! CLI intake contracts against durable task state and a loopback model fixture.
use jani_core::{kill, types::new_id};
use jani_store::tasks::TaskStore;
use serde_json::{json, Value};
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

mod support;
use support::provider::{say, start, tool, Scripted};

const TENANT: &str = "acme";
const TASK: &str = "task-intake";
const PRINCIPAL: &str = "bot-1";
const PURPOSE: &str = "SOFTWARE_DEV";

struct Fixture {
    root: PathBuf,
    state: PathBuf,
    event: PathBuf,
    contract: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(new_id("jani-dispatch"));
        fs::create_dir_all(&root).unwrap();
        let fixture = Self {
            state: root.join("state"),
            event: root.join("event.json"),
            contract: root.join("contract.json"),
            root,
        };
        fixture.write_contract(TASK, PURPOSE, &["repo.read"]);
        fixture.write_event("delivery-1", "repo#42", "inspect this change");
        fixture
    }

    fn write_contract(&self, task: &str, purpose: &str, capabilities: &[&str]) {
        fs::write(
            &self.contract,
            json!({
                "apiVersion": "jani.dev/v1alpha1", "kind": "WorkContract",
                "metadata": {"taskId": task, "tenant": TENANT, "principal": PRINCIPAL},
                "spec": {"purpose": purpose, "capabilities": capabilities},
            })
            .to_string(),
        )
        .unwrap();
    }

    fn write_event(&self, delivery: &str, reference: &str, prompt: &str) {
        fs::write(
            &self.event,
            json!({
                "source": "github", "event": "pull_request.opened", "reference": reference,
                "deliveryId": delivery, "prompt": prompt,
            })
            .to_string(),
        )
        .unwrap();
    }

    fn command(&self, model: &Scripted, args: &[&str]) -> Command {
        let mut command = support::jani();
        command
            .args(args)
            .arg("--repo")
            .arg(&self.root)
            .arg("--state-dir")
            .arg(&self.state)
            .arg("--contract")
            .arg(&self.contract)
            .args([
                "--tenant",
                TENANT,
                "--principal",
                PRINCIPAL,
                "--purpose",
                PURPOSE,
            ])
            .env("JANI_API_BASE", &model.base)
            .env("JANI_API_KEY", "dispatch-test-key")
            .env("JANI_MODEL", "scripted")
            .env_remove("OPENAI_API_KEY")
            .env_remove("JANI_POLICY")
            .env_remove("JANI_IDENTITY");
        if args.first() == Some(&"dispatch") {
            command.arg("--event").arg(&self.event);
        }
        command
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn stdout(output: Output) -> String {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

fn refused(output: Output, expected: &str) {
    assert!(
        !output.status.success(),
        "unexpected success: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(expected),
        "expected {expected:?}; received {stderr}"
    );
}

#[test]
fn preview_is_read_only_and_completed_duplicates_preserve_the_checkpoint() {
    let f = Fixture::new();
    let model = start(vec![say("one dispatch answer")]);
    let secret = format!("ghp_{}", "A1b2C3d4E5".repeat(4));
    f.write_event("delivery-1", &secret, &format!("inspect {secret}"));
    let mut preview = f.command(&model, &["dispatch", "dry-run"]);
    preview.env_remove("JANI_API_KEY");
    let preview: Value = serde_json::from_str(&stdout(preview.output().unwrap())).unwrap();
    assert_eq!(preview["dryRun"], true);
    assert_eq!(preview["queued"], false);
    assert!(preview["promptPreview"]
        .as_str()
        .unwrap()
        .contains("[REDACTED"));
    assert!(!preview.to_string().contains(&secret));
    assert!(!f.state.join("tasks").exists());
    assert!(model.requests.lock().unwrap().is_empty());

    assert!(
        stdout(f.command(&model, &["dispatch"]).output().unwrap()).contains("one dispatch answer")
    );
    let store = TaskStore::open(&f.state).unwrap();
    let snapshot = || serde_json::to_value(store.load(TENANT, TASK).unwrap().unwrap()).unwrap();
    let completed = snapshot();
    assert_eq!(completed["status"], "completed");
    let duplicate: Value =
        serde_json::from_str(&stdout(f.command(&model, &["dispatch"]).output().unwrap())).unwrap();
    assert_eq!(duplicate["deduplicated"], true);
    assert_eq!(duplicate["idempotencyKey"], preview["idempotencyKey"]);
    assert_eq!(duplicate["cached"]["turnStatus"], "completed");
    assert_eq!(snapshot(), completed);
    let requests = model.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert!(!requests[0].to_string().contains(&secret));
}

#[test]
fn invalid_arguments_and_contract_leave_no_claim() {
    let f = Fixture::new();
    let model = start(vec![]);
    refused(
        f.command(&model, &["dispatch", "typo"]).output().unwrap(),
        "only the 'dry-run'",
    );
    refused(
        f.command(&model, &["dispatch"])
            .arg("dry-run")
            .output()
            .unwrap(),
        "unexpected dispatch arguments",
    );
    f.write_contract(TASK, "WRONG_PURPOSE", &[]);
    refused(
        f.command(&model, &["dispatch"]).output().unwrap(),
        "does not match the requested purpose",
    );
    f.write_contract(TASK, PURPOSE, &[]);
    fs::write(&f.event, vec![b' '; 256 * 1024 + 1]).unwrap();
    refused(
        f.command(&model, &["dispatch"]).output().unwrap(),
        "event exceeds 262144 bytes",
    );
    f.write_event("delivery-1", "repo#42", "inspect this change");
    kill::activate(&f.state, Some(TENANT), "safety-operator", "test stop").unwrap();
    refused(
        f.command(&model, &["dispatch"]).output().unwrap(),
        "KILL_SWITCH_ACTIVE",
    );
    assert!(!f.state.join("tasks").exists());
    assert!(model.requests.lock().unwrap().is_empty());
}

#[test]
fn startup_failure_can_be_retried_before_the_agent_turn() {
    let f = Fixture::new();
    let model = start(vec![say("ran after startup was fixed")]);
    let obstruction = f.state.join("journal.jsonl");
    fs::create_dir_all(&obstruction).unwrap();
    refused(
        f.command(&model, &["dispatch"]).output().unwrap(),
        "startup failed; delivery was released",
    );
    assert!(TaskStore::open(&f.state)
        .unwrap()
        .load(TENANT, TASK)
        .unwrap()
        .is_none());
    assert!(model.requests.lock().unwrap().is_empty());
    fs::remove_dir(obstruction).unwrap();
    assert!(stdout(f.command(&model, &["dispatch"]).output().unwrap())
        .contains("ran after startup was fixed"));
    assert_eq!(model.requests.lock().unwrap().len(), 1);
}

#[test]
fn changed_event_or_assignment_never_replays_the_delivery() {
    let f = Fixture::new();
    let model = start(vec![say("done")]);
    stdout(f.command(&model, &["dispatch"]).output().unwrap());
    f.write_event("delivery-1", "repo#99", "inspect this change");
    refused(
        f.command(&model, &["dispatch"]).output().unwrap(),
        "different request or assignment",
    );
    f.write_event("delivery-1", "repo#42", "inspect this change");
    f.write_contract(TASK, PURPOSE, &[]);
    refused(
        f.command(&model, &["dispatch"]).output().unwrap(),
        "different request or assignment",
    );
    f.write_contract("other-task", PURPOSE, &["repo.read"]);
    refused(
        f.command(&model, &["dispatch"]).output().unwrap(),
        "different request or assignment",
    );
    f.write_event("delivery-2", "repo#42", "inspect this change");
    f.write_contract(TASK, PURPOSE, &["repo.read"]);
    refused(
        f.command(&model, &["dispatch"]).output().unwrap(),
        "already has a checkpoint",
    );
    assert!(TaskStore::open(&f.state)
        .unwrap()
        .load(TENANT, "other-task")
        .unwrap()
        .is_none());
    assert_eq!(model.requests.lock().unwrap().len(), 1);
}

#[test]
fn an_unsuccessful_turn_remains_owned_and_is_never_launched_twice() {
    let f = Fixture::new();
    let model = start(vec![say("")]);
    let first = f
        .command(&model, &["dispatch"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let second = f.command(&model, &["dispatch"]).output().unwrap();
    refused(first.wait_with_output().unwrap(), "requires reconciliation");
    refused(second, "requires reconciliation");
    refused(
        f.command(&model, &["dispatch"]).output().unwrap(),
        "requires reconciliation",
    );
    refused(
        f.command(&model, &["resume", "--task", TASK])
            .output()
            .unwrap(),
        "requires reconciliation",
    );
    refused(
        f.command(&model, &["run", "run a second turn"])
            .output()
            .unwrap(),
        "assigned to an intake delivery",
    );
    assert_eq!(model.requests.lock().unwrap().len(), 1);
}

#[test]
fn duplicate_returns_waiting_input_and_an_operator_can_resume_the_task() {
    let f = Fixture::new();
    f.write_contract(TASK, PURPOSE, &["agent.ask"]);
    let model = start(vec![
        tool(
            1,
            "ask",
            json!({"question": "Which value should I use?", "options": ["first", "second"]}),
        ),
        say("continued"),
    ]);
    let first: Value =
        serde_json::from_str(&stdout(f.command(&model, &["dispatch"]).output().unwrap())).unwrap();
    assert_eq!(first["turnStatus"], "waiting_input");
    assert!(first["answer"].as_str().unwrap().contains("Which value"));
    let duplicate: Value =
        serde_json::from_str(&stdout(f.command(&model, &["dispatch"]).output().unwrap())).unwrap();
    assert_eq!(duplicate["cached"]["turnStatus"], "waiting_input");
    assert_eq!(model.requests.lock().unwrap().len(), 1);
    let mut store = TaskStore::open(&f.state).unwrap();
    let checkpoint = store.claim_checkpoint(TENANT, TASK).unwrap().unwrap();
    assert_eq!(checkpoint.status, "waiting_input");
    assert_eq!(checkpoint.epoch, 2);
}

#[test]
fn legacy_preview_is_reported_as_requiring_reconciliation() {
    let f = Fixture::new();
    let model = start(vec![]);
    let legacy_key = "intake:github:old\0:reference:delivery-1";
    let mut store = TaskStore::open(&f.state).unwrap();
    store
        .begin_effect(
            TENANT,
            "legacy-task",
            1,
            legacy_key,
            "intake.dispatch",
            "old:reference",
            "legacy",
        )
        .unwrap();
    store
        .finish_effect(
            TENANT,
            "legacy-task",
            legacy_key,
            true,
            Some(&json!({"queued": true, "event": "pull_request.opened"})),
        )
        .unwrap();
    refused(
        f.command(&model, &["dispatch"]).output().unwrap(),
        "legacy delivery requires reconciliation",
    );
    assert!(store.load(TENANT, TASK).unwrap().is_none());
    assert!(model.requests.lock().unwrap().is_empty());
}
