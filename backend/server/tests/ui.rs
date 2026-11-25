//! End-to-end checks for `jani ui`: the trust boundary of the local web
//! workspace and its wiring to the real journal, approvals and session.

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
#[cfg(unix)]
use std::net::TcpListener;
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

fn start_with_state(root: &std::path::Path) -> Ui {
    let mut child = support::jani()
        .args(["ui", "--port", "0", "--repo"])
        .arg(root.join("repo"))
        .arg("--state-dir")
        .arg(root.join("state"))
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
        root: root.to_path_buf(),
        _stdout: stdout,
    }
}

fn start(extra: &[&str]) -> Ui {
    start_with_env(extra, &[])
}

fn start_with_env(extra: &[&str], env: &[(&str, String)]) -> Ui {
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
        .envs(env.iter().map(|(k, v)| (*k, v.as_str())))
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
    assert_eq!(v["proxied"], false, "plain jani ui is not behind a gateway");
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

#[test]
fn read_only_screens_answer_on_a_fresh_state_directory() {
    let ui = start(&[]);
    for path in [
        "/api/policy",
        "/api/system",
        "/api/native",
        "/api/updates",
        "/api/recovery",
        "/api/attention",
        "/api/mirror?sub=workflows",
        "/api/mirror?sub=recommendations",
        "/api/mirror?sub=history",
        "/api/lab",
        "/api/audit",
        "/api/evidence/list",
        "/api/approvals",
        "/api/budgets",
    ] {
        let reply = get(&ui, path);
        assert_eq!(reply.status, 200, "{path}: {}", reply.body);
    }
    let policy = get(&ui, "/api/policy").json();
    assert_eq!(policy["revision"], "local-baseline");
    assert_eq!(policy["settings"]["profiles"], json!([]));
    let native = get(&ui, "/api/native").json();
    assert_eq!(native.as_array().unwrap().len(), 12);
    let system = get(&ui, "/api/system").json();
    assert!(system["sandbox"]["doctor"]["backends"].is_array());
}

#[test]
fn views_refuse_what_the_command_line_keeps_to_itself() {
    let ui = start(&[]);
    // `mirror record` persists a recommendation; it is not offered.
    let record = get(&ui, "/api/mirror?sub=record&run=run-1");
    assert_eq!(record.status, 422, "{}", record.body);
    // State-changing and operator-only native operations stay on the CLI.
    for (tool, op) in [
        ("index", "build"),
        ("planner", "verify"),
        ("workspace", "rollback"),
    ] {
        let reply = post(
            &ui,
            "/api/native/run",
            json!({"tool": tool, "op": op, "args": []}),
        );
        assert_eq!(reply.status, 422, "{tool} {op}: {}", reply.body);
        assert!(reply.body.contains(&format!("jani native {tool} {op}")));
    }
    let unknown = post(&ui, "/api/native/run", json!({"tool": "nope", "op": "x"}));
    assert_eq!(unknown.status, 422);
    // Bad identifiers never reach the subsystem.
    assert_eq!(get(&ui, "/api/evidence?id=..%2F..%2Fetc").status, 422);
    assert_eq!(get(&ui, "/api/mirror?sub=critique&run=a%20b").status, 422);
    assert_eq!(get(&ui, "/api/approval?id=approval-missing").status, 404);
    // Updates make no request unless a source is configured.
    let update = post(&ui, "/api/updates/check", json!({}));
    assert_eq!(update.status, 422);
    assert!(update.body.contains("no update source configured"));
}

#[test]
fn policy_check_evaluates_without_running_anything() {
    let ui = start(&[]);
    let allowed = post(
        &ui,
        "/api/policy/check",
        json!({"action": "repo.read", "resource": "repo:README.md"}),
    );
    assert_eq!(allowed.status, 200, "{}", allowed.body);
    assert_eq!(allowed.json()["decision"], "permit");
    let write = post(
        &ui,
        "/api/policy/check",
        json!({"action": "repo.write", "resource": "repo:out/x.txt"}),
    );
    assert_eq!(write.status, 200);
    assert_ne!(
        write.json()["decision"],
        "permit",
        "no write grant at startup"
    );
    assert_eq!(
        post(&ui, "/api/policy/check", json!({"action": ""})).status,
        422
    );
}

#[test]
fn journal_paging_filtering_and_run_grouping_use_real_events() {
    let ui = start(&[]);
    // A read-only native operation is governed and writes started/completed.
    let ran = post(
        &ui,
        "/api/native/run",
        json!({"tool": "evidence", "op": "list", "args": []}),
    );
    assert_eq!(ran.status, 200, "{}", ran.body);
    let all = get(&ui, "/api/journal").json();
    assert!(all["total"].as_u64().unwrap() >= 2, "{all}");
    let one = get(&ui, "/api/journal?limit=1").json();
    assert_eq!(one["events"].as_array().unwrap().len(), 1);
    assert_eq!(one["hasMore"], true);
    assert_eq!(one["events"][0]["seq"], one["total"], "newest event last");
    let completed = get(&ui, "/api/journal?decision=completed").json();
    assert!(completed["matched"].as_u64().unwrap() >= 1);
    assert!(completed["events"]
        .as_array()
        .unwrap()
        .iter()
        .all(|e| e["decision"] == "completed"));
    let none = get(&ui, "/api/journal?q=zzzz-no-such-thing").json();
    assert_eq!(none["matched"], 0);
    let older = get(
        &ui,
        &format!("/api/journal?before={}", one["events"][0]["seq"]),
    )
    .json();
    assert!(older["events"]
        .as_array()
        .unwrap()
        .iter()
        .all(|e| e["seq"].as_u64().unwrap() < one["events"][0]["seq"].as_u64().unwrap()));
    // Operations that share the journal are not runs: there is no agent.run.
    let runs = get(&ui, "/api/runs").json();
    assert!(runs
        .as_array()
        .unwrap()
        .iter()
        .all(|r| r["runId"] != "native-tool"));
    // Their events are still in the verified journal and in an evidence export.
    assert_eq!(get(&ui, "/api/audit").json()["ok"], true);
}

#[test]
fn registered_agents_and_alerts_are_listed_read_only() {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let state =
        std::env::temp_dir().join(format!("jani-ui-agents-{}-{stamp:x}", std::process::id()));
    let repo = state.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    for args in [
        vec!["agent", "register", "worker-1"],
        vec![
            "alerts",
            "send",
            "worker-1",
            "payment.failed",
            "banking",
            "case-1",
            "high",
            "key-1",
        ],
    ] {
        let status = support::jani()
            .args(&args)
            .arg("--repo")
            .arg(&repo)
            .arg("--state-dir")
            .arg(state.join("state"))
            .env("JANI_API_KEY", "test-key")
            .output()
            .unwrap();
        assert!(
            status.status.success(),
            "{}",
            String::from_utf8_lossy(&status.stderr)
        );
    }
    let mut ui = start_with_state(&state);
    let view = get(&ui, "/api/attention").json();
    assert_eq!(view["agents"][0]["agentId"], "worker-1");
    assert_eq!(view["agents"][0]["phase"], "ready");
    assert_eq!(view["alerts"][0]["severity"], "high");
    assert_eq!(view["alerts"][0]["phase"], "pending");
    // Nothing on this screen can change that state.
    assert_eq!(post(&ui, "/api/attention", json!({})).status, 404);
    ui.root = state;
}

#[cfg(unix)]
#[test]
fn unix_socket_mode_has_no_tcp_port_and_keeps_every_check() {
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::UnixStream;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("jani-ui-sock-{}-{stamp:x}", std::process::id()));
    std::fs::create_dir_all(root.join("repo")).unwrap();
    let socket = root.join("ui.sock");
    let mut child = support::jani()
        .args(["ui", "--unix-socket"])
        .arg(&socket)
        .arg("--repo")
        .arg(root.join("repo"))
        .arg("--state-dir")
        .arg(root.join("state"))
        .env("JANI_API_KEY", "test-key")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let mut line = String::new();
    stdout.read_line(&mut line).unwrap();
    let rest = line.split("unix:").nth(1).expect(&line);
    let (path, token) = rest.trim().split_once("/#token=").expect(&line);
    assert_eq!(std::path::Path::new(path), socket);
    assert_eq!(
        std::fs::metadata(&socket).unwrap().permissions().mode() & 0o777,
        0o600,
        "owner-only socket"
    );
    let call = |host: &str, with_token: bool| -> (u16, String) {
        let mut s = UnixStream::connect(&socket).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        let mut head =
            format!("GET /api/overview HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n");
        if with_token {
            head.push_str(&format!("X-Jani-Token: {token}\r\n"));
        }
        head.push_str("\r\n");
        s.write_all(head.as_bytes()).unwrap();
        let mut raw = String::new();
        s.read_to_string(&mut raw).unwrap();
        let status = raw.split_whitespace().nth(1).unwrap().parse().unwrap();
        (status, raw)
    };
    assert_eq!(call("localhost", true).0, 200);
    assert_eq!(
        call("localhost", false).0,
        401,
        "the token is still required"
    );
    assert_eq!(
        call("attacker.example", true).0,
        403,
        "Host is still checked"
    );
    let (_, body) = call("localhost", true);
    assert!(body.contains("\"proxied\""), "{body}");
    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_dir_all(&root);
}

/// A model that replies from a script, one reply per request.
#[cfg(unix)]
fn scripted_provider(replies: Vec<Value>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for reply in replies {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            stream
                .set_read_timeout(Some(Duration::from_secs(20)))
                .unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut length = 0usize;
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 || line.trim_end().is_empty() {
                    break;
                }
                if let Some((name, value)) = line.trim_end().split_once(':') {
                    if name.eq_ignore_ascii_case("content-length") {
                        length = value.trim().parse().unwrap_or(0);
                    }
                }
            }
            let mut body = vec![0u8; length];
            let _ = reader.read_exact(&mut body);
            // The workspace streams, so answer as server-sent events.
            let message = &reply["choices"][0]["message"];
            let mut events = Vec::new();
            if let Some(calls) = message["tool_calls"].as_array() {
                let calls: Vec<Value> = calls
                    .iter()
                    .enumerate()
                    .map(|(index, c)| json!({"index": index, "id": c["id"], "type": c["type"], "function": c["function"]}))
                    .collect();
                events.push(json!({"choices": [{"index": 0, "delta": {"role": "assistant", "tool_calls": calls}}]}));
                events.push(
                    json!({"choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls"}]}),
                );
            } else {
                events.push(json!({"choices": [{"index": 0, "delta": {"role": "assistant", "content": message["content"]}}]}));
                events
                    .push(json!({"choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]}));
            }
            events.push(json!({"choices": [], "usage": reply["usage"]}));
            let mut text = String::new();
            for event in events {
                text.push_str(&format!("data: {event}\n\n"));
            }
            text.push_str("data: [DONE]\n\n");
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n{text}"
            );
            let _ = stream.flush();
            let _ = stream.shutdown(std::net::Shutdown::Both);
        }
    });
    format!("http://{address}/v1")
}

#[cfg(unix)]
fn tool_call(argv: &[&str]) -> Value {
    json!({
        "choices": [{"message": {"role": "assistant", "content": null, "tool_calls": [{
            "id": "call-1", "type": "function",
            "function": {"name": "command", "arguments": json!({"argv": argv, "timeout_secs": 20}).to_string()}
        }]}}],
        "usage": {"prompt_tokens": 5, "completion_tokens": 2}
    })
}

#[cfg(unix)]
fn say(text: &str) -> Value {
    json!({"choices": [{"message": {"role": "assistant", "content": text}}], "usage": {"prompt_tokens": 5, "completion_tokens": 2}})
}

#[cfg(unix)]
fn wait_for_consent(ui: &Ui) -> Value {
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        let session = get(ui, "/api/session?after=-1").json();
        if !session["consent"].is_null() {
            return session["consent"].clone();
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!(
        "the agent never asked: {}",
        get(ui, "/api/session?after=-1").body
    );
}

#[cfg(unix)]
#[test]
fn smart_exec_asks_the_operator_in_the_workspace_and_the_answer_decides() {
    let base = scripted_provider(vec![
        tool_call(&["touch", "made.txt"]),
        say("made it"),
        tool_call(&["touch", "other.txt"]),
        say("understood, not made"),
    ]);
    let ui = start_with_env(
        &["--exec", "smart"],
        &[("JANI_API_BASE", base), ("JANI_MODEL", "scripted".into())],
    );
    // Authority shows the mode the operator started with.
    assert_eq!(
        get(&ui, "/api/overview").json()["authority"]["execMode"],
        "smart"
    );

    post(&ui, "/api/session/send", json!({"text": "make a file"}));
    let question = wait_for_consent(&ui);
    assert_eq!(question["command"], "touch made.txt");
    assert!(question["reason"].as_str().unwrap().contains("touch"));
    assert_eq!(question["suggest"], "touch made.txt");
    // The file is not made while the question is open.
    assert!(!ui.root.join("repo/made.txt").exists());
    // A stale or wrong question id is refused; a malformed answer is refused.
    let id = question["id"].as_u64().unwrap();
    assert_eq!(
        post(
            &ui,
            "/api/consent",
            json!({"id": id + 7, "answer": "always"})
        )
        .status,
        409
    );
    assert_eq!(
        post(&ui, "/api/consent", json!({"id": id, "answer": "sure"})).status,
        400
    );
    assert_eq!(
        post(&ui, "/api/consent", json!({"id": id, "answer": "once"})).status,
        200
    );
    wait_for_entry(&ui, "made it");
    assert!(ui.root.join("repo/made.txt").exists());
    assert!(get(&ui, "/api/session?after=-1").json()["consent"].is_null());
    // Answering twice does nothing.
    assert_eq!(
        post(&ui, "/api/consent", json!({"id": id, "answer": "once"})).status,
        409
    );

    // Declining stops the command and the model is told so.
    post(&ui, "/api/session/send", json!({"text": "make another"}));
    let second = wait_for_consent(&ui);
    assert_eq!(second["command"], "touch other.txt");
    assert_eq!(
        post(
            &ui,
            "/api/consent",
            json!({"id": second["id"], "answer": "deny"})
        )
        .status,
        200
    );
    wait_for_entry(&ui, "understood, not made");
    assert!(!ui.root.join("repo/other.txt").exists());

    // And the journal says who allowed what, and what was declined.
    let journal = get(&ui, "/api/journal?limit=200").json();
    let text = journal.to_string();
    assert!(text.contains("exec_operator_once"), "{text}");
    assert!(text.contains("exec_operator_declined"), "{text}");
}

#[cfg(unix)]
#[test]
fn cancelling_a_turn_answers_a_waiting_question_no() {
    let base = scripted_provider(vec![tool_call(&["touch", "never.txt"]), say("stopped")]);
    let ui = start_with_env(
        &["--exec", "smart"],
        &[("JANI_API_BASE", base), ("JANI_MODEL", "scripted".into())],
    );
    post(&ui, "/api/session/send", json!({"text": "do it"}));
    wait_for_consent(&ui);
    assert_eq!(post(&ui, "/api/session/cancel", json!({})).status, 202);
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let session = get(&ui, "/api/session?after=-1").json();
        if session["consent"].is_null() && session["busy"] == false {
            break;
        }
        assert!(Instant::now() < deadline, "the question never closed");
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(!ui.root.join("repo/never.txt").exists());
}

#[cfg(unix)]
fn ask_call(question: &str, options: &[&str]) -> Value {
    json!({
        "choices": [{"message": {"role": "assistant", "content": null, "tool_calls": [{
            "id": "call-1", "type": "function",
            "function": {"name": "ask", "arguments": json!({"question": question, "options": options}).to_string()}
        }]}}],
        "usage": {"prompt_tokens": 5, "completion_tokens": 2}
    })
}

#[cfg(unix)]
fn wait_for_question(ui: &Ui) -> Value {
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        let session = get(ui, "/api/session?after=-1").json();
        if !session["question"].is_null() {
            return session["question"].clone();
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!(
        "the agent never asked: {}",
        get(ui, "/api/session?after=-1").body
    );
}

#[cfg(unix)]
#[test]
fn the_agent_asks_in_the_workspace_and_carries_on_with_the_answer() {
    let base = scripted_provider(vec![
        ask_call(
            "Which database should the cache use?",
            &["sqlite", "postgres"],
        ),
        say("Using sqlite, as you chose."),
        ask_call("And the table name?", &[]),
        say("Left it for later."),
    ]);
    let ui = start_with_env(
        &[],
        &[("JANI_API_BASE", base), ("JANI_MODEL", "scripted".into())],
    );
    post(&ui, "/api/session/send", json!({"text": "add a cache"}));
    let question = wait_for_question(&ui);
    assert_eq!(question["text"], "Which database should the cache use?");
    assert_eq!(question["options"], json!(["sqlite", "postgres"]));
    let id = question["id"].as_u64().unwrap();
    // Wrong id, bad payload: refused, and the question stays open.
    assert_eq!(
        post(&ui, "/api/answer", json!({"id": id + 9, "text": "x"})).status,
        409
    );
    assert_eq!(
        post(&ui, "/api/answer", json!({"id": id, "text": 5})).status,
        400
    );
    assert!(!get(&ui, "/api/session?after=-1").json()["question"].is_null());
    assert_eq!(
        post(&ui, "/api/answer", json!({"id": id, "text": "sqlite"})).status,
        200
    );
    wait_for_entry(&ui, "Using sqlite, as you chose.");
    assert!(get(&ui, "/api/session?after=-1").json()["question"].is_null());
    assert_eq!(
        post(&ui, "/api/answer", json!({"id": id, "text": "again"})).status,
        409,
        "answered once"
    );

    // Declining ends the turn on the question instead of hanging.
    post(&ui, "/api/session/send", json!({"text": "and a table"}));
    let second = wait_for_question(&ui);
    assert_eq!(
        post(
            &ui,
            "/api/answer",
            json!({"id": second["id"], "text": null})
        )
        .status,
        200
    );
    let note = wait_for_entry(&ui, "I need your input before I continue");
    assert!(note["text"].as_str().unwrap().contains("table name"));
}
