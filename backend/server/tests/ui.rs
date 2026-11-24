//! End-to-end checks for `jani ui`: the trust boundary of the local web
//! workspace and its wiring to the real journal, approvals and session.

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::process::{Child, Stdio};
use std::time::{Duration, Instant};

mod support;

struct Ui {
    child: Child,
    port: u16,
    token: String,
    root: PathBuf,
    // Held open so the server's later prints never hit a closed pipe.
    _stdout: BufReader<std::process::ChildStdout>,
}

impl Drop for Ui {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn start(extra: &[&str]) -> Ui {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("jani-ui-{}-{stamp:x}", std::process::id()));
    std::fs::create_dir_all(root.join("repo")).unwrap();
    std::fs::write(root.join("repo/README.md"), "hello\n").unwrap();
    let mut child = support::jani()
        .args(["ui", "--port", "0", "--repo"])
        .arg(root.join("repo"))
        .arg("--state-dir")
        .arg(root.join("state"))
        .args(extra)
        .env("JANI_API_KEY", "test-key")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let mut line = String::new();
    stdout.read_line(&mut line).unwrap();
    let url = line.split("http://127.0.0.1:").nth(1).expect(&line);
    let (port, token) = url.trim().split_once("/#token=").expect(&line);
    Ui {
        port: port.parse().unwrap(),
        token: token.to_string(),
        child,
        root,
        _stdout: stdout,
    }
}

struct Reply {
    status: u16,
    headers: String,
    body: String,
}

impl Reply {
    fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap_or_else(|_| panic!("not json: {}", self.body))
    }
}

fn request(ui: &Ui, method: &str, path: &str, headers: &[(&str, String)], body: &str) -> Reply {
    let mut stream = TcpStream::connect(("127.0.0.1", ui.port)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let mut head = format!("{method} {path} HTTP/1.1\r\n");
    if !headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("host")) {
        head.push_str(&format!("Host: 127.0.0.1:{}\r\n", ui.port));
    }
    for (k, v) in headers {
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    head.push_str(&format!(
        "Content-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    ));
    stream.write_all(head.as_bytes()).unwrap();
    stream.write_all(body.as_bytes()).unwrap();
    let mut raw = String::new();
    stream
        .read_to_string(&mut raw)
        .unwrap_or_else(|e| panic!("read failed: {e}; partial={raw:?}"));
    let (h, b) = raw.split_once("\r\n\r\n").unwrap_or((&raw, ""));
    let status = h.split_whitespace().nth(1).unwrap().parse().unwrap();
    Reply {
        status,
        headers: h.to_string(),
        body: b.to_string(),
    }
}

fn get(ui: &Ui, path: &str) -> Reply {
    request(ui, "GET", path, &[("X-Jani-Token", ui.token.clone())], "")
}

fn post(ui: &Ui, path: &str, body: Value) -> Reply {
    request(
        ui,
        "POST",
        path,
        &[
            ("X-Jani-Token", ui.token.clone()),
            ("Content-Type", "application/json".into()),
        ],
        &body.to_string(),
    )
}

fn wait_for_entry(ui: &Ui, needle: &str) -> Value {
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        let session = get(ui, "/api/session?after=-1").json();
        if let Some(entry) = session["entries"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["text"].as_str().unwrap_or("").contains(needle))
        {
            return entry.clone();
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("no transcript entry containing {needle:?}");
}

#[test]
fn the_page_is_served_with_a_nonce_csp_and_no_token_in_it() {
    let ui = start(&[]);
    let reply = request(&ui, "GET", "/", &[], "");
    assert_eq!(reply.status, 200);
    assert!(reply.headers.contains("Content-Security-Policy"));
    assert!(reply.headers.contains("default-src 'none'"));
    assert!(reply.headers.contains("frame-ancestors 'none'"));
    assert!(
        !reply.body.contains(&ui.token),
        "token must never be embedded"
    );
    assert!(!reply.body.contains("__NONCE__"));
    assert!(!reply.body.contains("https://"), "no third-party hosts");
}

#[test]
fn api_requires_the_token_and_the_exact_loopback_host() {
    let ui = start(&[]);
    assert_eq!(request(&ui, "GET", "/api/overview", &[], "").status, 401);
    let wrong = request(
        &ui,
        "GET",
        "/api/overview",
        &[("X-Jani-Token", "0".repeat(ui.token.len()))],
        "",
    );
    assert_eq!(wrong.status, 401);
    let rebound = request(
        &ui,
        "GET",
        "/api/overview",
        &[
            ("Host", "attacker.example".into()),
            ("X-Jani-Token", ui.token.clone()),
        ],
        "",
    );
    assert_eq!(rebound.status, 403, "DNS rebinding must be refused");
    assert_eq!(get(&ui, "/api/overview").status, 200);
}

#[test]
fn mutating_calls_need_json_and_a_same_origin() {
    let ui = start(&[]);
    let plain = request(
        &ui,
        "POST",
        "/api/session/send",
        &[("X-Jani-Token", ui.token.clone())],
        "{\"text\":\"/help\"}",
    );
    assert_eq!(plain.status, 415);
    let foreign = request(
        &ui,
        "POST",
        "/api/session/send",
        &[
            ("X-Jani-Token", ui.token.clone()),
            ("Content-Type", "application/json".into()),
            ("Origin", "http://attacker.example".into()),
        ],
        "{\"text\":\"/help\"}",
    );
    assert_eq!(foreign.status, 403);
    assert_eq!(get(&ui, "/api/nope").status, 404);
    let oversized = post(
        &ui,
        "/api/session/send",
        json!({"text": "x".repeat(70_000)}),
    );
    assert!(oversized.status == 400 || oversized.status == 413);
}

#[test]
fn overview_reports_the_frozen_authority_without_leaking_the_key() {
    let ui = start(&["--allow-write", "--write-path", "out"]);
    let overview = get(&ui, "/api/overview");
    assert!(!overview.body.contains("test-key"));
    let v = overview.json();
    assert_eq!(v["authority"]["allowWrite"], true);
    assert_eq!(v["authority"]["allowExec"], false);
    assert_eq!(v["authority"]["writePaths"], json!(["out"]));
    assert_eq!(v["model"]["keyConfigured"], true);
    assert_eq!(v["identity"]["verified"], false);
    assert_eq!(v["journal"]["ok"], true);
}

#[test]
fn slash_commands_run_through_the_real_session() {
    let ui = start(&[]);
    assert_eq!(
        post(&ui, "/api/session/send", json!({"text": "/status"})).status,
        202
    );
    let note = wait_for_entry(&ui, "Writes: false");
    assert_eq!(note["kind"], "note");
    assert!(note["text"].as_str().unwrap().contains("Worktree:"));
    // An unknown command is reported, not swallowed.
    post(&ui, "/api/session/send", json!({"text": "/bogus"}));
    let err = wait_for_entry(&ui, "unknown command");
    assert_eq!(err["kind"], "error");
}

#[test]
fn approval_decisions_are_refused_without_a_verified_identity() {
    let ui = start(&[]);
    let reply = post(
        &ui,
        "/api/approvals/decide",
        json!({"id": "approval-x", "approve": true, "role": "approver"}),
    );
    assert_eq!(reply.status, 403);
    assert!(reply.json()["error"]
        .as_str()
        .unwrap()
        .contains("verified identity"));
}

#[test]
fn a_tampered_journal_is_refused_not_displayed() {
    let ui = start(&[]);
    let journal = ui.root.join("state/journal.jsonl");
    // Forge one event whose digest chain cannot verify.
    std::fs::create_dir_all(ui.root.join("state")).unwrap();
    std::fs::write(
        &journal,
        "{\"event_version\":\"1.0\",\"event_id\":\"evt-1\",\"tenant_id\":\"local\",\"task_id\":\"t\",\"run_id\":\"r\",\"invocation_id\":\"run\",\"actor_id\":\"a\",\"worker_id\":\"a\",\"action\":\"agent.run\",\"resource_digest\":\"d\",\"decision\":\"started\",\"reason_code\":\"run_started\",\"policy_revision\":\"runtime\",\"data_classification\":\"INTERNAL\",\"timestamp\":1,\"previous_event_digest\":\"\",\"event_digest\":\"forged\"}\n",
    )
    .unwrap();
    let overview = get(&ui, "/api/overview").json();
    assert_eq!(overview["journal"]["ok"], false);
    let run = get(&ui, "/api/run?id=r");
    assert_eq!(run.status, 409, "{}", run.body);
    assert!(run.body.contains("refusing to show unverified events"));
    assert!(!run.body.contains("forged"));
}
