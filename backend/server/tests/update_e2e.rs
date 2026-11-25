use jani_core::crypto;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::fs;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

const DEFAULT_SIGNING_KEY: &str =
    "1111111111111111111111111111111111111111111111111111111111111111";
const UPDATE_CREDENTIAL_ENV: &str = "JANI_UPDATE_SCRUB_TEST_KEY";

fn public_key_spec(secret_hex: &str, key_id: &str) -> String {
    format!("{key_id}:{}", crypto::public_key_hex(secret_hex).unwrap())
}

fn manifest_signing_bytes(mut manifest: Value) -> Vec<u8> {
    manifest.as_object_mut().unwrap().remove("signature");
    let mut bytes = b"jani-release-manifest/v2\0".to_vec();
    bytes.extend(serde_json::to_vec(&manifest).unwrap());
    bytes
}

fn sign_manifest(manifest: &mut Value, secret_hex: &str) {
    let value = crypto::sign_hex(secret_hex, &manifest_signing_bytes(manifest.clone())).unwrap();
    manifest["signature"] = json!({"keyId": "release-test", "value": value});
}

static NEXT_DIR: AtomicU64 = AtomicU64::new(0);

fn temp_dir(label: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "jani-update-{label}-{}-{}",
        std::process::id(),
        NEXT_DIR.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = fs::remove_dir_all(&path);
    fs::create_dir_all(&path).unwrap();
    path
}

fn host_platform() -> (&'static str, &'static str) {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("windows", "x86_64") => ("windows", "x86_64-pc-windows-msvc"),
        ("windows", "aarch64") => ("windows", "aarch64-pc-windows-msvc"),
        ("linux", "x86_64") => ("linux", "x86_64-unknown-linux-gnu"),
        ("linux", "aarch64") => ("linux", "aarch64-unknown-linux-gnu"),
        ("macos", "x86_64") => ("macos", "x86_64-apple-darwin"),
        ("macos", "aarch64") => ("macos", "aarch64-apple-darwin"),
        (os, arch) => panic!("unsupported test host {os}/{arch}"),
    }
}

fn build_stub(dir: &Path, version: &str) -> PathBuf {
    let source = dir.join(format!("stub-{version}.rs"));
    fs::write(
        &source,
        format!(
            "fn main() {{\n    let arg = std::env::args().nth(1).unwrap_or_default();\n    if arg == \"--version\" {{\n        if [\"JANI_API_KEY\", \"OPENAI_API_KEY\", \"{UPDATE_CREDENTIAL_ENV}\"]
            .iter()
            .any(|name| std::env::var_os(name).is_some())
        {{
            std::process::exit(91);
        }}\n        println!(\"jani {version}\");\n    }} else {{ std::process::exit(2); }}\n}}\n"
        ),
    )
    .unwrap();
    let binary = dir.join(format!("stub-{version}{}", std::env::consts::EXE_SUFFIX));
    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".into());
    let output = Command::new(rustc)
        .arg("--edition=2021")
        .arg("--crate-name")
        .arg("jani_stub")
        .arg("-O")
        .arg(&source)
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("run rustc to build a stand-in jani binary");
    assert!(
        output.status.success(),
        "rustc failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    binary
}

fn build_marker_stub(dir: &Path) -> PathBuf {
    let source = dir.join("marker-stub.rs");
    fs::write(
        &source,
        "fn main() {\n    if let Some(path) = std::env::var_os(\"JANI_UPDATE_TARGET_MARKER\") {\n        std::fs::write(path, b\"executed\").unwrap();\n    }\n    if std::env::args().nth(1).as_deref() == Some(\"--version\") {\n        println!(\"jani 1.0.0\");\n    } else {\n        std::process::exit(2);\n    }\n}\n",
    )
    .unwrap();
    let binary = dir.join(format!("marker-stub{}", std::env::consts::EXE_SUFFIX));
    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".into());
    let output = Command::new(rustc)
        .arg("--edition=2021")
        .arg("--crate-name")
        .arg("jani_marker_stub")
        .arg("-O")
        .arg(&source)
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("run rustc to build an execution-marker stand-in");
    assert!(
        output.status.success(),
        "rustc failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    binary
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn publish(root: &Path, version: &str, artifact: &Path) {
    publish_with_signer(root, version, artifact, DEFAULT_SIGNING_KEY);
}

fn publish_with_signer(root: &Path, version: &str, artifact: &Path, secret_hex: &str) {
    let (platform, target) = host_platform();
    let tag = format!("v{version}");
    let release_dir = root.join("agent").join(platform).join(&tag);
    fs::create_dir_all(&release_dir).unwrap();
    let binary = format!("jani-{version}-{target}{}", std::env::consts::EXE_SUFFIX);
    let bytes = fs::read(artifact).unwrap();
    let digest = sha256_hex(&bytes);
    fs::write(release_dir.join(&binary), &bytes).unwrap();
    fs::write(
        release_dir.join(format!("{binary}.sha256")),
        format!("{digest}  {binary}\n"),
    )
    .unwrap();
    let commit = "0123456789abcdef0123456789abcdef01234567";
    let mut manifest = json!({
        "schemaVersion": "jani.release/v2",
        "type": "agent",
        "platform": platform,
        "package": "jani",
        "version": version,
        "tag": tag.clone(),
        "commit": commit,
        "dirty": false,
        "builtAt": "2026-10-26T00:00:00Z",
        "artifacts": [{
            "target": target,
            "binary": binary,
            "sha256": digest.clone(),
            "sizeBytes": bytes.len(),
            "commit": commit,
            "dirty": false,
            "builtAt": "2026-10-26T00:00:00Z"
        }]
    });
    sign_manifest(&mut manifest, secret_hex);
    fs::write(
        release_dir.join("manifest.json"),
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .unwrap();

    let channel_path = root.join("agent").join(platform).join("latest.json");
    let temporary = root
        .join("agent")
        .join(platform)
        .join(format!(".latest-{}.tmp", std::process::id()));
    fs::write(
        &temporary,
        serde_json::to_vec_pretty(&json!({
            "schemaVersion": "jani.release-channel/v2",
            "type": "agent",
            "platform": platform,
            "latest": tag
        }))
        .unwrap(),
    )
    .unwrap();
    if channel_path.exists() {
        fs::remove_file(&channel_path).unwrap();
    }
    fs::rename(&temporary, &channel_path).unwrap();
}

fn tamper_artifact(root: &Path, version: &str, artifact: &Path) {
    let (platform, target) = host_platform();
    let binary = format!("jani-{version}-{target}{}", std::env::consts::EXE_SUFFIX);
    let path = root
        .join("agent")
        .join(platform)
        .join(format!("v{version}"))
        .join(binary);
    let mut bytes = fs::read(artifact).unwrap();
    bytes[0] ^= 0xff;
    fs::write(path, bytes).unwrap();
}

struct TestOrigin {
    address: SocketAddr,
    requests: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
}

impl TestOrigin {
    fn start(root: &Path) -> Self {
        Self::start_at(root, 0)
    }

    fn start_at(root: &Path, port: u16) -> Self {
        let listener = TcpListener::bind(("127.0.0.1", port)).expect("bind test origin");
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let root = root.to_path_buf();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let thread_requests = Arc::clone(&requests);
        let thread_stop = Arc::clone(&stop);
        let handle = thread::spawn(move || {
            while !thread_stop.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let _ = stream.set_nonblocking(false);
                        let _ = serve(stream, &root, &thread_requests);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2));
                    }
                    Err(_) => break,
                }
            }
        });
        Self {
            address,
            requests,
            stop,
            handle: Some(handle),
        }
    }

    fn url(&self) -> String {
        format!("http://{}", self.address)
    }

    fn artifact_requests(&self) -> usize {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .filter(|path| path.contains("/jani-"))
            .count()
    }
}

impl Drop for TestOrigin {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn serve(
    mut stream: TcpStream,
    root: &Path,
    requests: &Arc<Mutex<Vec<String>>>,
) -> std::io::Result<()> {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let request = read_request(&mut stream);
    let Some((method, path)) = request else {
        return Ok(());
    };
    let clean = path.split('?').next().unwrap_or_default();
    requests.lock().unwrap().push(clean.to_string());
    let relative = clean.trim_start_matches('/');
    let allowed = !relative.is_empty()
        && !relative.contains("..")
        && !relative.contains('\\')
        && !relative.contains(':');
    let body = if allowed {
        fs::read(root.join(relative)).ok()
    } else {
        None
    };
    let (status, body) = match body {
        Some(body) => ("200 OK", body),
        None => ("404 Not Found", b"not found".to_vec()),
    };
    let head_only = method.eq_ignore_ascii_case("HEAD");
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nContent-Type: application/octet-stream\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        body.len()
    )?;
    if !head_only {
        stream.write_all(&body)?;
    }
    stream.flush()
}

fn read_request(stream: &mut TcpStream) -> Option<(String, String)> {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let read = stream.read(&mut chunk).ok()?;
        if read == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..read]);
        if buffer.windows(4).any(|window| window == b"\r\n\r\n") || buffer.len() > 32 * 1024 {
            break;
        }
    }
    let text = String::from_utf8_lossy(&buffer);
    let mut parts = text.lines().next()?.split_whitespace();
    Some((parts.next()?.to_string(), parts.next()?.to_string()))
}

fn jani(cfg_root: &Path, state: &Path) -> Command {
    command_for(Path::new(env!("CARGO_BIN_EXE_jani")), cfg_root, state)
}

fn command_for(binary: &Path, cfg_root: &Path, state: &Path) -> Command {
    let mut command = Command::new(binary);
    command
        .env("JANI_USER_SETTINGS", cfg_root.join("no-user-settings.json"))
        .env(
            "JANI_UPDATE_KEYS",
            public_key_spec(DEFAULT_SIGNING_KEY, "release-test"),
        )
        .env("JANI_UPDATE_RETRY_BASE_MS", "1")
        .env("NO_PROXY", "127.0.0.1,localhost")
        .env("no_proxy", "127.0.0.1,localhost")
        .env_remove("JANI_API_KEY")
        .env_remove("OPENAI_API_KEY")
        .env_remove("JANI_SETTINGS")
        .env_remove("JANI_UPDATE_SOURCE")
        .arg("--allow-insecure-local-update")
        .arg("--repo")
        .arg(cfg_root)
        .arg("--state-dir")
        .arg(state);
    command
}

fn output_json(output: &Output) -> Value {
    assert!(
        output.status.success(),
        "jani update failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "update output was not JSON: {error}\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

fn version_of(path: &Path) -> Option<String> {
    let output = Command::new(path).arg("--version").output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let line = text.lines().next()?.trim();
    line.strip_prefix("jani ").map(str::to_string)
}

#[test]
fn update_check_and_install_push_a_verified_release_to_multiple_targets() {
    let root = temp_dir("fleet");
    let state = root.join("state");
    let origin_root = root.join("origin");
    let current = build_stub(&root, "1.0.0");
    let next = build_stub(&root, "9.9.9");
    let device_a = root.join(format!("device-a{}", std::env::consts::EXE_SUFFIX));
    let device_b = root.join(format!("device-b{}", std::env::consts::EXE_SUFFIX));
    fs::copy(&current, &device_a).unwrap();
    fs::copy(&current, &device_b).unwrap();
    publish(&origin_root, "9.9.9", &next);
    let origin = TestOrigin::start(&origin_root);

    let check = jani(&root, &state)
        .arg("update")
        .arg("check")
        .arg("--source")
        .arg(origin.url())
        .arg("--to")
        .arg(&device_a)
        .arg("--to")
        .arg(&device_b)
        .output()
        .expect("run update check");
    let report = output_json(&check);
    assert_eq!(report["latestVersion"], "9.9.9");
    assert_eq!(report["updateAvailable"], false);
    let targets = report["targets"].as_array().unwrap();
    assert_eq!(targets.len(), 2);
    for target in targets {
        assert_eq!(target["currentVersion"], Value::Null);
        assert_eq!(target["status"], "unknown-version");
    }
    assert_eq!(origin.artifact_requests(), 0, "check downloaded a binary");

    let forced_check = jani(&root, &state)
        .arg("update")
        .arg("check")
        .arg("--source")
        .arg(origin.url())
        .arg("--force")
        .arg("--to")
        .arg(&device_a)
        .arg("--to")
        .arg(&device_b)
        .output()
        .expect("run forced update check");
    let report = output_json(&forced_check);
    assert_eq!(report["updateAvailable"], true);
    for target in report["targets"].as_array().unwrap() {
        assert_eq!(target["status"], "update-available");
    }

    let install = jani(&root, &state)
        .arg("update")
        .arg("install")
        .arg("--source")
        .arg(origin.url())
        .arg("--force")
        .arg("--to")
        .arg(&device_a)
        .arg("--to")
        .arg(&device_b)
        .output()
        .expect("run update install");
    let report = output_json(&install);
    assert_eq!(report["status"], "installed");
    assert_eq!(report["artifactVerified"], true);
    assert_eq!(report["latestVersion"], "9.9.9");
    let targets = report["targets"].as_array().unwrap();
    assert_eq!(targets.len(), 2);
    for target in targets {
        assert_eq!(target["status"], "installed");
        assert_eq!(target["previousVersion"], Value::Null);
        assert_eq!(target["newVersion"], "9.9.9");
    }
    assert_eq!(version_of(&device_a).as_deref(), Some("9.9.9"));
    assert_eq!(version_of(&device_b).as_deref(), Some("9.9.9"));
    let receipts = fs::read_dir(state.join("updates")).unwrap().count();
    assert_eq!(receipts, 1, "install should leave one audit receipt");
    let receipt = report["receipt"].as_str().expect("receipt path");
    assert!(Path::new(receipt).is_file());

    let downloads = origin.artifact_requests();
    let again = jani(&root, &state)
        .arg("update")
        .arg("install")
        .arg("--source")
        .arg(origin.url())
        .arg("--to")
        .arg(&device_a)
        .arg("--to")
        .arg(&device_b)
        .output()
        .expect("run repeated update install");
    let report = output_json(&again);
    assert_eq!(report["status"], "up-to-date");
    assert_eq!(report["artifactVerified"], false);
    assert_eq!(
        origin.artifact_requests(),
        downloads,
        "re-downloaded an installed release"
    );

    let _ = fs::remove_dir_all(root);
}

#[test]
fn watch_installs_a_release_published_while_it_is_running() {
    let root = temp_dir("watch");
    let state = root.join("state");
    let origin_root = root.join("origin");
    let current = PathBuf::from(env!("CARGO_BIN_EXE_jani"));
    let next = build_stub(&root, "9.9.9");
    let target = root.join(format!("watched-device{}", std::env::consts::EXE_SUFFIX));
    fs::copy(&current, &target).unwrap();
    publish(&origin_root, env!("CARGO_PKG_VERSION"), &current);
    let origin = TestOrigin::start(&origin_root);

    let log = root.join("watch.out");
    let errors = root.join("watch.err");
    let stdout = fs::File::create(&log).unwrap();
    let stderr = fs::File::create(&errors).unwrap();
    let mut child: Child = jani(&root, &state)
        .arg("update")
        .arg("watch")
        .arg("--source")
        .arg(origin.url())
        .arg("--interval")
        .arg("1")
        .arg("--cycles")
        .arg("6")
        .arg("--to")
        .arg(&target)
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .spawn()
        .expect("spawn update watch");

    thread::sleep(Duration::from_millis(1500));
    assert_eq!(
        version_of(&target).as_deref(),
        Some(env!("CARGO_PKG_VERSION"))
    );
    publish(&origin_root, "9.9.9", &next);

    let deadline = Instant::now() + Duration::from_secs(30);
    while child.try_wait().unwrap().is_none() {
        if Instant::now() >= deadline {
            let _ = child.kill();
            let output = child.wait_with_output().unwrap();
            panic!(
                "watch did not exit: stdout {}\nstderr {}",
                fs::read_to_string(&log).unwrap_or_default(),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        thread::sleep(Duration::from_millis(50));
    }
    let status = child.wait().unwrap();
    assert!(
        status.success(),
        "watch failed: {}",
        fs::read_to_string(&errors).unwrap()
    );
    assert_eq!(
        version_of(&target).as_deref(),
        Some("9.9.9"),
        "watch did not apply the newly published release: {}",
        fs::read_to_string(&log).unwrap()
    );
    let stdout = fs::read_to_string(&log).unwrap();
    assert!(stdout.contains("\"action\": \"watch\""), "{stdout}");
    assert!(stdout.contains("\"status\": \"installed\""), "{stdout}");
    assert!(
        stdout.matches("\"cycle\"").count() >= 2,
        "watch did not keep polling: {stdout}"
    );
    let receipts = fs::read_dir(state.join("updates")).unwrap().count();
    assert!(receipts >= 2, "watch should record each cycle");
    assert!(
        origin.artifact_requests() >= 1,
        "watch never downloaded the release artifact"
    );

    let _ = fs::remove_dir_all(root);
}

#[test]
fn watch_survives_an_unreachable_origin_and_installs_when_it_returns() {
    let root = temp_dir("watch-outage");
    let state = root.join("state");
    let origin_root = root.join("origin");
    let current = PathBuf::from(env!("CARGO_BIN_EXE_jani"));
    let next = build_stub(&root, "9.9.9");
    let target = root.join(format!("offline-device{}", std::env::consts::EXE_SUFFIX));
    fs::copy(&current, &target).unwrap();
    publish(&origin_root, "9.9.9", &next);
    let probe = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = probe.local_addr().unwrap().port();
    drop(probe);

    let log = root.join("watch.out");
    let errors = root.join("watch.err");
    let stdout = fs::File::create(&log).unwrap();
    let stderr = fs::File::create(&errors).unwrap();
    let mut child: Child = jani(&root, &state)
        .arg("update")
        .arg("watch")
        .arg("--source")
        .arg(format!("http://127.0.0.1:{port}"))
        .arg("--interval")
        .arg("1")
        .arg("--cycles")
        .arg("8")
        .arg("--to")
        .arg(&target)
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .spawn()
        .expect("spawn update watch");

    let error_deadline = Instant::now() + Duration::from_secs(20);
    loop {
        assert!(
            child.try_wait().unwrap().is_none(),
            "watch exited while the origin was unreachable: {}",
            fs::read_to_string(&log).unwrap_or_default()
        );
        if fs::read_to_string(&log)
            .unwrap_or_default()
            .contains("\"status\": \"error\"")
        {
            break;
        }
        assert!(
            Instant::now() < error_deadline,
            "watch never reported an unreachable-origin cycle: {}",
            fs::read_to_string(&log).unwrap_or_default()
        );
        thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(
        version_of(&target).as_deref(),
        Some(env!("CARGO_PKG_VERSION"))
    );
    let _origin = TestOrigin::start_at(&origin_root, port);

    let deadline = Instant::now() + Duration::from_secs(30);
    while child.try_wait().unwrap().is_none() {
        if Instant::now() >= deadline {
            let _ = child.kill();
            panic!(
                "watch did not exit: stdout {}",
                fs::read_to_string(&log).unwrap_or_default()
            );
        }
        thread::sleep(Duration::from_millis(50));
    }
    assert!(
        child.wait().unwrap().success(),
        "watch failed: {}",
        fs::read_to_string(&errors).unwrap()
    );
    assert_eq!(version_of(&target).as_deref(), Some("9.9.9"));
    let stdout = fs::read_to_string(&log).unwrap();
    assert!(
        stdout.contains("\"status\": \"error\""),
        "unreachable cycles were not reported: {stdout}"
    );
    assert!(
        stdout.contains("\"status\": \"installed\""),
        "watch never installed after the origin returned: {stdout}"
    );

    let _ = fs::remove_dir_all(root);
}

#[test]
fn the_agent_replaces_its_own_running_executable_and_requests_a_restart() {
    let root = temp_dir("self");
    let state = root.join("state");
    let origin_root = root.join("origin");
    let next = build_stub(&root, "9.9.9");
    let agent = root.join(format!("agent{}", std::env::consts::EXE_SUFFIX));
    fs::copy(env!("CARGO_BIN_EXE_jani"), &agent).unwrap();
    publish(&origin_root, "9.9.9", &next);
    let origin = TestOrigin::start(&origin_root);

    let output = command_for(&agent, &root, &state)
        .arg("update")
        .arg("install")
        .arg("--source")
        .arg(origin.url())
        .output()
        .expect("run self-update");
    let report = output_json(&output);
    assert_eq!(report["status"], "installed");
    assert_eq!(report["artifactVerified"], true);
    let targets = report["targets"].as_array().unwrap();
    assert_eq!(targets.len(), 1);
    assert_eq!(targets[0]["status"], "installed");
    assert_eq!(targets[0]["previousVersion"], env!("CARGO_PKG_VERSION"));
    assert_eq!(targets[0]["restartRequired"], true);
    assert_eq!(version_of(&agent).as_deref(), Some("9.9.9"));

    let _ = fs::remove_dir_all(root);
}

#[test]
fn a_tampered_artifact_is_rejected_and_the_target_is_untouched() {
    let root = temp_dir("tamper");
    let state = root.join("state");
    let origin_root = root.join("origin");
    let current = PathBuf::from(env!("CARGO_BIN_EXE_jani"));
    let next = build_stub(&root, "9.9.9");
    let target = root.join(format!("guarded{}", std::env::consts::EXE_SUFFIX));
    fs::copy(&current, &target).unwrap();
    publish(&origin_root, "9.9.9", &next);
    tamper_artifact(&origin_root, "9.9.9", &next);
    let origin = TestOrigin::start(&origin_root);

    let output = jani(&root, &state)
        .arg("update")
        .arg("install")
        .arg("--source")
        .arg(origin.url())
        .arg("--to")
        .arg(&target)
        .output()
        .expect("run tampered update install");
    assert!(!output.status.success(), "tampered artifact was accepted");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("hash"), "{stderr}");
    assert_eq!(
        version_of(&target).as_deref(),
        Some(env!("CARGO_PKG_VERSION"))
    );
    let leftovers = fs::read_dir(&root)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .contains(".jani-update-")
        })
        .count();
    assert_eq!(leftovers, 0, "failed update left a temporary candidate");

    let _ = fs::remove_dir_all(root);
}

#[test]
fn update_inspection_never_executes_an_existing_target() {
    let root = temp_dir("no-target-execution");
    let state = root.join("state");
    let origin_root = root.join("origin");
    let marker = root.join("target-executed");
    let target = root.join(format!("unknown-device{}", std::env::consts::EXE_SUFFIX));
    fs::copy(build_marker_stub(&root), &target).unwrap();
    let next = build_stub(&root, "9.9.9");
    publish(&origin_root, "9.9.9", &next);
    let origin = TestOrigin::start(&origin_root);

    let check = jani(&root, &state)
        .env("JANI_UPDATE_TARGET_MARKER", &marker)
        .arg("update")
        .arg("check")
        .arg("--source")
        .arg(origin.url())
        .arg("--to")
        .arg(&target)
        .output()
        .expect("run update check");
    let report = output_json(&check);
    assert_eq!(report["updateAvailable"], false);
    assert_eq!(report["targets"][0]["status"], "unknown-version");
    assert_eq!(report["targets"][0]["currentVersion"], Value::Null);
    assert!(
        !marker.exists(),
        "update check executed the existing target"
    );

    let install = jani(&root, &state)
        .env("JANI_UPDATE_TARGET_MARKER", &marker)
        .arg("update")
        .arg("install")
        .arg("--source")
        .arg(origin.url())
        .arg("--to")
        .arg(&target)
        .output()
        .expect("run update install without force");
    let report = output_json(&install);
    assert_eq!(report["status"], "unknown-version");
    assert_eq!(report["artifactVerified"], false);
    assert!(
        !marker.exists(),
        "update install executed the existing target"
    );
    assert_eq!(origin.artifact_requests(), 0);

    let forced = jani(&root, &state)
        .env("JANI_UPDATE_TARGET_MARKER", &marker)
        .arg("update")
        .arg("check")
        .arg("--source")
        .arg(origin.url())
        .arg("--force")
        .arg("--to")
        .arg(&target)
        .output()
        .expect("run forced update check");
    let report = output_json(&forced);
    assert_eq!(report["updateAvailable"], true);
    assert!(
        !marker.exists(),
        "forced check executed the existing target"
    );
    assert_eq!(origin.artifact_requests(), 0);

    let _ = fs::remove_dir_all(root);
}

#[test]
fn update_requires_an_explicit_trusted_signer_and_rejects_tampering() {
    let root = temp_dir("manifest-trust");
    let state = root.join("state");
    let origin_root = root.join("origin");
    let next = build_stub(&root, "9.9.9");
    publish(&origin_root, "9.9.9", &next);
    let (platform, _) = host_platform();
    let manifest_path = origin_root
        .join("agent")
        .join(platform)
        .join("v9.9.9")
        .join("manifest.json");
    let signed = fs::read(&manifest_path).unwrap();
    let origin = TestOrigin::start(&origin_root);
    let target = root.join(format!("trusted-device{}", std::env::consts::EXE_SUFFIX));
    let check = |keys: Option<String>| {
        let mut command = jani(&root, &state);
        command
            .arg("update")
            .arg("check")
            .arg("--source")
            .arg(origin.url())
            .arg("--to")
            .arg(&target);
        if let Some(keys) = keys {
            command.env("JANI_UPDATE_KEYS", keys);
        } else {
            command.env_remove("JANI_UPDATE_KEYS");
        }
        command.output().expect("run update check")
    };

    let output = check(None);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("trust root is required"));

    let wrong = public_key_spec(&"22".repeat(32), "release-test");
    let output = check(Some(wrong));
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("signature"));
    assert_eq!(origin.artifact_requests(), 0);

    let mut unsigned: Value = serde_json::from_slice(&signed).unwrap();
    unsigned.as_object_mut().unwrap().remove("signature");
    fs::write(
        &manifest_path,
        serde_json::to_vec_pretty(&unsigned).unwrap(),
    )
    .unwrap();
    let output = check(Some(public_key_spec(DEFAULT_SIGNING_KEY, "release-test")));
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("unsigned"));

    let mut tampered: Value = serde_json::from_slice(&signed).unwrap();
    tampered["artifacts"][0]["sha256"] = json!("b".repeat(64));
    fs::write(
        &manifest_path,
        serde_json::to_vec_pretty(&tampered).unwrap(),
    )
    .unwrap();
    let output = check(Some(public_key_spec(DEFAULT_SIGNING_KEY, "release-test")));
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("signature"));
    assert_eq!(origin.artifact_requests(), 0);

    fs::write(&manifest_path, &signed).unwrap();
    let output = check(Some(public_key_spec(DEFAULT_SIGNING_KEY, "release-test")));
    let report = output_json(&output);
    assert_eq!(report["latestVersion"], "9.9.9");
    assert_eq!(report["manifestSignatureKey"], "release-test");
    assert_eq!(origin.artifact_requests(), 0);

    let _ = fs::remove_dir_all(root);
}

#[test]
fn update_accepts_trust_from_the_explicit_cli_flag() {
    let root = temp_dir("cli-trust");
    let state = root.join("state");
    let origin_root = root.join("origin");
    let current = PathBuf::from(env!("CARGO_BIN_EXE_jani"));
    let next = build_stub(&root, "9.9.9");
    let target = root.join(format!(
        "cli-trusted-device{}",
        std::env::consts::EXE_SUFFIX
    ));
    fs::copy(&current, &target).unwrap();
    publish(&origin_root, "9.9.9", &next);
    let origin = TestOrigin::start(&origin_root);
    let key = public_key_spec(DEFAULT_SIGNING_KEY, "release-test");

    let check = jani(&root, &state)
        .env_remove("JANI_UPDATE_KEYS")
        .arg("update")
        .arg("check")
        .arg("--source")
        .arg(origin.url())
        .arg("--update-key")
        .arg(&key)
        .arg("--to")
        .arg(&target)
        .output()
        .expect("run update check with CLI trust");
    let report = output_json(&check);
    assert_eq!(report["latestVersion"], "9.9.9");
    assert_eq!(report["manifestSignatureKey"], "release-test");
    assert_eq!(report["updateAvailable"], true);
    assert_eq!(origin.artifact_requests(), 0);

    let install = jani(&root, &state)
        .env_remove("JANI_UPDATE_KEYS")
        .arg("update")
        .arg("install")
        .arg("--source")
        .arg(origin.url())
        .arg("--update-key")
        .arg(&key)
        .arg("--to")
        .arg(&target)
        .output()
        .expect("run update install with CLI trust");
    let report = output_json(&install);
    assert_eq!(report["status"], "installed");
    assert_eq!(report["artifactVerified"], true);
    assert_eq!(
        report["targets"][0]["previousVersion"],
        env!("CARGO_PKG_VERSION")
    );
    assert_eq!(version_of(&target).as_deref(), Some("9.9.9"));

    let _ = fs::remove_dir_all(root);
}

#[test]
fn update_candidate_verification_does_not_inherit_provider_credentials() {
    let root = temp_dir("candidate-scrub");
    let state = root.join("state");
    let origin_root = root.join("origin");
    let next = build_stub(&root, "9.9.9");
    publish(&origin_root, "9.9.9", &next);
    fs::write(
        root.join("no-user-settings.json"),
        json!({
            "version": 1,
            "defaultModel": "fast",
            "models": {"fast": {"model": "fixture-model", "apiKeyEnv": UPDATE_CREDENTIAL_ENV}}
        })
        .to_string(),
    )
    .unwrap();
    let origin = TestOrigin::start(&origin_root);
    let target = root.join(format!("scrubbed-device{}", std::env::consts::EXE_SUFFIX));

    let output = jani(&root, &state)
        .env("JANI_API_KEY", "fixture-jani-update-key")
        .env("OPENAI_API_KEY", "fixture-openai-update-key")
        .env(UPDATE_CREDENTIAL_ENV, "fixture-custom-update-key")
        .arg("update")
        .arg("install")
        .arg("--source")
        .arg(origin.url())
        .arg("--to")
        .arg(&target)
        .output()
        .expect("run update install with credential guards");
    let report = output_json(&output);
    assert_eq!(report["status"], "installed");
    assert_eq!(report["targets"][0]["newVersion"], "9.9.9");
    assert_eq!(version_of(&target).as_deref(), Some("9.9.9"));
    let visible = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!visible.contains("fixture-jani-update-key"));
    assert!(!visible.contains("fixture-openai-update-key"));
    assert!(!visible.contains("fixture-custom-update-key"));

    let _ = fs::remove_dir_all(root);
}
