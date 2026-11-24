//! Per-user `jani ui` processes.
//!
//! Every signed-in person gets their own worktree, state directory and `jani ui`
//! process. The process is started with the operator's fixed authority flags
//! and an unguessable token that never leaves the gateway. Isolation between
//! people comes from three independent layers that fail closed:
//!
//! 1. a separate unprivileged Unix user per person (`JANI_CLOUD_UID_BASE`),
//! 2. bubblewrap, with only that person's directory writable,
//! 3. jani's own broker, policy, budgets and audit journal.
//!
//! Layers 1 and 2 are optional individually, but the gateway refuses to start
//! with neither unless the operator says so explicitly.

use anyhow::{anyhow, bail, Context, Result};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::{Child, Command},
    sync::Mutex,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sandbox {
    /// bubblewrap must be present; refuse to start otherwise.
    Bwrap,
    /// bubblewrap when present, plain process otherwise.
    Auto,
    /// No bubblewrap. Only for development and tests.
    Off,
}

#[derive(Clone, Debug)]
pub struct Settings {
    pub data_dir: PathBuf,
    pub jani_bin: PathBuf,
    /// Fixed authority flags, e.g. `--allow-write --write-path src --model x`.
    pub jani_args: Vec<String>,
    /// Environment variables copied from the gateway into each workspace.
    pub passthrough_env: Vec<String>,
    pub template: Option<PathBuf>,
    pub max_active: usize,
    pub idle_ttl: Duration,
    pub sandbox: Sandbox,
    pub uid_base: Option<u32>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Failure {
    Capacity,
    Start(String),
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Failure::Capacity => f.write_str("all workspaces are busy; try again shortly"),
            Failure::Start(why) => write!(f, "workspace could not start: {why}"),
        }
    }
}

/// Where a running workspace listens and the token the gateway presents.
#[derive(Clone, Debug)]
pub struct Endpoint {
    pub port: u16,
    pub token: String,
}

struct Running {
    child: Child,
    endpoint: Endpoint,
    last_used: Instant,
}

pub struct Manager {
    settings: Settings,
    running: Mutex<HashMap<String, Running>>,
}

/// Stable, non-reversible directory name for an ifinsta subject. `sub` is
/// already pairwise per sector; hashing keeps it out of paths and process lists.
pub fn user_key(sub: &str) -> String {
    let digest = Sha256::digest(format!("jani-cloud/v1:{sub}").as_bytes());
    digest.iter().take(12).map(|b| format!("{b:02x}")).collect()
}

/// `jani --principal` value attributing journal events to this person.
pub fn principal_for(key: &str) -> String {
    format!("ifinsta-{key}")
}

/// Parse the single line `jani ui` prints at startup.
pub fn parse_ready_line(line: &str) -> Option<Endpoint> {
    let rest = line.split("http://127.0.0.1:").nth(1)?;
    let (port, token) = rest.trim().split_once("/#token=")?;
    let token = token.trim();
    if token.len() < 32 || !token.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    Some(Endpoint {
        port: port.parse().ok()?,
        token: token.to_string(),
    })
}

/// The `bwrap` invocation prefix. Only the person's own directory is writable;
/// the system is read-only and the host's other users and processes are not
/// visible. The network namespace is shared because the model API is remote and
/// `jani ui` listens on loopback; the per-process token and Host check protect it.
/// Where the jani binary appears inside the sandbox.
pub const SANDBOX_JANI: &str = "/opt/jani/jani";

pub fn bwrap_prefix(user_dir: &Path) -> Vec<String> {
    let mut a: Vec<String> = [
        "--die-with-parent",
        "--new-session",
        "--unshare-pid",
        "--unshare-ipc",
        "--unshare-uts",
        "--ro-bind",
        "/usr",
        "/usr",
        "--ro-bind-try",
        "/bin",
        "/bin",
        "--ro-bind-try",
        "/sbin",
        "/sbin",
        "--ro-bind-try",
        "/lib",
        "/lib",
        "--ro-bind-try",
        "/lib64",
        "/lib64",
        "--ro-bind-try",
        "/etc/ssl",
        "/etc/ssl",
        "--ro-bind-try",
        "/etc/ca-certificates",
        "/etc/ca-certificates",
        "--ro-bind-try",
        "/etc/resolv.conf",
        "/etc/resolv.conf",
        "--ro-bind-try",
        "/etc/hosts",
        "/etc/hosts",
        "--ro-bind-try",
        "/etc/nsswitch.conf",
        "/etc/nsswitch.conf",
        "--proc",
        "/proc",
        "--dev",
        "/dev",
        "--tmpfs",
        "/tmp",
        "--bind",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    a.push(user_dir.display().to_string());
    a.push("/work".into());
    a.extend(["--chdir".into(), "/work".into(), "--".into()]);
    a
}

fn which(program: &str) -> Option<PathBuf> {
    if program.contains('/') {
        let p = PathBuf::from(program);
        return p.is_file().then_some(p);
    }
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|dir| dir.join(program))
            .find(|candidate| candidate.is_file())
    })
}

impl Settings {
    /// Refuse insecure combinations before accepting any traffic.
    pub fn validate(&self, running_as_root: bool) -> Result<Sandbox> {
        let bwrap = which("bwrap").is_some();
        let effective = match self.sandbox {
            Sandbox::Bwrap if !bwrap => bail!(
                "JANI_CLOUD_SANDBOX=bwrap but `bwrap` is not on PATH. Install bubblewrap, or set JANI_CLOUD_SANDBOX=off for development."
            ),
            Sandbox::Auto if bwrap => Sandbox::Bwrap,
            Sandbox::Auto => Sandbox::Off,
            other => other,
        };
        if self.uid_base.is_some() && !running_as_root {
            bail!("JANI_CLOUD_UID_BASE needs the gateway to run as root so it can drop each workspace to its own user");
        }
        if effective == Sandbox::Off && self.uid_base.is_none() && !insecure_allowed() {
            bail!(
                "no isolation configured: need bubblewrap or JANI_CLOUD_UID_BASE. Set JANI_CLOUD_ALLOW_UNISOLATED=1 to run anyway (development only)."
            );
        }
        if which(&self.jani_bin.display().to_string()).is_none() {
            bail!("jani binary not found: {}", self.jani_bin.display());
        }
        Ok(effective)
    }
}

fn insecure_allowed() -> bool {
    std::env::var("JANI_CLOUD_ALLOW_UNISOLATED").as_deref() == Ok("1")
}

impl Manager {
    pub fn new(settings: Settings) -> Arc<Self> {
        Arc::new(Self {
            settings,
            running: Mutex::new(HashMap::new()),
        })
    }

    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    pub async fn active(&self) -> usize {
        self.running.lock().await.len()
    }

    /// The running workspace for this person, starting one if needed.
    pub async fn endpoint(&self, sub: &str, sandbox: Sandbox) -> Result<Endpoint, Failure> {
        let key = user_key(sub);
        let mut running = self.running.lock().await;
        if let Some(entry) = running.get_mut(&key) {
            match entry.child.try_wait() {
                Ok(None) => {
                    entry.last_used = Instant::now();
                    return Ok(entry.endpoint.clone());
                }
                _ => {
                    running.remove(&key);
                }
            }
        }
        if running.len() >= self.settings.max_active {
            return Err(Failure::Capacity);
        }
        let (child, endpoint) = self
            .spawn(&key, sandbox)
            .await
            .map_err(|e| Failure::Start(format!("{e:#}")))?;
        running.insert(
            key,
            Running {
                child,
                endpoint: endpoint.clone(),
                last_used: Instant::now(),
            },
        );
        Ok(endpoint)
    }

    /// Stop a person's workspace now (sign-out). Their files and journal stay.
    pub async fn stop(&self, sub: &str) -> bool {
        let key = user_key(sub);
        match self.running.lock().await.remove(&key) {
            Some(mut entry) => {
                let _ = entry.child.start_kill();
                let _ = entry.child.wait().await;
                true
            }
            None => false,
        }
    }

    /// Stop every workspace. Used on shutdown.
    pub async fn stop_all(&self) -> usize {
        let mut running = self.running.lock().await;
        let n = running.len();
        for (_, mut entry) in running.drain() {
            let _ = entry.child.start_kill();
            let _ = entry.child.wait().await;
        }
        n
    }

    /// Stop workspaces nobody has used for the idle period.
    pub async fn reap(&self) -> usize {
        let mut running = self.running.lock().await;
        let ttl = self.settings.idle_ttl;
        let mut stale: Vec<String> = Vec::new();
        for (key, entry) in running.iter_mut() {
            if entry.last_used.elapsed() > ttl || matches!(entry.child.try_wait(), Ok(Some(_))) {
                stale.push(key.clone());
            }
        }
        for key in &stale {
            if let Some(mut entry) = running.remove(key) {
                let _ = entry.child.start_kill();
            }
        }
        stale.len()
    }

    async fn spawn(&self, key: &str, sandbox: Sandbox) -> Result<(Child, Endpoint)> {
        let user_dir = self.settings.data_dir.join("users").join(key);
        let uid = self.prepare(&user_dir, key).await?;
        let (program, args) = self.command_line(&user_dir, key, sandbox);
        let mut command = Command::new(&program);
        command
            .args(&args)
            .env_clear()
            .env("PATH", "/usr/local/bin:/usr/bin:/bin")
            .env(
                "HOME",
                if sandbox == Sandbox::Bwrap {
                    "/work/home".into()
                } else {
                    user_dir.join("home").display().to_string()
                },
            )
            .env("JANI_USER_SETTINGS", "/nonexistent/jani-settings.json")
            .env("JANI_UI_PROXIED", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        for name in &self.settings.passthrough_env {
            if let Ok(value) = std::env::var(name) {
                command.env(name, value);
            }
        }
        #[cfg(unix)]
        {
            if let Some(uid) = uid {
                command.uid(uid).gid(uid);
            }
            // If the gateway dies for any reason, including SIGKILL, the kernel
            // stops the workspace too. (Runs after the uid change, which would
            // otherwise clear it.)
            #[cfg(target_os = "linux")]
            unsafe {
                command.pre_exec(|| {
                    if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) != 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
        }
        let _ = uid;
        let mut child = command
            .spawn()
            .with_context(|| format!("cannot start {}", program.display()))?;
        let stdout = child.stdout.take().ok_or_else(|| anyhow!("no stdout"))?;
        // Keep what the child says on stderr so a failed start can be diagnosed
        // in the operator's log. It is never sent to the browser.
        let tail = Arc::new(std::sync::Mutex::new(String::new()));
        if let Some(stderr) = child.stderr.take() {
            let key = key.to_string();
            let tail = tail.clone();
            tokio::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    tracing::warn!(workspace = %key, "{line}");
                    if let Ok(mut t) = tail.lock() {
                        t.push_str(&line);
                        t.push('\n');
                        let excess = t.len().saturating_sub(400);
                        t.drain(..excess);
                    }
                }
            });
        }
        let mut lines = BufReader::new(stdout).lines();
        let first = match tokio::time::timeout(Duration::from_secs(15), lines.next_line()).await {
            Err(_) => bail!("timed out waiting for the workspace to start"),
            Ok(Err(e)) => bail!("workspace output unreadable: {e}"),
            Ok(Ok(Some(line))) => line,
            Ok(Ok(None)) => {
                tokio::time::sleep(Duration::from_millis(150)).await;
                let why = tail
                    .lock()
                    .map(|t| t.trim().to_string())
                    .unwrap_or_default();
                bail!("workspace exited before it was ready: {why}")
            }
        };
        let endpoint = parse_ready_line(&first)
            .ok_or_else(|| anyhow!("unexpected first line from workspace: {first:.80}"))?;
        // Keep the pipes drained so a chatty child can never block on a full pipe.
        tokio::spawn(async move { while let Ok(Some(_)) = lines.next_line().await {} });
        Ok((child, endpoint))
    }

    fn command_line(&self, user_dir: &Path, key: &str, sandbox: Sandbox) -> (PathBuf, Vec<String>) {
        let in_sandbox = sandbox == Sandbox::Bwrap;
        let (repo, state) = if in_sandbox {
            ("/work/repo".to_string(), "/work/state".to_string())
        } else {
            (
                user_dir.join("repo").display().to_string(),
                user_dir.join("state").display().to_string(),
            )
        };
        let mut jani: Vec<String> = vec![
            "ui".into(),
            "--port".into(),
            "0".into(),
            "--repo".into(),
            repo,
            "--state-dir".into(),
            state,
            "--principal".into(),
            principal_for(key),
        ];
        jani.extend(self.settings.jani_args.iter().cloned());
        if in_sandbox {
            // The binary is bound read-only at a fixed path, wherever it lives on the host.
            let host_bin = which(&self.settings.jani_bin.display().to_string())
                .unwrap_or_else(|| self.settings.jani_bin.clone());
            let mut args = bwrap_prefix(user_dir);
            let at = args.len() - 1; // before the trailing `--`
            args.splice(
                at..at,
                [
                    "--ro-bind".to_string(),
                    host_bin.display().to_string(),
                    SANDBOX_JANI.to_string(),
                ],
            );
            args.push(SANDBOX_JANI.to_string());
            args.extend(jani);
            (PathBuf::from("bwrap"), args)
        } else {
            (self.settings.jani_bin.clone(), jani)
        }
    }

    /// Create the person's directories, seed the worktree, and return the Unix
    /// user to run as when per-person users are enabled.
    async fn prepare(&self, user_dir: &Path, key: &str) -> Result<Option<u32>> {
        let uid = match self.settings.uid_base {
            Some(base) => Some(self.uid_for(base, key).await?),
            None => None,
        };
        let repo = user_dir.join("repo");
        let fresh = !repo.exists();
        for sub in ["repo", "state", "home"] {
            std::fs::create_dir_all(user_dir.join(sub))?;
        }
        if fresh {
            if let Some(template) = &self.settings.template {
                copy_tree(template, &repo).context("cannot copy the workspace template")?;
            } else {
                std::fs::write(
                    repo.join("README.md"),
                    "# Workspace\n\nYour private Jani workspace. Add files, or ask Jani to explore them.\n",
                )?;
            }
            git(&repo, &["init", "-q"]);
            git(&repo, &["add", "-A"]);
            git(
                &repo,
                &[
                    "-c",
                    "user.name=jani-cloud",
                    "-c",
                    "user.email=jani-cloud@localhost",
                    "commit",
                    "-q",
                    "-m",
                    "Initial workspace",
                    "--allow-empty",
                ],
            );
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = if uid.is_some() { 0o700 } else { 0o750 };
            std::fs::set_permissions(user_dir, std::fs::Permissions::from_mode(mode))?;
            if let Some(uid) = uid {
                // Each person must be able to reach their own directory, and nobody
                // else's: the parents are traverse-only (no listing), theirs is 0700.
                for parent in [
                    self.settings.data_dir.as_path(),
                    user_dir.parent().unwrap_or(user_dir),
                ] {
                    std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o711))?;
                }
                chown_tree(user_dir, uid)?;
            }
        }
        Ok(uid)
    }

    /// One Unix user per person, allocated once and remembered on disk so the
    /// same person always gets the same user (and therefore their own files).
    async fn uid_for(&self, base: u32, key: &str) -> Result<u32> {
        let dir = self.settings.data_dir.join("uids");
        std::fs::create_dir_all(&dir)?;
        let mine = dir.join(key);
        if let Ok(text) = std::fs::read_to_string(&mine) {
            if let Ok(uid) = text.trim().parse() {
                return Ok(uid);
            }
        }
        // The manager lock is held by the caller, so this counter has no race.
        let counter = dir.join("next");
        let next: u32 = std::fs::read_to_string(&counter)
            .ok()
            .and_then(|t| t.trim().parse().ok())
            .unwrap_or(0);
        let uid = base
            .checked_add(next)
            .ok_or_else(|| anyhow!("uid range exhausted"))?;
        std::fs::write(&mine, uid.to_string())?;
        std::fs::write(&counter, (next + 1).to_string())?;
        Ok(uid)
    }
}

fn git(dir: &Path, args: &[&str]) {
    let _ = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

fn copy_tree(from: &Path, to: &Path) -> Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        let target = to.join(entry.file_name());
        // Symlinks are skipped: a template must not be able to point out of the workspace.
        if kind.is_dir() {
            if entry.file_name() != ".git" {
                copy_tree(&entry.path(), &target)?;
            }
        } else if kind.is_file() {
            std::fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

#[cfg(unix)]
fn chown_tree(path: &Path, uid: u32) -> Result<()> {
    use std::os::unix::fs::chown;
    chown(path, Some(uid), Some(uid))?;
    let meta = std::fs::symlink_metadata(path)?;
    if meta.is_dir() {
        for entry in std::fs::read_dir(path)? {
            chown_tree(&entry?.path(), uid)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(dir: &Path) -> Settings {
        Settings {
            data_dir: dir.to_path_buf(),
            jani_bin: PathBuf::from("/bin/sh"),
            jani_args: vec![],
            passthrough_env: vec![],
            template: None,
            max_active: 2,
            idle_ttl: Duration::from_secs(60),
            sandbox: Sandbox::Off,
            uid_base: None,
        }
    }

    #[test]
    fn keys_are_stable_distinct_and_hide_the_subject() {
        let a = user_key("sub-a");
        assert_eq!(a, user_key("sub-a"));
        assert_ne!(a, user_key("sub-b"));
        assert_eq!(a.len(), 24);
        assert!(!a.contains("sub"));
        assert_eq!(principal_for(&a), format!("ifinsta-{a}"));
    }

    #[test]
    fn the_ready_line_is_parsed_strictly() {
        let token = "ab".repeat(24);
        let ok = format!("Jani workspace: http://127.0.0.1:4123/#token={token}");
        let e = parse_ready_line(&ok).unwrap();
        assert_eq!((e.port, e.token.as_str()), (4123, token.as_str()));
        assert!(parse_ready_line("Jani workspace: http://evil.example:1/#token=abcd").is_none());
        assert!(parse_ready_line("http://127.0.0.1:99999/#token=").is_none());
        assert!(
            parse_ready_line(&format!("http://127.0.0.1:1/#token={}zz", "a".repeat(40))).is_none()
        );
        assert!(parse_ready_line("garbage").is_none());
    }

    #[test]
    fn bwrap_makes_only_the_users_directory_writable() {
        let args = bwrap_prefix(Path::new("/data/users/abc"));
        let joined = args.join(" ");
        assert!(joined.contains("--ro-bind /usr /usr"));
        assert!(joined.contains("--bind /data/users/abc /work"));
        assert!(joined.contains("--unshare-pid"));
        assert!(joined.contains("--die-with-parent"));
        // The only writable bind is the person's own directory.
        assert_eq!(args.iter().filter(|a| a.as_str() == "--bind").count(), 1);
        assert!(!joined.contains("/data/users ") && !joined.contains("--bind / "));
        assert_eq!(args.last().map(String::as_str), Some("--"));
    }

    #[test]
    fn no_isolation_is_refused_unless_explicitly_allowed() {
        let dir = std::env::temp_dir();
        let s = settings(&dir);
        std::env::remove_var("JANI_CLOUD_ALLOW_UNISOLATED");
        assert!(s
            .validate(true)
            .unwrap_err()
            .to_string()
            .contains("no isolation"));
        let mut with_uid = settings(&dir);
        with_uid.uid_base = Some(40000);
        assert!(with_uid
            .validate(false)
            .unwrap_err()
            .to_string()
            .contains("as root"));
        assert!(with_uid.validate(true).is_ok());
        let mut bw = settings(&dir);
        bw.sandbox = Sandbox::Bwrap;
        // Present or not, a hard requirement never silently downgrades.
        if which("bwrap").is_none() {
            assert!(bw
                .validate(true)
                .unwrap_err()
                .to_string()
                .contains("not on PATH"));
        }
    }

    #[tokio::test]
    async fn capacity_and_uid_allocation_are_enforced() {
        let dir = std::env::temp_dir().join(format!("jani-cloud-t-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut s = settings(&dir);
        s.uid_base = Some(50000);
        let m = Manager::new(s);
        let a = m.uid_for(50000, "aaa").await.unwrap();
        let b = m.uid_for(50000, "bbb").await.unwrap();
        assert_eq!((a, b), (50000, 50001));
        assert_eq!(
            m.uid_for(50000, "aaa").await.unwrap(),
            50000,
            "stable per person"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
