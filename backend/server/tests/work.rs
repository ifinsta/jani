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

    // The task is still "running" under the dead worker's lease, with round 1 saved.
    assert_eq!(show(&dir, &id)["status"], "running");
    let task = format!("task-{}", id.trim_start_matches("wrk-"));
    let saved: Value = serde_json::from_str(
        &std::fs::read_to_string(dir.join(".jani/conversations").join(format!("{task}.json")))
            .unwrap(),
    )
    .unwrap();
    assert!(
        saved["messages"].to_string().contains("a * b"),
        "round 1 was saved before the crash"
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
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("resumed"),
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
fn a_saved_conversation_resumes_only_when_asked_and_never_holds_a_secret() {
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

    // The snapshot is private and redacted.
    let file = dir.join(".jani/conversations/t-resume.json");
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(
        text.contains("BLUEBIRD") && !text.contains(secret),
        "secrets are redacted before they are saved"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&file).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    // Without --resume the model starts clean.
    let fresh = start(vec![say("What codename?")]);
    jani(
        &dir,
        Some(&fresh),
        &["--print", "what is the codename?", "--task", "t-resume"],
    )
    .output()
    .unwrap();
    assert!(!fresh.requests.lock().unwrap()[0]
        .to_string()
        .contains("BLUEBIRD"));

    // With --resume (on a snapshot that still has the first turn) it remembers.
    let again = start(vec![say("BLUEBIRD.")]);
    std::fs::write(&file, &text).unwrap(); // the run above saved its own turn; go back to the first
    let out = jani(
        &dir,
        Some(&again),
        &[
            "--print",
            "what is the codename?",
            "--task",
            "t-resume",
            "--resume",
        ],
    )
    .output()
    .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        again.requests.lock().unwrap()[0]
            .to_string()
            .contains("BLUEBIRD"),
        "the earlier turn came back"
    );

    // --resume needs a task to resume.
    let bare = jani(&dir, None, &["--print", "x", "--resume"])
        .output()
        .unwrap();
    assert!(!bare.status.success() && String::from_utf8_lossy(&bare.stderr).contains("--task"));
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
