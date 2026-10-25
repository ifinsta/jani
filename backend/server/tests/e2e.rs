use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

fn temp_root() -> PathBuf {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("jani-e2e-{}-{stamp}", std::process::id()));
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/one.txt"), "hello\n").unwrap();
    root
}

fn read_http_body(stream: &mut TcpStream) -> String {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut length = 0usize;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            break;
        }
        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            break;
        }
        if let Some((name, value)) = trimmed.split_once(':') {
            if name.eq_ignore_ascii_case("content-length") {
                length = value.trim().parse().unwrap_or(0);
            }
        }
    }
    let mut body = vec![0u8; length];
    if length > 0 {
        reader.read_exact(&mut body).unwrap();
    }
    String::from_utf8_lossy(&body).into_owned()
}

fn write_http_response(stream: &mut TcpStream, body: &str) {
    let response = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    stream.write_all(response.as_bytes()).unwrap();
    stream.flush().unwrap();
    // Close before accepting the next connection so a pooled client socket cannot
    // race the next request against a half-closed connection.
    let _ = stream.shutdown(Shutdown::Both);
}

fn accept_by_deadline(listener: &TcpListener) -> Option<TcpStream> {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        match listener.accept() {
            Ok((stream, _)) => return Some(stream),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                if Instant::now() >= deadline {
                    return None;
                }
                thread::sleep(Duration::from_millis(10));
            }
            Err(_) => return None,
        }
    }
}

fn start_provider(
    responses: Vec<String>,
) -> (String, Arc<Mutex<Vec<String>>>, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = listener.local_addr().unwrap();
    let bodies = Arc::new(Mutex::new(Vec::new()));
    let captured = bodies.clone();
    let handle = thread::spawn(move || {
        for response in responses {
            let Some(mut stream) = accept_by_deadline(&listener) else {
                break;
            };
            stream.set_nonblocking(false).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(10)))
                .unwrap();
            captured.lock().unwrap().push(read_http_body(&mut stream));
            write_http_response(&mut stream, &response);
        }
    });
    (format!("http://{address}/v1"), bodies, handle)
}

fn journal_events(state: &Path) -> Vec<Value> {
    let text = std::fs::read_to_string(state.join("journal.jsonl")).unwrap();
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[test]
fn governed_run_calls_a_local_provider_and_records_evidence() {
    let root = temp_root();
    let state = root.join("state");
    let tool_call = json!({
        "choices": [{
            "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": "call-1",
                    "type": "function",
                    "function": {
                        "name": "workspace_read",
                        "arguments": "{\"path\":\"src/one.txt\"}"
                    }
                }]
            }
        }],
        "usage": {"prompt_tokens": 12, "completion_tokens": 6}
    });
    let final_answer = json!({
        "choices": [{
            "message": {"role": "assistant", "content": "The file says hello."}
        }],
        "usage": {"prompt_tokens": 20, "completion_tokens": 7}
    });
    let (base, bodies, provider) =
        start_provider(vec![tool_call.to_string(), final_answer.to_string()]);

    let mut child = Command::new(env!("CARGO_BIN_EXE_jani"))
        .args([
            "--print",
            "read src/one.txt",
            "--repo",
            root.to_str().unwrap(),
            "--state-dir",
            state.to_str().unwrap(),
            "--max-steps",
            "4",
        ])
        .env("JANI_API_KEY", "test-provider-secret")
        .env("JANI_API_BASE", &base)
        .env("JANI_MODEL", "test-model")
        .env_remove("OPENAI_API_KEY")
        .env_remove("JANI_POLICY")
        .env_remove("JANI_IDENTITY")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn jani");

    let deadline = Instant::now() + Duration::from_secs(60);
    while child.try_wait().unwrap().is_none() {
        if Instant::now() >= deadline {
            let _ = child.kill();
            panic!("jani did not finish within 60 seconds");
        }
        thread::sleep(Duration::from_millis(10));
    }
    let output = child.wait_with_output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "jani failed: {}", stderr.trim());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("The file says hello."), "{stdout}");
    provider.join().unwrap();

    let requests = bodies.lock().unwrap().clone();
    assert_eq!(requests.len(), 2, "expected two model turns");
    assert!(requests[0].contains("read src/one.txt"));
    assert!(requests[1].contains("hello"));
    for request in &requests {
        assert!(
            !request.contains("test-provider-secret"),
            "provider credential leaked into the model request body"
        );
    }

    let events = journal_events(&state);
    assert!(events
        .iter()
        .any(|event| { event["action"] == "repo.read" && event["decision"] == "executed" }));
    assert!(events
        .iter()
        .any(|event| { event["action"] == "agent.run" && event["decision"] == "completed" }));
    let task_id = events[0]["task_id"].as_str().unwrap().to_string();

    let audit = Command::new(env!("CARGO_BIN_EXE_jani"))
        .args(["audit", "verify", "--state-dir", state.to_str().unwrap()])
        .output()
        .expect("verify audit journal");
    assert!(audit.status.success());
    let audit_json: Value = serde_json::from_slice(&audit.stdout).unwrap();
    assert_eq!(audit_json["ok"], true);

    let budget = Command::new(env!("CARGO_BIN_EXE_jani"))
        .args([
            "budget",
            "show",
            "--task",
            &task_id,
            "--state-dir",
            state.to_str().unwrap(),
        ])
        .output()
        .expect("show budget");
    assert!(budget.status.success());
    let budget_json: Value = serde_json::from_slice(&budget.stdout).unwrap();
    assert_eq!(budget_json["toolCalls"], 1);
    assert_eq!(budget_json["inputTokens"], 32);
    assert_eq!(budget_json["outputTokens"], 13);

    let _ = std::fs::remove_dir_all(root);
}
