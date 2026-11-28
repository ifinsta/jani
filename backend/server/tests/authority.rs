//! Signed authorization boundaries exercised against a loopback-only model.
use jani_core::crypto::{public_key_hex, sign_hex};
use jani_core::identity::{IdentityAssertion, IdentitySignature};
use jani_core::policy::{PolicyBundle, PolicySignature};
use jani_core::types::{new_id, now_unix};
use serde_json::{json, Value};
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

mod support;

struct Fixture {
    root: PathBuf,
    signer: String,
    public: String,
}

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(new_id("jani-authority"));
        fs::create_dir(&root).unwrap();
        let signer = "63".repeat(32);
        let public = public_key_hex(&signer).unwrap();
        Self {
            root,
            signer,
            public,
        }
    }

    fn identity(&self, key_id: &str, expires_at: u64) -> PathBuf {
        let mut assertion: IdentityAssertion = serde_json::from_value(json!({
            "apiVersion": "jani.dev/v1alpha1", "kind": "IdentityAssertion",
            "principal": "worker", "tenant": "org", "subjectType": "worker",
            "issuer": "fixture", "issuedAt": now_unix(), "expiresAt": expires_at, "nonce": "fixture-nonce",
        })).unwrap();
        assertion.signature = Some(IdentitySignature {
            key_id: key_id.into(),
            value: sign_hex(&self.signer, &assertion.signing_bytes().unwrap()).unwrap(),
        });
        let path = self.root.join("identity.json");
        fs::write(&path, serde_json::to_vec(&assertion).unwrap()).unwrap();
        path
    }

    fn policy(&self, expires_at: u64) -> PathBuf {
        let mut policy: PolicyBundle = serde_json::from_value(json!({
            "apiVersion": "jani.dev/v1alpha1", "kind": "OrganizationPolicy",
            "metadata": {"organization": "org", "revision": "fixture", "tenant": "org",
              "issuer": "fixture", "issuedAt": now_unix(), "expiresAt": expires_at},
            "spec": {"data": {"sourceDefault": "INTERNAL", "allowedModelClasses": ["PUBLIC", "INTERNAL"],
              "allowedProviders": ["*"], "allowedRegions": ["*"], "allowedPurposes": ["SOFTWARE_DEV"]}},
        })).unwrap();
        policy.signature = Some(PolicySignature {
            key_id: "policy".into(),
            value: sign_hex(&self.signer, &policy.signing_bytes().unwrap()).unwrap(),
        });
        let path = self.root.join("policy.json");
        fs::write(&path, serde_json::to_vec(&policy).unwrap()).unwrap();
        path
    }

    fn command(&self, model: &Model) -> Command {
        let mut command = support::jani();
        command
            .args([
                "run",
                "bounded test fixture",
                "--tenant",
                "org",
                "--principal",
                "worker",
                "--purpose",
                "SOFTWARE_DEV",
                "--mode",
                "local",
                "--protocol",
                "openai",
                "--task",
                "task-fixture",
            ])
            .arg("--repo")
            .arg(&self.root)
            .arg("--state-dir")
            .arg(self.root.join("state"))
            .env("JANI_API_BASE", &model.base)
            .env("JANI_API_KEY", "loopback-fixture-key")
            .env("JANI_MODEL", "loopback")
            .env("JANI_MODEL_RETRY_BASE_MS", "1");
        for variable in [
            "OPENAI_API_KEY",
            "JANI_POLICY",
            "JANI_POLICY_KEYS",
            "JANI_IDENTITY",
            "JANI_IDENTITY_KEYS",
            "JANI_PLUGIN_KEYS",
            "JANI_REVOKED_KEYS",
            "JANI_SETTINGS",
            "JANI_CONTRACT",
        ] {
            command.env_remove(variable);
        }
        command
    }

    fn journal(&self) -> Vec<Value> {
        fs::read_to_string(self.root.join("state/journal.jsonl"))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

struct Model {
    base: String,
    seen: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl Model {
    fn new(reply_after: u64, status: &str, body: Value) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let base = format!("http://{}/v1", listener.local_addr().unwrap());
        let stop = Arc::new(AtomicBool::new(false));
        let seen = Arc::new(AtomicUsize::new(0));
        let stopped = stop.clone();
        let requests = seen.clone();
        let body = body.to_string();
        let response = format!("HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len());
        let worker = thread::spawn(move || {
            while !stopped.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream.set_nonblocking(false).unwrap();
                        stream
                            .set_read_timeout(Some(Duration::from_secs(5)))
                            .unwrap();
                        drain_request(&mut stream);
                        requests.fetch_add(1, Ordering::SeqCst);
                        while now_unix() < reply_after && !stopped.load(Ordering::SeqCst) {
                            thread::sleep(Duration::from_millis(10));
                        }
                        let _ = stream.write_all(response.as_bytes());
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10))
                    }
                    Err(error) => panic!("fixture provider accept failed: {error}"),
                }
            }
        });
        Self {
            base,
            seen,
            stop,
            worker: Some(worker),
        }
    }
}

impl Drop for Model {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let joined = self.worker.take().unwrap().join();
        if !thread::panicking() {
            joined.unwrap();
        }
    }
}

fn drain_request(stream: &mut TcpStream) {
    let mut reader = BufReader::new(stream);
    let mut length = 0;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap() == 0 || line.trim().is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            if name.eq_ignore_ascii_case("content-length") {
                length = value.trim().parse().unwrap();
            }
        }
    }
    reader.read_exact(&mut vec![0; length]).unwrap();
}

#[test]
fn identity_expiry_stops_transport_retries() {
    let fixture = Fixture::new();
    let deadline = now_unix() + 12;
    let identity = fixture.identity("identity", deadline);
    let model = Model::new(
        deadline,
        "503 Service Unavailable",
        json!({"error": "fixture retry"}),
    );
    let output = fixture
        .command(&model)
        .arg("--identity")
        .arg(identity)
        .arg("--identity-key")
        .arg(format!("identity:{}", fixture.public))
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success() && stderr.contains("identity assertion has expired"),
        "{stderr}"
    );
    assert_eq!(model.seen.load(Ordering::SeqCst), 1);
    assert!(fixture
        .journal()
        .iter()
        .any(|event| event["reason_code"] == "identity_expired"));
}

#[test]
fn late_policy_reply_is_withheld_but_usage_is_recorded() {
    let fixture = Fixture::new();
    let deadline = now_unix() + 12;
    let policy = fixture.policy(deadline);
    let model = Model::new(
        deadline,
        "200 OK",
        json!({
            "choices": [{"message": {"role": "assistant", "content": "late fixture answer"}}],
            "usage": {"prompt_tokens": 37, "completion_tokens": 11}
        }),
    );
    let output = fixture
        .command(&model)
        .arg("--policy")
        .arg(policy)
        .arg("--policy-key")
        .arg(format!("policy:{}", fixture.public))
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success() && stderr.contains("policy bundle has expired"),
        "{stderr}"
    );
    assert!(!String::from_utf8_lossy(&output.stdout).contains("late fixture answer"));
    assert_eq!(model.seen.load(Ordering::SeqCst), 1);
    let journal = fixture.journal();
    assert!(journal
        .iter()
        .any(|event| event["reason_code"] == "policy_expired"));
    assert!(journal
        .iter()
        .all(|event| event["reason_code"] != "final_answer"));
    let ledger = fs::read_dir(fixture.root.join("state/budgets"))
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| path.extension().is_some_and(|ext| ext == "json"))
        .unwrap();
    let usage: Value = serde_json::from_slice(&fs::read(ledger).unwrap()).unwrap();
    assert_eq!(usage["inputTokens"], 37);
    assert_eq!(usage["outputTokens"], 11);
}

#[test]
fn revoked_identity_material_is_denied_across_configured_key_domains() {
    let fixture = Fixture::new();
    let identity = fixture.identity("alias", now_unix() + 60);
    let model = Model::new(0, "200 OK", support::provider::say("unreachable"));
    let output = fixture
        .command(&model)
        .arg("--identity")
        .arg(identity)
        .arg("--identity-key")
        .arg(format!("alias:{}", fixture.public))
        .arg("--plugin-key")
        .arg(format!("old:{}", fixture.public))
        .args(["--revoked-key", "old"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("revoked by public key fingerprint"));
    let output = fixture
        .command(&model)
        .arg("--identity")
        .arg(fixture.root.join("identity.json"))
        .arg("--identity-key")
        .arg(format!(
            "alias:{}",
            public_key_hex(&"42".repeat(32)).unwrap()
        ))
        .arg("--plugin-key")
        .arg(format!("alias:{}", fixture.public))
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("signature verification failed"));
    assert_eq!(model.seen.load(Ordering::SeqCst), 0);
}
