//! The agent asks instead of guessing: what happens with nobody to answer, with
//! the work queue, and with the web workspace.

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

mod support;
use support::provider::{say, start, tool, write, Scripted};

const BUGGY: &str = "def add(a, b):\n    return a - b\n";
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
    let dir = std::env::temp_dir().join(format!("jani-ask-{}-{stamp:x}", std::process::id()));
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

fn ask(id: usize, question: &str, options: &[&str]) -> Value {
    tool(id, "ask", json!({"question": question, "options": options}))
}

fn json_out(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|_| {
        panic!(
            "not json: {} / {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

fn journal(dir: &Path) -> Vec<Value> {
    std::fs::read_to_string(dir.join(".jani/journal.jsonl"))
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

const SCOPE: [&str; 5] = [
    "--allow-write",
    "--write-path",
    "calc.py",
    "--write-path",
    "test_calc.py",
];

#[test]
fn with_nobody_to_answer_the_turn_ends_on_the_question_and_does_not_guess() {
    let dir = fixture();
    let model = start(vec![
        ask(
            0,
            "Should add() accept strings and concatenate them?",
            &["numbers only", "strings too"],
        ),
        say("(never reached: the turn must have ended)"),
    ]);
    let out = jani(&dir, Some(&model), &["--print", "improve add()"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("I need your input before I continue"),
        "{text}"
    );
    assert!(
        text.contains("accept strings")
            && text.contains("1. numbers only")
            && text.contains("2. strings too"),
        "{text}"
    );
    assert_eq!(
        model.requests.lock().unwrap().len(),
        1,
        "the model was not asked to continue past the question"
    );
    let events = journal(&dir);
    assert!(events
        .iter()
        .any(|e| e["action"] == "agent.ask" && e["decision"] == "executed"));
    assert!(events.iter().any(|e| e["action"] == "agent.run"
        && e["reason_code"] == "needs_input"
        && e["decision"] == "completed"));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn malformed_questions_are_refused_and_the_model_can_carry_on() {
    let dir = fixture();
    let model = start(vec![
        tool(0, "ask", json!({"question": "x".repeat(600)})),
        tool(
            1,
            "ask",
            json!({"question": "pick", "options": ["a", "b", "c", "d", "e", "f", "g"]}),
        ),
        say("Proceeding on my own defaults."),
    ]);
    let out = jani(&dir, Some(&model), &["--print", "do it"])
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("Proceeding on my own defaults"));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_blocked_task_waits_for_the_operator_then_resumes_with_the_answer_and_finishes() {
    let dir = fixture();
    let add = jani(
        &dir,
        None,
        &[
            "work",
            "add",
            "make add() correct",
            "--accept",
            "python3 -m unittest -q",
        ],
    )
    .output()
    .unwrap();
    let id = json_out(&add)["id"].as_str().unwrap().to_string();

    // First run: the model asks, and nobody is there.
    let asking = start(vec![ask(
        0,
        "Fix add() in place, or add a new add_numbers() and deprecate it?",
        &["in place", "new function"],
    )]);
    let out = jani(&dir, Some(&asking), &["work", "run"])
        .args(SCOPE)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let blocked = json_out(&jani(&dir, None, &["work", "show", &id]).output().unwrap());
    assert_eq!(
        (blocked["status"].as_str(), blocked["outcome"].as_str()),
        (Some("blocked"), Some("blocked"))
    );
    assert!(blocked["question"].as_str().unwrap().contains("in place"));
    assert_eq!(blocked["options"], json!(["in place", "new function"]));
    // Nothing was checked, committed or merged while it waits, and a blocked task is not picked up again.
    assert_eq!(std::fs::read_to_string(dir.join("calc.py")).unwrap(), BUGGY);
    assert!(blocked["commit"].is_null());
    let idle = start(vec![]);
    jani(&dir, Some(&idle), &["work", "run"])
        .args(SCOPE)
        .output()
        .unwrap();
    assert!(
        idle.requests.lock().unwrap().is_empty(),
        "no worker takes a blocked task"
    );

    // The operator answers; the task goes back in the queue with the answer attached.
    let empty = jani(&dir, None, &["work", "answer", &id, ""])
        .output()
        .unwrap();
    assert!(!empty.status.success(), "an empty answer is refused");
    let answered = jani(&dir, None, &["work", "answer", &id, "in place"])
        .output()
        .unwrap();
    assert!(
        answered.status.success(),
        "{}",
        String::from_utf8_lossy(&answered.stderr)
    );
    assert_eq!(json_out(&answered)["status"], "queued");

    // The next run resumes the conversation, sees the answer, and finishes the job.
    let finishing = start(vec![
        write(1, "calc.py", FIXED),
        say("Fixed in place, as you said."),
    ]);
    let out = jani(&dir, Some(&finishing), &["work", "run"])
        .args(SCOPE)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let first = finishing.requests.lock().unwrap()[0].to_string();
    assert!(
        first.contains("Fix add() in place") && first.contains("A (from the operator): in place"),
        "the question and answer reach the model"
    );
    let done = json_out(&jani(&dir, None, &["work", "show", &id]).output().unwrap());
    assert_eq!(
        (done["status"].as_str(), done["outcome"].as_str()),
        (Some("done"), Some("verified"))
    );
    let branch = done["branch"].as_str().unwrap();
    assert_eq!(
        sh(&dir, "git", &["show", &format!("{branch}:calc.py")]),
        FIXED
    );
    let _ = std::fs::remove_dir_all(dir);
}
