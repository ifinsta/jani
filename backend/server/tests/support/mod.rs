#![allow(dead_code)] // each test binary uses a different part of this module

use std::path::PathBuf;
use std::process::Command;

// Integration tests must never read the developer's real user-wide settings.
pub fn hermetic_user_settings() -> PathBuf {
    std::env::temp_dir().join(format!(
        "jani-test-no-user-settings-{}.json",
        std::process::id()
    ))
}

pub fn jani() -> Command {
    jani_with_user_settings(hermetic_user_settings())
}

pub fn jani_with_user_settings(path: PathBuf) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_jani"));
    command.env("JANI_USER_SETTINGS", path);
    command
}

/// A model that answers from a script, one reply per request, and records what it was sent.
pub mod provider {
    use serde_json::{json, Value};
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    pub struct Scripted {
        pub base: String,
        pub requests: Arc<Mutex<Vec<Value>>>,
    }

    pub fn tool(id: usize, name: &str, args: Value) -> Value {
        json!({
            "choices": [{"message": {"role": "assistant", "content": null, "tool_calls": [{
                "id": format!("call-{id}"),
                "type": "function",
                "function": {"name": name, "arguments": args.to_string()}
            }]}}],
            "usage": {"prompt_tokens": 10, "completion_tokens": 4}
        })
    }

    pub fn write(id: usize, path: &str, content: &str) -> Value {
        tool(id, "write", json!({"path": path, "content": content}))
    }

    pub fn say(text: &str) -> Value {
        json!({
            "choices": [{"message": {"role": "assistant", "content": text}}],
            "usage": {"prompt_tokens": 10, "completion_tokens": 4}
        })
    }

    /// Serves `replies` in order. Past the end it answers every request with a plain
    /// "(no more script)" so a test that over-asks fails on its own assertions.
    pub fn start(replies: Vec<Value>) -> Scripted {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let seen = requests.clone();
        std::thread::spawn(move || {
            for (next, stream) in listener.incoming().enumerate() {
                let Ok(mut stream) = stream else { return };
                let _ = stream.set_read_timeout(Some(Duration::from_secs(30)));
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
                if let Ok(value) = serde_json::from_slice::<Value>(&body) {
                    seen.lock().unwrap().push(value);
                }
                let reply = replies
                    .get(next)
                    .cloned()
                    .unwrap_or_else(|| say("(no more script)"));
                let text = reply.to_string();
                let _ = write!(
                    stream,
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                    text.len(),
                    text
                );
                let _ = stream.flush();
                let _ = stream.shutdown(std::net::Shutdown::Both);
            }
        });
        Scripted {
            base: format!("http://{address}/v1"),
            requests,
        }
    }
}
