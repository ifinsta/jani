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
            "fn main() {{\n    let arg = std::env::args().nth(1).unwrap_or_default();\n    if arg == \"--version\" {{ println!(\"jani {version}\"); }} else {{ std::process::exit(2); }}\n}}\n"
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
    let manifest = json!({
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
        .env("JANI_UPDATE_RETRY_BASE_MS", "1")
        .env("NO_PROXY", "127.0.0.1,localhost")
        .env("no_proxy", "127.0.0.1,localhost")
        .env_remove("JANI_UPDATE_SOURCE")
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
    assert_eq!(report["updateAvailable"], true);
    let targets = report["targets"].as_array().unwrap();
    assert_eq!(targets.len(), 2);
    for target in targets {
        assert_eq!(target["currentVersion"], "1.0.0");
        assert_eq!(target["status"], "update-available");
    }
    assert_eq!(origin.artifact_requests(), 0, "check downloaded a binary");

    let install = jani(&root, &state)
        .arg("update")
        .arg("install")
        .arg("--source")
        .arg(origin.url())
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
        assert_eq!(target["previousVersion"], "1.0.0");
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
    let current = build_stub(&root, "1.0.0");
    let next = build_stub(&root, "9.9.9");
    let target = root.join(format!("watched-device{}", std::env::consts::EXE_SUFFIX));
    fs::copy(&current, &target).unwrap();
    publish(&origin_root, "1.0.0", &current);
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
    assert_eq!(version_of(&target).as_deref(), Some("1.0.0"));
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
    let current = build_stub(&root, "1.0.0");
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
    assert_eq!(version_of(&target).as_deref(), Some("1.0.0"));
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
    let current = build_stub(&root, "1.0.0");
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
    assert_eq!(version_of(&target).as_deref(), Some("1.0.0"));
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
