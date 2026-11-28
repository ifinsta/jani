//! The work queue, saved conversations and project memory, against real
//! processes: a worker that dies mid-task, a conversation that comes back, a
//! note that is scrubbed before it is kept.

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

mod support;
use support::provider::{say, start, tool, write, Scripted};

const BUGGY: &str = "def add(a, b):\n    return a - b\n";
const WRONG: &str = "def add(a, b):\n    return a * b\n";
const FIXED: &str = "def add(a, b):\n    return a + b\n";
const TEST: &str = "import unittest\nfrom calc import add\n\nclass T(unittest.TestCase):\n    def test_add(self):\n        self.assertEqual(add(2, 3), 5)\n";

fn sh(dir: &Path, program: &str, args: &[&str]) -> String {
    let out = Command::new(program)
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{program} {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn fixture() -> PathBuf {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("jani-work-{}-{stamp:x}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("calc.py"), BUGGY).unwrap();
    std::fs::write(dir.join("test_calc.py"), TEST).unwrap();
    std::fs::write(dir.join(".gitignore"), ".jani/\n__pycache__/\n").unwrap();
    sh(&dir, "git", &["init", "-q", "-b", "main"]);
    sh(&dir, "git", &["add", "-A"]);
    sh(
        &dir,
        "git",
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-qm",
            "initial",
        ],
    );
    dir
}

fn jani(dir: &Path, model: Option<&Scripted>, args: &[&str]) -> Command {
    let mut c = support::jani();
    c.args(args)
        .arg("--repo")
        .arg(dir)
        .arg("--state-dir")
        .arg(dir.join(".jani"))
        .env("JANI_API_KEY", "test-key")
        .env("JANI_MODEL", "scripted")
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .env_remove("OPENAI_API_KEY")
        .env_remove("JANI_POLICY")
        .env_remove("JANI_IDENTITY")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(model) = model {
        c.env("JANI_API_BASE", &model.base);
    }
    c
}

const SCOPE: [&str; 5] = [
    "--allow-write",
    "--write-path",
    "calc.py",
    "--write-path",
    "test_calc.py",
];

fn json_out(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|_| {
        panic!(
            "not json: {} / {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

fn add(dir: &Path, prompt: &str, extra: &[&str]) -> String {
    let out = jani(
        dir,
        None,
        &["work", "add", prompt, "--accept", "python3 -m unittest -q"],
    )
    .args(extra)
    .output()
    .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    json_out(&out)["id"].as_str().unwrap().to_string()
}

fn show(dir: &Path, id: &str) -> Value {
    json_out(&jani(dir, None, &["work", "show", id]).output().unwrap())
}

fn wait_until(what: &str, secs: u64, mut ok: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < end {
        if ok() {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("timed out waiting for {what}");
}

fn install_acceptance_barrier(dir: &Path) {
    std::fs::write(
        dir.join("hold_acceptance.py"),
        r#"from pathlib import Path
import time

Path("check-started").touch()
deadline = time.monotonic() + 60
while not Path("release-check").exists():
    if time.monotonic() >= deadline:
        raise TimeoutError("test did not release the acceptance check")
    time.sleep(0.05)
"#,
    )
    .unwrap();
    sh(dir, "git", &["add", "hold_acceptance.py"]);
    sh(
        dir,
        "git",
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-qm",
            "add acceptance barrier",
        ],
    );
}

fn restore_expired_claim(dir: &Path, id: &str, finished: &Value) {
    let mut task = finished.clone();
    task["status"] = json!("running");
    task["owner"] = json!("interrupted-worker");
    task["leaseUntilMillis"] = json!(0);
    task["outcome"] = Value::Null;
    task["report"] = Value::Null;
    task["branch"] = Value::Null;
    task["commit"] = Value::Null;
    std::fs::write(
        dir.join(".jani/work/tasks").join(format!("{id}.json")),
        serde_json::to_vec(&task).unwrap(),
    )
    .unwrap();
}

#[test]
fn a_queue_with_damaged_records_still_runs_healthy_tasks_in_order() {
    let dir = fixture();
    let broken = add(&dir, "unreadable", &[]);
    let invalid_check = add(&dir, "bad acceptance", &[]);
    let first = add(&dir, "first healthy task", &[]);
    let second = add(&dir, "second healthy task", &[]);
    let records = dir.join(".jani/work/tasks");
    let broken_path = records.join(format!("{broken}.json"));
    std::fs::write(&broken_path, "{invalid JSON").unwrap();
    let invalid_path = records.join(format!("{invalid_check}.json"));
    let mut invalid = show(&dir, &invalid_check);
    invalid["accept"] = json!(["python 'unfinished"]);
    std::fs::write(invalid_path, serde_json::to_vec(&invalid).unwrap()).unwrap();

    let provider = start(vec![
        write(0, "calc.py", FIXED),
        say("First complete."),
        write(0, "calc.py", FIXED),
        say("Second complete."),
    ]);
    let result = jani(&dir, Some(&provider), &["work", "run"])
        .args(SCOPE)
        .output()
        .unwrap();
    assert_eq!(result.status.code(), Some(1));
    let diagnostics = String::from_utf8_lossy(&result.stderr);
    assert_eq!(diagnostics.matches(&broken).count(), 1, "{diagnostics}");
    assert_eq!(
        std::fs::read_to_string(broken_path).unwrap(),
        "{invalid JSON"
    );
    let summaries: Vec<Value> = String::from_utf8(result.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert!(summaries.len() >= 2, "{diagnostics}");
    assert_eq!(summaries[summaries.len() - 2]["id"], first);
    assert_eq!(summaries[summaries.len() - 1]["id"], second);
    assert_eq!(show(&dir, &first)["outcome"], "verified");
    assert_eq!(show(&dir, &second)["outcome"], "verified");
    assert_eq!(show(&dir, &invalid_check)["status"], "failed");
    assert_eq!(provider.requests.lock().unwrap().len(), 4);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_bad_saved_acceptance_check_does_not_consume_the_once_slot() {
    let dir = fixture();
    let bad = add(&dir, "invalid check", &[]);
    let healthy = add(&dir, "make add() correct", &[]);
    let path = dir.join(".jani/work/tasks").join(format!("{bad}.json"));
    let mut invalid = show(&dir, &bad);
    invalid["accept"] = json!(["\"\""]);
    std::fs::write(path, serde_json::to_vec(&invalid).unwrap()).unwrap();

    let provider = start(vec![write(0, "calc.py", FIXED), say("Fixed.")]);
    let output = jani(&dir, Some(&provider), &["work", "run", "--once"])
        .args(SCOPE)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(json_out(&output)["id"], healthy);
    let rejected = show(&dir, &bad);
    assert_eq!(rejected["status"], "failed");
    assert_eq!(rejected["error"], "invalid acceptance check 1");
    assert_eq!(rejected["attempts"], 0);
    assert_eq!(show(&dir, &healthy)["outcome"], "verified");
    assert_eq!(provider.requests.lock().unwrap().len(), 2);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_completion_write_failure_cannot_stop_later_tasks() {
    let dir = fixture();
    install_acceptance_barrier(&dir);
    let first = add(
        &dir,
        "first task",
        &["--accept", "python3 hold_acceptance.py"],
    );
    let second = add(&dir, "second task", &[]);
    let suffix = first.strip_prefix("wrk-").unwrap();
    let delivery = dir.join(".jani/deliveries").join(format!("dlv-{suffix}"));
    let tree = delivery.join("tree");
    let provider = start(vec![
        write(0, "calc.py", FIXED),
        say("First complete."),
        write(0, "calc.py", FIXED),
        say("Second complete."),
    ]);
    let worker = jani(&dir, Some(&provider), &["work", "run"])
        .args(SCOPE)
        .args(["--sandbox", "off"])
        .env("JANI_WORK_LEASE_SECS", "120")
        .spawn()
        .unwrap();
    wait_until("first task to reach acceptance", 60, || {
        tree.join("check-started").is_file()
    });
    let staging = dir
        .join(".jani/work/tasks")
        .join(format!("{first}.json.tmp"));
    std::fs::create_dir(&staging).unwrap();
    std::fs::write(tree.join("release-check"), "").unwrap();
    let output = worker.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(1));
    let diagnostics = String::from_utf8_lossy(&output.stderr);
    assert!(
        diagnostics.contains(&format!("cannot write task {first}")),
        "{diagnostics}"
    );
    assert_eq!(json_out(&output)["id"], second);
    assert_eq!(show(&dir, &second)["outcome"], "verified");
    let unfinished = show(&dir, &first);
    assert_eq!(unfinished["status"], "running");
    let report: Value =
        serde_json::from_slice(&std::fs::read(delivery.join("report.json")).unwrap()).unwrap();
    assert_eq!(report["outcome"], "verified");
    assert_eq!(provider.requests.lock().unwrap().len(), 4);

    std::fs::remove_dir(staging).unwrap();
    restore_expired_claim(&dir, &first, &unfinished);
    let replacement = start(vec![]);
    let output = jani(&dir, Some(&replacement), &["work", "run", "--once"])
        .args(SCOPE)
        .args(["--sandbox", "off"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let recovered = show(&dir, &first);
    assert_eq!(recovered["outcome"], "verified", "{recovered}");
    assert_eq!(recovered["commit"], report["commit"]);
    assert!(replacement.requests.lock().unwrap().is_empty());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_watch_worker_keeps_polling_after_a_stable_record_fault() {
    let dir = fixture();
    let broken = add(&dir, "damaged task", &[]);
    std::fs::write(
        dir.join(".jani/work/tasks").join(format!("{broken}.json")),
        "{unreadable",
    )
    .unwrap();
    let provider = start(vec![
        write(0, "calc.py", FIXED),
        say("First complete."),
        write(0, "calc.py", FIXED),
        say("Second complete."),
    ]);
    let mut worker = jani(&dir, Some(&provider), &["work", "run", "--watch"])
        .args(SCOPE)
        .spawn()
        .unwrap();
    for prompt in ["first healthy task", "second healthy task"] {
        let id = add(&dir, prompt, &[]);
        wait_until(prompt, 60, || {
            assert!(
                worker.try_wait().unwrap().is_none(),
                "watch worker stopped early"
            );
            show(&dir, &id)["outcome"] == "verified"
        });
    }
    worker.kill().unwrap();
    let output = worker.wait_with_output().unwrap();
    let diagnostics = String::from_utf8_lossy(&output.stderr);
    assert_eq!(diagnostics.matches(&broken).count(), 1, "{diagnostics}");
    assert_eq!(provider.requests.lock().unwrap().len(), 4);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn an_expired_worker_recovers_its_commit_and_reports_without_repeating_acceptance() {
    let dir = fixture();
    let id = add(&dir, "make add() correct", &[]);
    let original = start(vec![write(0, "calc.py", FIXED), say("Fixed add.")]);
    let output = jani(&dir, Some(&original), &["work", "run", "--once"])
        .args(SCOPE)
        .output()
        .unwrap();
    assert!(output.status.success());
    let finished = show(&dir, &id);
    assert_eq!(finished["outcome"], "verified", "{finished}");
    let report = Path::new(finished["report"].as_str().unwrap());
    let original_report = std::fs::read(report.with_extension("json")).unwrap();
    std::fs::remove_file(report).unwrap();
    std::fs::remove_file(report.with_extension("json")).unwrap();
    restore_expired_claim(&dir, &id, &finished);
    let journal_path = dir.join(".jani/journal.jsonl");
    let before = std::fs::read_to_string(&journal_path).unwrap();
    let terminal = before
        .lines()
        .position(|line| line.contains("\"action\":\"delivery.run\""))
        .unwrap();
    let prefix = before.lines().take(terminal).collect::<Vec<_>>().join("\n") + "\n";
    std::fs::write(&journal_path, &prefix).unwrap();

    let retry = start(vec![say("There should be no new turn.")]);
    let output = jani(&dir, Some(&retry), &["work", "run", "--once"])
        .args(SCOPE)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(retry.requests.lock().unwrap().is_empty());
    let recovered = show(&dir, &id);
    assert_eq!(recovered["outcome"], finished["outcome"], "{recovered}");
    assert_eq!(recovered["commit"], finished["commit"]);
    assert_eq!(
        std::fs::read(report.with_extension("json")).unwrap(),
        original_report
    );
    let after = std::fs::read_to_string(journal_path).unwrap();
    assert_eq!(after.matches("\"action\":\"acceptance.run\"").count(), 1);
    assert_eq!(after.matches("\"action\":\"delivery.run\"").count(), 1);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_prepared_delivery_advances_once_after_lease_budget_exhaustion() {
    let dir = fixture();
    let id = add(&dir, "make add() correct", &[]);
    let original = start(vec![write(0, "calc.py", FIXED), say("Fixed add.")]);
    assert!(jani(&dir, Some(&original), &["work", "run", "--once"])
        .args(SCOPE)
        .output()
        .unwrap()
        .status
        .success());
    let mut finished = show(&dir, &id);
    let oid = finished["commit"].as_str().unwrap().to_string();
    let branch = finished["branch"].as_str().unwrap();
    let parent = sh(&dir, "git", &["rev-parse", &format!("{oid}^")]);
    let tree = Path::new(finished["report"].as_str().unwrap())
        .parent()
        .unwrap()
        .join("tree");
    sh(&tree, "git", &["reset", "--mixed", parent.trim()]);
    assert_eq!(sh(&tree, "git", &["rev-parse", "HEAD"]), parent);
    std::fs::write(tree.join("operator-note.txt"), "Keep this staged.").unwrap();
    sh(&tree, "git", &["add", "operator-note.txt"]);
    let journal = dir.join(".jani/journal.jsonl");
    let before = std::fs::read_to_string(&journal).unwrap();
    let terminal = before
        .lines()
        .position(|line| line.contains("\"action\":\"delivery.run\""))
        .unwrap();
    let prefix = before.lines().take(terminal).collect::<Vec<_>>().join("\n") + "\n";
    std::fs::write(&journal, prefix).unwrap();
    assert_eq!(sh(&dir, "git", &["rev-parse", branch]), parent);

    finished["interruptions"] = json!(2);
    restore_expired_claim(&dir, &id, &finished);
    let restricted = start(vec![]);
    let output = jani(&dir, Some(&restricted), &["work", "run", "--once"])
        .args(["--allow-write", "--write-path", "test_calc.py"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(restricted.requests.lock().unwrap().is_empty());
    let refused = show(&dir, &id);
    assert_eq!(refused["status"], "failed");
    assert_eq!(refused["interruptions"], 3);
    assert!(
        refused["error"]
            .as_str()
            .unwrap()
            .contains("worker authority"),
        "{refused}"
    );
    assert_eq!(sh(&tree, "git", &["rev-parse", "HEAD"]), parent);
    assert!(jani(&dir, None, &["work", "retry", &id])
        .output()
        .unwrap()
        .status
        .success());
    let replacement = start(vec![]);
    let output = jani(&dir, Some(&replacement), &["work", "run", "--once"])
        .args(SCOPE)
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(replacement.requests.lock().unwrap().is_empty());
    let result = show(&dir, &id);
    assert_eq!(result["outcome"], "verified", "{result}");
    assert_eq!(result["commit"], oid);
    assert_eq!(result["deliveryGeneration"], finished["deliveryGeneration"]);
    assert_eq!(
        sh(&tree, "git", &["diff", "--cached", "--name-only"]).trim(),
        "operator-note.txt"
    );
    let journal = std::fs::read_to_string(journal).unwrap();
    assert_eq!(journal.matches("\"action\":\"acceptance.run\"").count(), 1);
    assert_eq!(journal.matches("\"action\":\"delivery.run\"").count(), 1);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn retry_repairs_failed_report_publication_without_repeating_a_completed_delivery() {
    let dir = fixture();
    let id = add(&dir, "make add() correct", &[]);
    let suffix = id.strip_prefix("wrk-").unwrap();
    let report = dir
        .join(".jani/deliveries")
        .join(format!("dlv-{suffix}"))
        .join("report.md");
    std::fs::create_dir_all(&report).unwrap();
    let original = start(vec![write(0, "calc.py", FIXED), say("Fixed add.")]);
    assert!(jani(&dir, Some(&original), &["work", "run", "--once"])
        .args(SCOPE)
        .output()
        .unwrap()
        .status
        .success());
    let interrupted = show(&dir, &id);
    assert_eq!(interrupted["status"], "failed", "{interrupted}");
    assert!(
        interrupted["error"]
            .as_str()
            .unwrap()
            .contains("cannot publish"),
        "{interrupted}"
    );
    let oid = sh(&dir, "git", &["rev-parse", &format!("jani/{suffix}")]);
    std::fs::remove_dir(&report).unwrap();
    assert!(jani(&dir, None, &["work", "retry", &id])
        .output()
        .unwrap()
        .status
        .success());
    let replacement = start(vec![]);
    assert!(jani(&dir, Some(&replacement), &["work", "run", "--once"])
        .args(SCOPE)
        .output()
        .unwrap()
        .status
        .success());
    assert!(replacement.requests.lock().unwrap().is_empty());
    let result = show(&dir, &id);
    assert_eq!(result["outcome"], "verified", "{result}");
    assert_eq!(result["commit"], oid.trim());
    assert_eq!(
        result["deliveryGeneration"],
        interrupted["deliveryGeneration"]
    );
    assert!(report.is_file());
    let journal = std::fs::read_to_string(dir.join(".jani/journal.jsonl")).unwrap();
    assert_eq!(journal.matches("\"action\":\"delivery.run\"").count(), 1);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn retry_after_cancellation_recovers_an_already_published_commit() {
    let dir = fixture();
    let id = add(&dir, "make add() correct", &[]);
    let original = start(vec![write(0, "calc.py", FIXED), say("Fixed add.")]);
    assert!(jani(&dir, Some(&original), &["work", "run", "--once"])
        .args(SCOPE)
        .output()
        .unwrap()
        .status
        .success());
    let finished = show(&dir, &id);
    assert_eq!(finished["outcome"], "verified", "{finished}");
    let report = Path::new(finished["report"].as_str().unwrap());
    std::fs::remove_file(report).unwrap();
    restore_expired_claim(&dir, &id, &finished);
    let cancelled = jani(&dir, None, &["work", "cancel", &id]).output().unwrap();
    assert!(cancelled.status.success());
    assert_eq!(json_out(&cancelled)["status"], "cancelled");
    let queued = jani(&dir, None, &["work", "retry", &id]).output().unwrap();
    assert!(queued.status.success());
    assert_eq!(
        json_out(&queued)["deliveryGeneration"],
        finished["deliveryGeneration"]
    );
    let replacement = start(vec![]);
    assert!(jani(&dir, Some(&replacement), &["work", "run", "--once"])
        .args(SCOPE)
        .output()
        .unwrap()
        .status
        .success());
    assert!(replacement.requests.lock().unwrap().is_empty());
    let recovered = show(&dir, &id);
    assert_eq!(recovered["outcome"], "verified", "{recovered}");
    assert_eq!(recovered["commit"], finished["commit"]);
    assert!(report.is_file());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_worker_does_not_reexecute_a_delivery_with_missing_or_conflicting_evidence() {
    for missing in [false, true] {
        let dir = fixture();
        let id = add(&dir, "make add() correct", &[]);
        let original = start(vec![write(0, "calc.py", FIXED), say("Fixed add.")]);
        assert!(jani(&dir, Some(&original), &["work", "run", "--once"])
            .args(SCOPE)
            .output()
            .unwrap()
            .status
            .success());
        let mut finished = show(&dir, &id);
        assert_eq!(finished["outcome"], "verified", "{finished}");
        let report = PathBuf::from(finished["report"].as_str().unwrap());
        let original_report = std::fs::read(report.with_extension("json")).unwrap();
        let receipt = report.parent().unwrap().join("completion-0.json");
        if missing {
            std::fs::remove_file(receipt).unwrap();
            finished["interruptions"] = json!(2);
        } else {
            let mut prepared: Value =
                serde_json::from_slice(&std::fs::read(&receipt).unwrap()).unwrap();
            prepared["delivery"]["answer"] =
                json!("Update that did not come from the recorded run");
            std::fs::write(receipt, serde_json::to_vec(&prepared).unwrap()).unwrap();
        }
        restore_expired_claim(&dir, &id, &finished);
        let replacement = start(vec![]);
        assert!(jani(&dir, Some(&replacement), &["work", "run", "--once"])
            .args(SCOPE)
            .output()
            .unwrap()
            .status
            .success());
        assert!(replacement.requests.lock().unwrap().is_empty());
        let result = show(&dir, &id);
        assert_eq!(result["status"], "failed", "{result}");
        let expected = if missing {
            assert_eq!(result["interruptions"], 3);
            "finalization file is missing"
        } else {
            "audit evidence conflicts"
        };
        assert!(
            result["error"].as_str().unwrap().contains(expected),
            "{result}"
        );
        assert_eq!(
            std::fs::read(report.with_extension("json")).unwrap(),
            original_report
        );
        if missing {
            assert!(jani(&dir, None, &["work", "retry", &id])
                .output()
                .unwrap()
                .status
                .success());
            assert_eq!(
                show(&dir, &id)["deliveryGeneration"],
                finished["deliveryGeneration"]
            );
        }
        let _ = std::fs::remove_dir_all(dir);
    }
}

#[test]
fn a_worker_runs_queued_tasks_in_order_and_records_each_outcome() {
    let dir = fixture();
    let good = add(&dir, "make add() correct", &[]);
    let bad = add(&dir, "make add() correct, again", &["--repair-rounds", "1"]);
    // Task 1 is fixed on the first try. Task 2's model only claims success.
    let model = start(vec![
        write(0, "calc.py", FIXED),
        say("Fixed."),
        say("It is fixed, trust me."),
        say("Still fixed."),
    ]);
    let out = jani(&dir, Some(&model), &["work", "run"])
        .args(SCOPE)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let first = show(&dir, &good);
    assert_eq!(
        (first["status"].as_str(), first["outcome"].as_str()),
        (Some("done"), Some("verified")),
        "{first}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let branch = first["branch"].as_str().unwrap();
    assert_eq!(
        sh(&dir, "git", &["show", &format!("{branch}:calc.py")]),
        FIXED
    );
    let second = show(&dir, &bad);
    assert_eq!(
        (second["status"].as_str(), second["outcome"].as_str()),
        (Some("failed"), Some("failed"))
    );
    assert!(
        second["error"].as_str().unwrap().contains("acceptance"),
        "{second}"
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("calc.py")).unwrap(),
        BUGGY,
        "the checkout is never touched"
    );

    // Authority was the worker's flags, not anything the queue said, and the queue is inspectable.
    let list = json_out(&jani(&dir, None, &["work", "list"]).output().unwrap());
    assert_eq!(list.as_array().unwrap().len(), 2);
    assert!(first["history"]
        .as_array()
        .unwrap()
        .iter()
        .any(|h| h["event"].as_str().unwrap().starts_with("claimed by")));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn work_add_preserves_an_explicit_repair_limit_equal_to_the_default() {
    let dir = fixture();
    let default = jani_core::config::defaults::REPAIR_ROUNDS;
    let inherited = add(&dir, "use the worker limit", &[]);
    let fixed = add(
        &dir,
        "use the task limit",
        &["--repair-rounds", &default.to_string()],
    );
    assert!(show(&dir, &inherited)["repairRounds"].is_null());
    assert_eq!(show(&dir, &fixed)["repairRounds"], default);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn an_operator_retry_resumes_the_existing_worktree_and_resets_the_attempt_budget() {
    let dir = fixture();
    let id = add(&dir, "fix add", &["--repair-rounds", "0"]);
    let first = start(vec![say("No change.")]);
    let out = jani(&dir, Some(&first), &["work", "run", "--once"])
        .args(SCOPE)
        .output()
        .unwrap();
    assert!(out.status.success());
    let failed = show(&dir, &id);
    assert_eq!(failed["status"], "failed", "{failed}");
    let branch = failed["branch"].as_str().unwrap();

    let out = jani(&dir, None, &["work", "retry", &id]).output().unwrap();
    assert!(out.status.success());
    assert_eq!(json_out(&out)["attempts"], 0);
    let retry = start(vec![write(0, "calc.py", FIXED), say("Fixed.")]);
    let out = jani(&dir, Some(&retry), &["work", "run", "--once"])
        .args(SCOPE)
        .output()
        .unwrap();
    assert!(out.status.success());
    let finished = show(&dir, &id);
    assert_eq!(finished["outcome"], "verified", "{finished}");
    assert_eq!(finished["attempts"], 1);
    assert_eq!(finished["branch"], branch);
    assert_eq!(
        sh(&dir, "git", &["show", &format!("{branch}:calc.py")]),
        FIXED
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_retry_recovers_a_registered_worktree_without_workspace_metadata() {
    let dir = fixture();
    let id = add(
        &dir,
        "finish the existing checkout",
        &["--repair-rounds", "0"],
    );
    let suffix = id.strip_prefix("wrk-").unwrap();
    let branch = format!("jani/{suffix}");
    let delivery = dir.join(".jani/deliveries").join(format!("dlv-{suffix}"));
    let tree = delivery.join("tree");
    sh(
        &dir,
        "git",
        &[
            "worktree",
            "add",
            "-b",
            &branch,
            tree.to_str().unwrap(),
            "HEAD",
        ],
    );
    let initial = start(vec![]);
    let out = jani(&dir, Some(&initial), &["work", "run", "--once"])
        .args(SCOPE)
        .output()
        .unwrap();
    assert!(out.status.success());
    assert_eq!(show(&dir, &id)["status"], "failed");
    assert!(initial.requests.lock().unwrap().is_empty());
    assert!(!delivery.join("workspace.json").exists());

    assert!(jani(&dir, None, &["work", "retry", &id])
        .output()
        .unwrap()
        .status
        .success());
    let retry = start(vec![
        write(0, "calc.py", FIXED),
        say("Fixed in the recovered checkout."),
    ]);
    let out = jani(&dir, Some(&retry), &["work", "run", "--once"])
        .args(SCOPE)
        .output()
        .unwrap();
    assert!(out.status.success());
    let finished = show(&dir, &id);
    assert_eq!(finished["outcome"], "verified", "{finished}");
    assert_eq!(finished["branch"], branch);
    assert_eq!(
        sh(&dir, "git", &["show", &format!("{branch}:calc.py")]),
        FIXED
    );
    let metadata: Value =
        serde_json::from_slice(&std::fs::read(delivery.join("workspace.json")).unwrap()).unwrap();
    assert!(metadata["base"].is_null());
    assert_eq!(std::fs::read_to_string(dir.join("calc.py")).unwrap(), BUGGY);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_retry_preserves_the_starting_commit_recorded_before_checkout() {
    let dir = fixture();
    let id = add(&dir, "fix add", &["--repair-rounds", "0"]);
    let suffix = id.strip_prefix("wrk-").unwrap();
    let branch = format!("jani/{suffix}");
    let base = sh(&dir, "git", &["rev-parse", "HEAD"]);
    sh(&dir, "git", &["branch", &branch]);
    let initial = start(vec![]);
    let out = jani(&dir, Some(&initial), &["work", "run", "--once"])
        .args(SCOPE)
        .output()
        .unwrap();
    assert!(out.status.success());
    assert_eq!(show(&dir, &id)["status"], "failed");
    let delivery = dir.join(".jani/deliveries").join(format!("dlv-{suffix}"));
    let metadata: Value =
        serde_json::from_slice(&std::fs::read(delivery.join("workspace.json")).unwrap()).unwrap();
    assert_eq!(metadata["base"], base.trim());
    assert_eq!(metadata["pending"], true);
    assert!(!delivery.join("tree").exists());

    sh(&dir, "git", &["branch", "-d", &branch]);
    sh(
        &dir,
        "git",
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "--allow-empty",
            "-qm",
            "advance main during interruption",
        ],
    );
    assert_ne!(sh(&dir, "git", &["rev-parse", "HEAD"]), base);
    assert!(jani(&dir, None, &["work", "retry", &id])
        .output()
        .unwrap()
        .status
        .success());
    let retry = start(vec![write(0, "calc.py", FIXED), say("Fixed.")]);
    let out = jani(&dir, Some(&retry), &["work", "run", "--once"])
        .args(SCOPE)
        .output()
        .unwrap();
    assert!(out.status.success());
    let finished = show(&dir, &id);
    assert_eq!(finished["outcome"], "verified", "{finished}");
    assert_eq!(sh(&dir, "git", &["rev-parse", &format!("{branch}^")]), base);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_retry_refuses_a_checkout_registered_for_a_different_branch() {
    let dir = fixture();
    let id = add(&dir, "fix add", &["--repair-rounds", "0"]);
    let suffix = id.strip_prefix("wrk-").unwrap();
    let tree = dir
        .join(".jani/deliveries")
        .join(format!("dlv-{suffix}"))
        .join("tree");
    sh(
        &dir,
        "git",
        &[
            "worktree",
            "add",
            "-b",
            "operator-review",
            tree.to_str().unwrap(),
            "HEAD",
        ],
    );
    let model = start(vec![]);
    assert!(jani(&dir, Some(&model), &["work", "run", "--once"])
        .args(SCOPE)
        .output()
        .unwrap()
        .status
        .success());
    assert!(jani(&dir, None, &["work", "retry", &id])
        .output()
        .unwrap()
        .status
        .success());
    assert!(jani(&dir, Some(&model), &["work", "run", "--once"])
        .args(SCOPE)
        .output()
        .unwrap()
        .status
        .success());
    let failed = show(&dir, &id);
    assert_eq!(failed["status"], "failed");
    assert!(
        failed["error"]
            .as_str()
            .unwrap()
            .contains("not the registered worktree"),
        "{failed}"
    );
    assert!(model.requests.lock().unwrap().is_empty());
    assert_eq!(
        sh(&tree, "git", &["branch", "--show-current"]).trim(),
        "operator-review"
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_retry_checks_git_identity_at_the_actual_nested_working_directory() {
    let dir = fixture();
    let scope = dir.join("nested");
    std::fs::create_dir(&scope).unwrap();
    for file in ["calc.py", "test_calc.py"] {
        std::fs::rename(dir.join(file), scope.join(file)).unwrap();
    }
    sh(&dir, "git", &["add", "-A"]);
    sh(
        &dir,
        "git",
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-qm",
            "move the package",
        ],
    );
    let id = add(&dir, "fix add", &["--repair-rounds", "0"]);
    let suffix = id.strip_prefix("wrk-").unwrap();
    let run = |model: &Scripted| {
        jani(&dir, Some(model), &["work", "run", "--once"])
            .arg("--repo")
            .arg(&scope)
            .args(SCOPE)
            .output()
            .unwrap()
    };

    assert!(run(&start(vec![say("No fix yet.")])).status.success());
    let first = show(&dir, &id);
    assert_eq!(first["outcome"], "failed", "{first}");
    let nested_tree = dir
        .join(".jani/deliveries")
        .join(format!("dlv-{suffix}"))
        .join("tree/nested");
    sh(&nested_tree, "git", &["init", "-q", "-b", "independent"]);

    assert!(jani(&dir, None, &["work", "retry", &id])
        .output()
        .unwrap()
        .status
        .success());
    let model = start(vec![]);
    assert!(run(&model).status.success());
    let failed = show(&dir, &id);
    assert!(
        failed["error"]
            .as_str()
            .unwrap()
            .contains("no longer belongs"),
        "{failed}"
    );
    assert!(model.requests.lock().unwrap().is_empty());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn cancelling_during_acceptance_stops_later_checks_and_leaves_the_branch_uncommitted() {
    let dir = fixture();
    install_acceptance_barrier(&dir);
    let out = jani(
        &dir,
        None,
        &[
            "work",
            "add",
            "fix add",
            "--accept",
            "python3 hold_acceptance.py",
            "--accept",
            "python3 -c \"from pathlib import Path; Path('second-check').touch()\"",
        ],
    )
    .output()
    .unwrap();
    assert!(out.status.success());
    let task = json_out(&out);
    let id = task["id"].as_str().unwrap();
    let suffix = id.strip_prefix("wrk-").unwrap();
    let delivery = dir.join(".jani/deliveries").join(format!("dlv-{suffix}"));
    let tree = delivery.join("tree");
    let model = start(vec![write(0, "calc.py", FIXED), say("Fixed.")]);
    let worker = jani(&dir, Some(&model), &["work", "run", "--once"])
        .args(SCOPE)
        .args(["--sandbox", "off"])
        .env("JANI_WORK_LEASE_SECS", "2")
        .spawn()
        .unwrap();
    wait_until("acceptance check to start", 60, || {
        tree.join("check-started").is_file()
    });
    assert_eq!(
        std::fs::read_to_string(tree.join("calc.py")).unwrap(),
        FIXED
    );
    let out = jani(&dir, None, &["work", "cancel", id]).output().unwrap();
    assert!(out.status.success());
    // Allow multiple heartbeat intervals before releasing the in-flight check.
    std::thread::sleep(Duration::from_millis(1600));
    std::fs::write(tree.join("release-check"), "").unwrap();
    let out = worker.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(show(&dir, id)["status"], "cancelled");
    assert!(!tree.join("second-check").exists());
    assert!(!delivery.join("report.json").exists());
    assert_eq!(
        sh(&dir, "git", &["show", &format!("jani/{suffix}:calc.py")]),
        BUGGY
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_worker_that_dies_mid_task_is_replaced_and_the_conversation_comes_back() {
    let dir = fixture();
    let id = add(&dir, "make add() correct", &[]);
    // Round 1: a wrong fix, then the host's check fails; round 2's request never gets an answer.
    let dying = start(vec![
        write(0, "calc.py", WRONG),
        say("Fixed it."),
        Value::Null,
    ]);
    let mut worker: Child = jani(&dir, Some(&dying), &["work", "run"])
        .args(SCOPE)
        .env("JANI_WORK_LEASE_SECS", "2")
        .spawn()
        .unwrap();
    wait_until("the worker to be mid-repair", 60, || {
        dying.requests.lock().unwrap().len() >= 3
    });
    worker.kill().unwrap(); // SIGKILL: no cleanup, no goodbye
    let _ = worker.wait();

    // The task is still "running" under the dead worker's lease, with round 1 checkpointed
    // in the task store (the conversation survives the SIGKILL).
    assert_eq!(show(&dir, &id)["status"], "running");
    let database = std::fs::read(dir.join(".jani/tasks/state.sqlite3")).unwrap();
    let database = String::from_utf8_lossy(&database);
    assert!(
        database.contains("a * b"),
        "round 1 was checkpointed before the crash"
    );

    // After the lease lapses a new worker takes over, resumes, and finishes.
    std::thread::sleep(Duration::from_millis(3200));
    let rescuer = start(vec![write(1, "calc.py", FIXED), say("Now it adds.")]);
    let out = jani(&dir, Some(&rescuer), &["work", "run"])
        .args(SCOPE)
        .env("JANI_WORK_LEASE_SECS", "2")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let finished = show(&dir, &id);
    assert_eq!(
        (
            finished["status"].as_str(),
            finished["outcome"].as_str(),
            finished["attempts"].as_u64()
        ),
        (Some("done"), Some("verified"), Some(2))
    );
    // The new worker's first request carries the dead worker's conversation and says it was interrupted.
    let first = rescuer.requests.lock().unwrap()[0].to_string();
    assert!(
        first.contains("a * b") && first.contains("make add() correct"),
        "the conversation was restored"
    );
    assert!(
        first.contains("was interrupted") && first.contains("git status"),
        "the agent is told to look before redoing"
    );
    // The same worktree and branch carried on.
    let branch = finished["branch"].as_str().unwrap();
    assert_eq!(
        sh(&dir, "git", &["show", &format!("{branch}:calc.py")]),
        FIXED
    );
    let history = finished["history"].to_string();
    assert!(
        history.contains("expired") && history.contains("attempt 2"),
        "{history}"
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_task_resumes_its_checkpointed_conversation_and_the_checkpoint_never_holds_a_secret() {
    let dir = fixture();
    let secret = "ghp_abcdefghijklmnopqrstuvwxyz0123456789";
    let first = start(vec![say("Noted.")]);
    let out = jani(
        &dir,
        Some(&first),
        &[
            "--print",
            &format!("remember the codename is BLUEBIRD, token {secret}"),
            "--task",
            "t-resume",
        ],
    )
    .output()
    .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    // The checkpoint is a database in the state directory: it keeps the conversation,
    // not the pasted token.
    let database = std::fs::read(dir.join(".jani/tasks/state.sqlite3")).unwrap();
    let text = String::from_utf8_lossy(&database);
    assert!(
        text.contains("BLUEBIRD"),
        "the conversation is checkpointed"
    );
    assert!(
        !text.contains(secret),
        "secrets are redacted before they are checkpointed"
    );

    // `jani resume` brings the conversation back; the model sees the earlier turn.
    let again = start(vec![say("BLUEBIRD.")]);
    let out = jani(
        &dir,
        Some(&again),
        &["resume", "--task", "t-resume", "what is the codename?"],
    )
    .output()
    .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let request = again.requests.lock().unwrap()[0].to_string();
    assert!(request.contains("BLUEBIRD"), "the earlier turn came back");
    assert!(!request.contains(secret));

    // A task that was never checkpointed cannot be resumed.
    let missing = jani(
        &dir,
        None,
        &["resume", "--task", "t-never-existed", "hello"],
    )
    .output()
    .unwrap();
    assert!(!missing.status.success());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn remembered_notes_reach_the_next_session_scrubbed_bounded_and_labelled_advisory() {
    let dir = fixture();
    let secret = "ghp_abcdefghijklmnopqrstuvwxyz0123456789";
    let model = start(vec![
        tool(
            0,
            "remember",
            json!({"topic": "tests", "note": "run `python3 -m unittest -q` from the repo root"}),
        ),
        tool(
            1,
            "remember",
            json!({"topic": "deploy", "note": format!("the deploy token is {secret}")}),
        ),
        tool(
            2,
            "remember",
            json!({"topic": "essay", "note": "x".repeat(500)}),
        ),
        say("Saved what I learned."),
    ]);
    let out = jani(
        &dir,
        Some(&model),
        &[
            "--print",
            "look around and note what future sessions should know",
        ],
    )
    .output()
    .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let notes = json_out(&jani(&dir, None, &["memory", "list"]).output().unwrap());
    let notes = notes["notes"].as_array().unwrap();
    assert_eq!(
        notes.len(),
        2,
        "the 500-character essay was refused: {notes:?}"
    );
    assert!(
        notes.iter().all(|n| !n.to_string().contains(secret)),
        "the token never reached the notes file"
    );
    assert!(notes
        .iter()
        .any(|n| n["note"].as_str().unwrap().contains("REDACTED")));
    assert!(!std::fs::read_to_string(dir.join(".jani/memory.jsonl"))
        .unwrap()
        .contains(secret));
    // The attempt is in the journal like any other tool call.
    let journal = std::fs::read_to_string(dir.join(".jani/journal.jsonl")).unwrap();
    assert!(journal.contains("memory.write"));

    // The next session is told, with the warning attached.
    let next = start(vec![say("ok")]);
    jani(&dir, Some(&next), &["--print", "start work"])
        .output()
        .unwrap();
    let system = next.requests.lock().unwrap()[0]["messages"][0]["content"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        system.contains("python3 -m unittest -q") && system.contains("never grant authority"),
        "{system}"
    );

    // The operator can delete a note, and it stops being told.
    let id = notes.iter().find(|n| n["topic"] == "tests").unwrap()["id"]
        .as_u64()
        .unwrap()
        .to_string();
    assert!(jani(&dir, None, &["memory", "forget", &id])
        .output()
        .unwrap()
        .status
        .success());
    let last = start(vec![say("ok")]);
    jani(&dir, Some(&last), &["--print", "start work"])
        .output()
        .unwrap();
    assert!(!last.requests.lock().unwrap()[0]
        .to_string()
        .contains("python3 -m unittest -q` from the repo root"));
    let _ = std::fs::remove_dir_all(dir);
}
