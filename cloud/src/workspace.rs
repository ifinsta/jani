//! Per-user `jani ui` processes.
//!
//! Every signed-in person gets their own worktree, state directory and `jani ui`
//! process. The process is started with the operator's fixed authority flags and
//! an unguessable token that never leaves the gateway, and it listens on a Unix
//! socket in the person's own directory, so there is no network port for another
//! process to find. Isolation between people comes from independent layers that
//! fail closed:
//!
//! 1. a separate unprivileged Unix user per person (`JANI_CLOUD_UID_BASE`),
//! 2. bubblewrap, with only that person's directory writable, and, by default,
//!    no network of its own: outbound traffic goes through an allowlist proxy,
//! 3. resource limits (cgroups and rlimits) and a disk quota,
//! 4. jani's own broker, policy, budgets and audit journal.

use crate::cgroup::{self, Cgroups, Joined, Limits, PreExec};
use crate::egress::{self, Allow, Rule};
use anyhow::{anyhow, bail, Context, Result};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NetMode {
    /// The workspace has no network namespace of its own (shares the host's).
    Shared,
    /// Its own empty network namespace; outbound only through the allowlist proxy.
    Isolated,
    /// Isolated when bubblewrap is in use, otherwise shared.
    Auto,
}

#[derive(Clone, Debug)]
pub struct Settings {
    pub data_dir: PathBuf,
    pub jani_bin: PathBuf,
    /// This gateway's own binary, run as the in-sandbox relay.
    pub self_bin: PathBuf,
    /// Fixed authority flags, e.g. `--allow-write --write-path src --model x`.
    pub jani_args: Vec<String>,
    /// Environment variables copied from the gateway into each workspace.
    pub passthrough_env: Vec<String>,
    pub template: Option<PathBuf>,
    /// A Jani settings file for every workspace (permission rules, model profiles).
    pub settings_file: Option<PathBuf>,
    pub max_active: usize,
    pub idle_ttl: Duration,
    pub sandbox: Sandbox,
    pub uid_base: Option<u32>,
    pub net: NetMode,
    /// Extra egress destinations beyond the model provider, as `host:port`.
    pub egress_allow: Vec<String>,
    pub limits: Limits,
    pub quotas: cgroup::Mode,
    pub disk_check: Duration,
}

/// What the host and configuration resolved to at startup.
#[derive(Clone, Debug)]
pub struct Plan {
    pub sandbox: Sandbox,
    pub isolated_net: bool,
    pub egress: Arc<Allow>,
    pub cgroups: Option<Cgroups>,
    pub notes: Vec<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Failure {
    Capacity,
    OverQuota { used_mb: u64, limit_mb: u64 },
    Start(String),
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Failure::Capacity => f.write_str("all workspaces are busy; try again shortly"),
            Failure::OverQuota { used_mb, limit_mb } => write!(
                f,
                "workspace storage {used_mb} MB is over its {limit_mb} MB limit"
            ),
            Failure::Start(why) => write!(f, "workspace could not start: {why}"),
        }
    }
}

/// Where a running workspace listens and the token the gateway presents.
#[derive(Clone, Debug)]
pub struct Endpoint {
    pub socket: PathBuf,
    pub token: String,
}

struct Running {
    child: Child,
    endpoint: Endpoint,
    last_used: Instant,
    joined: Joined,
    egress: Option<tokio::task::JoinHandle<()>>,
    egress_socket: Option<PathBuf>,
}

impl Running {
    fn shutdown(&mut self) {
        let _ = self.child.start_kill();
        if let Some(task) = self.egress.take() {
            task.abort();
        }
        if let Some(sock) = self.egress_socket.take() {
            let _ = std::fs::remove_file(sock);
        }
    }
}

pub struct Manager {
    settings: Settings,
    plan: Plan,
    running: Mutex<HashMap<String, Running>>,
    over_quota: Mutex<HashSet<String>>,
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

/// Parse the single line `jani ui` prints at startup for a Unix socket:
/// `Jani workspace: unix:/path/to.sock/#token=HEX`.
pub fn parse_ready_line(line: &str) -> Option<(String, String)> {
    let rest = line.split("unix:").nth(1)?;
    let (path, token) = rest.trim().split_once("/#token=")?;
    let token = token.trim();
    if token.len() < 32 || !token.bytes().all(|b| b.is_ascii_hexdigit()) || !path.starts_with('/') {
        return None;
    }
    Some((path.to_string(), token.to_string()))
}

/// Where the jani binary and the relay appear inside the sandbox.
pub const SANDBOX_JANI: &str = "/opt/jani/jani";
pub const SANDBOX_SETTINGS: &str = "/opt/jani/settings.json";
pub const SANDBOX_RELAY: &str = "/opt/jani/jani-cloud";
pub const SANDBOX_EGRESS: &str = "/run/egress.sock";
pub const SANDBOX_PROXY_LISTEN: &str = "127.0.0.1:3128";

/// The `bwrap` invocation prefix. Only the person's own directory is writable;
/// the system is read-only and the host's other users and processes are not
/// visible. With `egress_socket` the workspace also gets an empty network
/// namespace and one way out: the allowlist proxy's socket.
pub fn bwrap_prefix(user_dir: &Path, egress_socket: Option<&Path>) -> Vec<String> {
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
    if let Some(sock) = egress_socket {
        a.extend([
            "--unshare-net".into(),
            "--ro-bind".into(),
            sock.display().to_string(),
            SANDBOX_EGRESS.into(),
        ]);
    }
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

fn insecure_allowed() -> bool {
    std::env::var("JANI_CLOUD_ALLOW_UNISOLATED").as_deref() == Ok("1")
}

/// The model destination implied by `--api-base` in the fixed jani arguments.
fn api_base_rule(jani_args: &[String]) -> Option<Rule> {
    let at = jani_args.iter().position(|a| a == "--api-base")?;
    Allow::from_api_base(jani_args.get(at + 1)?)
}

impl Settings {
    /// Resolve and validate against this host. Refuses insecure combinations
    /// before any traffic is accepted.
    pub fn plan(&self, running_as_root: bool) -> Result<Plan> {
        let bwrap = which("bwrap").is_some();
        let sandbox = match self.sandbox {
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
        if sandbox == Sandbox::Off && self.uid_base.is_none() && !insecure_allowed() {
            bail!(
                "no isolation configured: need bubblewrap or JANI_CLOUD_UID_BASE. Set JANI_CLOUD_ALLOW_UNISOLATED=1 to run anyway (development only)."
            );
        }
        if which(&self.jani_bin.display().to_string()).is_none() {
            bail!("jani binary not found: {}", self.jani_bin.display());
        }
        if let Some(file) = &self.settings_file {
            if !file.is_file() {
                bail!("JANI_CLOUD_SETTINGS does not exist: {}", file.display());
            }
        }
        if self.jani_args.iter().any(|a| a == "--settings") {
            bail!("pass the settings file with JANI_CLOUD_SETTINGS, not --settings: inside the sandbox it must be mounted");
        }
        let isolated_net = match self.net {
            NetMode::Isolated if sandbox != Sandbox::Bwrap => {
                bail!(
                    "JANI_CLOUD_NET=isolated needs bubblewrap (a network namespace per workspace)"
                )
            }
            NetMode::Isolated => true,
            NetMode::Auto => sandbox == Sandbox::Bwrap,
            NetMode::Shared => false,
        };
        let mut rules: Vec<Rule> = api_base_rule(&self.jani_args).into_iter().collect();
        for text in &self.egress_allow {
            rules.push(Rule::parse(text)?);
        }
        if isolated_net && rules.is_empty() {
            bail!("the network is isolated but no destination is allowed: pass --api-base in JANI_CLOUD_JANI_ARGS or set JANI_CLOUD_EGRESS_ALLOW");
        }
        let cgroups = if self.quotas == cgroup::Mode::Off {
            None
        } else {
            Cgroups::detect()
        };
        if let Some(cg) = &cgroups {
            let stale = cg.cleanup_stale();
            if stale > 0 {
                tracing::info!(
                    removed = stale,
                    "removed empty cgroups left by an earlier run"
                );
            }
        }
        let mut notes = cgroup::plan(
            self.quotas,
            &self.limits,
            cgroups.as_ref(),
            self.uid_base.is_some(),
        )?;
        if !isolated_net {
            notes.push(
                "network is SHARED: workspaces can reach any destination the container can".into(),
            );
        }
        Ok(Plan {
            sandbox,
            isolated_net,
            egress: Arc::new(Allow::new(rules)),
            cgroups,
            notes,
        })
    }
}

/// Bytes on disk under a directory, without following symlinks.
pub fn dir_usage(path: &Path) -> u64 {
    use std::os::unix::fs::MetadataExt;
    let mut total = 0u64;
    let mut stack = vec![path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(meta) = std::fs::symlink_metadata(entry.path()) else {
                continue;
            };
            total += meta.blocks() * 512;
            if meta.is_dir() {
                stack.push(entry.path());
            }
        }
    }
    total
}

impl Manager {
    pub fn new(settings: Settings, plan: Plan) -> Arc<Self> {
        Arc::new(Self {
            settings,
            plan,
            running: Mutex::new(HashMap::new()),
            over_quota: Mutex::new(HashSet::new()),
        })
    }

    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    pub fn plan(&self) -> &Plan {
        &self.plan
    }

    pub async fn active(&self) -> usize {
        self.running.lock().await.len()
    }

    fn user_dir(&self, key: &str) -> PathBuf {
        self.settings.data_dir.join("users").join(key)
    }

    fn disk_limit_bytes(&self) -> Option<u64> {
        self.settings.limits.disk_mb.map(|mb| mb * 1024 * 1024)
    }

    /// Storage used and the limit, in MB, for this person.
    pub fn storage(&self, sub: &str) -> (u64, Option<u64>) {
        let used = dir_usage(&self.user_dir(&user_key(sub)));
        (used / (1024 * 1024), self.settings.limits.disk_mb)
    }

    /// The running workspace for this person, starting one if needed.
    pub async fn endpoint(&self, sub: &str) -> Result<Endpoint, Failure> {
        let key = user_key(sub);
        if let Some(limit) = self.disk_limit_bytes() {
            let mut over = self.over_quota.lock().await;
            if over.contains(&key) {
                let used = dir_usage(&self.user_dir(&key));
                if used <= limit {
                    over.remove(&key);
                } else {
                    return Err(Failure::OverQuota {
                        used_mb: used / (1024 * 1024),
                        limit_mb: limit / (1024 * 1024),
                    });
                }
            }
        }
        let mut running = self.running.lock().await;
        if let Some(entry) = running.get_mut(&key) {
            match entry.child.try_wait() {
                Ok(None) => {
                    entry.last_used = Instant::now();
                    return Ok(entry.endpoint.clone());
                }
                _ => {
                    if let Some(mut dead) = running.remove(&key) {
                        dead.shutdown();
                        Cgroups::remove(&dead.joined);
                    }
                }
            }
        }
        if running.len() >= self.settings.max_active {
            return Err(Failure::Capacity);
        }
        let started = self
            .spawn(&key)
            .await
            .map_err(|e| Failure::Start(format!("{e:#}")))?;
        let endpoint = started.endpoint.clone();
        running.insert(key, started);
        Ok(endpoint)
    }

    /// Stop a person's workspace now (sign-out). Their files and journal stay.
    pub async fn stop(&self, sub: &str) -> bool {
        let key = user_key(sub);
        match self.running.lock().await.remove(&key) {
            Some(mut entry) => {
                entry.shutdown();
                let _ = entry.child.wait().await;
                Cgroups::remove(&entry.joined);
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
            entry.shutdown();
            let _ = entry.child.wait().await;
            Cgroups::remove(&entry.joined);
        }
        n
    }

    /// Stop workspaces nobody has used for the idle period, and ones that died.
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
                entry.shutdown();
                Cgroups::remove(&entry.joined);
            }
        }
        stale.len()
    }

    /// Stop any workspace whose directory has grown past the disk limit, and
    /// refuse to start it again until it is back under. Returns how many.
    pub async fn enforce_disk(&self) -> usize {
        let Some(limit) = self.disk_limit_bytes() else {
            return 0;
        };
        let keys: Vec<String> = self.running.lock().await.keys().cloned().collect();
        let mut stopped = 0;
        for key in keys {
            let dir = self.user_dir(&key);
            let used = tokio::task::spawn_blocking(move || dir_usage(&dir))
                .await
                .unwrap_or(0);
            if used > limit {
                if let Some(mut entry) = self.running.lock().await.remove(&key) {
                    entry.shutdown();
                    let _ = entry.child.wait().await;
                    Cgroups::remove(&entry.joined);
                }
                self.over_quota.lock().await.insert(key.clone());
                tracing::warn!(workspace = %key, used_mb = used / (1024 * 1024), limit_mb = limit / (1024 * 1024), "workspace is over its disk quota and was stopped");
                stopped += 1;
            }
        }
        stopped
    }

    /// Delete the person's worktree and scratch home and re-seed a fresh one.
    /// State (journal, approvals, budget) is kept.
    pub async fn reset(&self, sub: &str) -> Result<()> {
        self.stop(sub).await;
        let key = user_key(sub);
        let dir = self.user_dir(&key);
        for sub_dir in ["repo", "home"] {
            let path = dir.join(sub_dir);
            if std::fs::symlink_metadata(&path).is_ok() {
                if std::fs::symlink_metadata(&path)?.file_type().is_symlink() {
                    std::fs::remove_file(&path)?;
                } else {
                    std::fs::remove_dir_all(&path)?;
                }
            }
        }
        self.prepare(&dir, &key).await?;
        self.over_quota.lock().await.remove(&key);
        Ok(())
    }

    async fn spawn(&self, key: &str) -> Result<Running> {
        let user_dir = self.user_dir(key);
        let uid = self.prepare(&user_dir, key).await?;
        let sandboxed = self.plan.sandbox == Sandbox::Bwrap;
        // Hard limits: join the cgroup and set rlimits in the child before exec.
        let joined = match (&self.plan.cgroups, self.settings.quotas) {
            (Some(cg), mode) if mode != cgroup::Mode::Off => {
                match cg.create(key, &self.settings.limits) {
                    Ok(j) => j,
                    Err(e) if mode == cgroup::Mode::Require => {
                        return Err(e.context("cannot create the workspace's cgroup"))
                    }
                    Err(e) => {
                        tracing::warn!(workspace = %key, error = %e, "cgroup limits NOT applied");
                        Joined::default()
                    }
                }
            }
            _ => Joined::default(),
        };
        let limits = if self.settings.quotas == cgroup::Mode::Off {
            Limits::default()
        } else {
            self.settings.limits.clone()
        };
        let pre = PreExec::new(&joined, &limits, uid);
        // Egress: a per-workspace socket the gateway serves, mounted into the sandbox.
        let egress_sock = if self.plan.isolated_net {
            let run = self.settings.data_dir.join("run");
            std::fs::create_dir_all(&run)?;
            Some(run.join(format!("{key}.egress.sock")))
        } else {
            None
        };
        let egress_task = match &egress_sock {
            Some(sock) => Some(egress::spawn(
                sock,
                self.plan.egress.clone(),
                principal_for(key),
            )?),
            None => None,
        };
        let (program, args) = self.command_line(&user_dir, key, egress_sock.as_deref());
        let mut command = Command::new(&program);
        command
            .args(&args)
            .env_clear()
            .env("PATH", "/usr/local/bin:/usr/bin:/bin")
            .env(
                "HOME",
                if sandboxed {
                    "/work/home".to_string()
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
        // One step between fork and exec: join cgroups, set limits, drop to the
        // person's own Unix user, and arrange to die with the gateway.
        unsafe {
            command.pre_exec(move || pre.apply());
        }
        let mut child = match command.spawn() {
            Ok(c) => c,
            Err(e) => {
                if let Some(t) = egress_task {
                    t.abort();
                }
                Cgroups::remove(&joined);
                return Err(anyhow!(e).context(format!("cannot start {}", program.display())));
            }
        };
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
            Err(_) => Err(anyhow!("timed out waiting for the workspace to start")),
            Ok(Err(e)) => Err(anyhow!("workspace output unreadable: {e}")),
            Ok(Ok(Some(line))) => Ok(line),
            Ok(Ok(None)) => {
                tokio::time::sleep(Duration::from_millis(150)).await;
                let why = tail
                    .lock()
                    .map(|t| t.trim().to_string())
                    .unwrap_or_default();
                Err(anyhow!("workspace exited before it was ready: {why}"))
            }
        };
        let ready = first.and_then(|line| {
            parse_ready_line(&line)
                .ok_or_else(|| anyhow!("unexpected first line from workspace: {line:.80}"))
        });
        let (inner_socket, token) = match ready {
            Ok(v) => v,
            Err(e) => {
                let _ = child.start_kill();
                if let Some(t) = egress_task {
                    t.abort();
                }
                Cgroups::remove(&joined);
                return Err(e);
            }
        };
        // Keep the pipes drained so a chatty child can never block on a full pipe.
        tokio::spawn(async move { while let Ok(Some(_)) = lines.next_line().await {} });
        // Inside the sandbox the socket is under /work, which is this directory.
        let socket = match inner_socket.strip_prefix("/work/") {
            Some(rel) if sandboxed => user_dir.join(rel),
            _ => PathBuf::from(inner_socket),
        };
        Ok(Running {
            child,
            endpoint: Endpoint { socket, token },
            last_used: Instant::now(),
            joined,
            egress: egress_task,
            egress_socket: egress_sock,
        })
    }

    fn command_line(
        &self,
        user_dir: &Path,
        key: &str,
        egress_sock: Option<&Path>,
    ) -> (PathBuf, Vec<String>) {
        let in_sandbox = self.plan.sandbox == Sandbox::Bwrap;
        let (repo, state, socket) = if in_sandbox {
            (
                "/work/repo".to_string(),
                "/work/state".to_string(),
                "/work/ui.sock".to_string(),
            )
        } else {
            (
                user_dir.join("repo").display().to_string(),
                user_dir.join("state").display().to_string(),
                user_dir.join("ui.sock").display().to_string(),
            )
        };
        let mut jani: Vec<String> = vec![
            "ui".into(),
            "--unix-socket".into(),
            socket,
            "--repo".into(),
            repo,
            "--state-dir".into(),
            state,
            "--principal".into(),
            principal_for(key),
        ];
        if let Some(file) = &self.settings.settings_file {
            jani.extend([
                "--settings".to_string(),
                if in_sandbox {
                    SANDBOX_SETTINGS.to_string()
                } else {
                    file.display().to_string()
                },
            ]);
        }
        jani.extend(self.settings.jani_args.iter().cloned());
        if !in_sandbox {
            return (self.settings.jani_bin.clone(), jani);
        }
        // Binaries are bound read-only at fixed paths, wherever they live on the host.
        let host_bin = which(&self.settings.jani_bin.display().to_string())
            .unwrap_or_else(|| self.settings.jani_bin.clone());
        let mut args = bwrap_prefix(user_dir, egress_sock);
        let at = args.len() - 1; // before the trailing `--`
        let mut binds = vec![
            "--ro-bind".to_string(),
            host_bin.display().to_string(),
            SANDBOX_JANI.to_string(),
        ];
        if egress_sock.is_some() {
            binds.extend([
                "--ro-bind".to_string(),
                self.settings.self_bin.display().to_string(),
                SANDBOX_RELAY.to_string(),
            ]);
        }
        if let Some(file) = &self.settings.settings_file {
            binds.extend([
                "--ro-bind".to_string(),
                file.display().to_string(),
                SANDBOX_SETTINGS.to_string(),
            ]);
        }
        args.splice(at..at, binds);
        if egress_sock.is_some() {
            // The relay gives the empty network namespace its one way out.
            args.extend(
                [
                    SANDBOX_RELAY,
                    "sandbox-init",
                    "--socket",
                    SANDBOX_EGRESS,
                    "--listen",
                    SANDBOX_PROXY_LISTEN,
                    "--",
                ]
                .map(String::from),
            );
        }
        args.push(SANDBOX_JANI.to_string());
        args.extend(jani);
        (PathBuf::from("bwrap"), args)
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
            self_bin: PathBuf::from("/bin/sh"),
            jani_args: vec![],
            passthrough_env: vec![],
            template: None,
            settings_file: None,
            max_active: 2,
            idle_ttl: Duration::from_secs(60),
            sandbox: Sandbox::Off,
            uid_base: None,
            net: NetMode::Auto,
            egress_allow: vec![],
            limits: Limits::default(),
            quotas: cgroup::Mode::Auto,
            disk_check: Duration::from_secs(10),
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
        let ok = format!("Jani workspace: unix:/work/ui.sock/#token={token}");
        assert_eq!(
            parse_ready_line(&ok),
            Some(("/work/ui.sock".to_string(), token.clone()))
        );
        assert!(
            parse_ready_line("Jani workspace: http://127.0.0.1:4123/#token=abcd").is_none(),
            "TCP is not accepted"
        );
        assert!(parse_ready_line("unix:relative.sock/#token=").is_none());
        assert!(parse_ready_line(&format!("unix:/a.sock/#token={}zz", "a".repeat(40))).is_none());
        assert!(parse_ready_line("garbage").is_none());
    }

    #[test]
    fn bwrap_makes_only_the_users_directory_writable_and_isolates_the_network_on_request() {
        let shared = bwrap_prefix(Path::new("/data/users/abc"), None);
        let joined = shared.join(" ");
        assert!(joined.contains("--ro-bind /usr /usr"));
        assert!(joined.contains("--bind /data/users/abc /work"));
        assert!(joined.contains("--unshare-pid") && joined.contains("--die-with-parent"));
        assert_eq!(
            shared.iter().filter(|a| a.as_str() == "--bind").count(),
            1,
            "only one writable mount"
        );
        assert!(
            !joined.contains("--unshare-net"),
            "shared network unless asked"
        );
        assert_eq!(shared.last().map(String::as_str), Some("--"));
        let isolated = bwrap_prefix(
            Path::new("/data/users/abc"),
            Some(Path::new("/data/run/abc.egress.sock")),
        );
        let j = isolated.join(" ");
        assert!(j.contains("--unshare-net"));
        assert!(j.contains("--ro-bind /data/run/abc.egress.sock /run/egress.sock"));
        assert_eq!(
            isolated.iter().filter(|a| a.as_str() == "--bind").count(),
            1,
            "the socket is read-only"
        );
    }

    #[test]
    fn no_isolation_is_refused_unless_explicitly_allowed() {
        let dir = std::env::temp_dir();
        let s = settings(&dir);
        std::env::remove_var("JANI_CLOUD_ALLOW_UNISOLATED");
        assert!(s
            .plan(true)
            .unwrap_err()
            .to_string()
            .contains("no isolation"));
        let mut with_uid = settings(&dir);
        with_uid.uid_base = Some(40000);
        assert!(with_uid
            .plan(false)
            .unwrap_err()
            .to_string()
            .contains("as root"));
        assert!(with_uid.plan(true).is_ok());
        let mut bw = settings(&dir);
        bw.sandbox = Sandbox::Bwrap;
        if which("bwrap").is_none() {
            assert!(bw
                .plan(true)
                .unwrap_err()
                .to_string()
                .contains("not on PATH"));
        }
    }

    #[test]
    fn isolated_network_needs_bubblewrap_and_an_allowed_destination() {
        let dir = std::env::temp_dir();
        let mut s = settings(&dir);
        s.uid_base = Some(40000);
        s.net = NetMode::Isolated;
        assert!(s
            .plan(true)
            .unwrap_err()
            .to_string()
            .contains("needs bubblewrap"));
        if which("bwrap").is_some() {
            let mut b = settings(&dir);
            b.sandbox = Sandbox::Bwrap;
            b.net = NetMode::Isolated;
            assert!(b
                .plan(true)
                .unwrap_err()
                .to_string()
                .contains("no destination is allowed"));
            b.jani_args = vec!["--api-base".into(), "https://api.model.test/v1".into()];
            let plan = b.plan(true).unwrap();
            assert!(plan.isolated_net);
            assert!(plan.egress.rule_for("api.model.test", 443).is_some());
            assert!(plan.egress.rule_for("evil.test", 443).is_none());
        }
        let mut shared = settings(&dir);
        shared.uid_base = Some(40000);
        let plan = shared.plan(true).unwrap();
        assert!(!plan.isolated_net);
        assert!(
            plan.notes.iter().any(|n| n.contains("SHARED")),
            "shared network is called out"
        );
    }

    #[tokio::test]
    async fn uid_allocation_is_stable_per_person() {
        let dir = std::env::temp_dir().join(format!("jani-cloud-t-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let s = settings(&dir);
        let plan = s.plan(true);
        let _ = plan; // not needed here
        let m = Manager::new(
            s,
            Plan {
                sandbox: Sandbox::Off,
                isolated_net: false,
                egress: Arc::new(Allow::default()),
                cgroups: None,
                notes: vec![],
            },
        );
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

    #[test]
    fn disk_usage_counts_blocks_and_ignores_symlinks() {
        let dir = std::env::temp_dir().join(format!("jani-cloud-du-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("a")).unwrap();
        std::fs::write(dir.join("a/f"), vec![1u8; 100_000]).unwrap();
        std::os::unix::fs::symlink("/usr", dir.join("link")).unwrap();
        let used = dir_usage(&dir);
        assert!((100_000..10_000_000).contains(&used), "{used}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
