use crate::config::{validate_relative, Config, JResult};
use crate::process::ProcessIsolation;
use crate::types::{new_id, NetworkPolicy, SandboxMode, SandboxProfile, SandboxReceipt};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Backend {
    None,
    #[cfg_attr(windows, allow(dead_code))]
    Bubblewrap(PathBuf),
    #[cfg(windows)]
    WindowsJob,
}

impl Backend {
    pub fn name(&self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Bubblewrap(_) => "bubblewrap",
            #[cfg(windows)]
            Self::WindowsJob => "windows-job",
        }
    }
}

#[derive(Clone, Debug)]
pub struct SandboxPlan {
    pub backend: Backend,
    pub backend_version: String,
    pub profile: SandboxProfile,
    pub network: NetworkPolicy,
    pub enforced: bool,
    pub filesystem_confined: bool,
    pub network_confined: bool,
    pub process_tree: bool,
    pub attested: bool,
}

impl SandboxPlan {
    pub fn receipt(&self, termination_reason: Option<String>) -> SandboxReceipt {
        SandboxReceipt {
            sandbox_id: new_id("sbx"),
            backend: self.backend.name().into(),
            backend_version: self.backend_version.clone(),
            profile: self.profile,
            enforced: self.enforced,
            filesystem_confined: self.filesystem_confined,
            network_confined: self.network_confined,
            network: self.network,
            process_tree: self.process_tree,
            attested: self.attested,
            termination_reason,
        }
    }
}

pub struct SandboxManager<'a> {
    pub cfg: &'a Config,
}

impl<'a> SandboxManager<'a> {
    pub fn new(cfg: &'a Config) -> Self {
        Self { cfg }
    }

    /// Select a backend for the requested profile. A profile that requires
    /// confinement never silently falls back to unconfined execution.
    pub fn prepare(
        &self,
        profile: SandboxProfile,
        network: NetworkPolicy,
    ) -> Result<SandboxPlan, String> {
        let isolation_required = profile.requires_isolation();
        let mode = effective_mode(self.cfg);
        match mode {
            SandboxMode::Off => {
                if isolation_required {
                    return Err(format!(
                        "sandbox profile '{}' requires an OS backend, but sandboxing is off",
                        profile.as_str()
                    ));
                }
                Ok(SandboxPlan {
                    backend: Backend::None,
                    backend_version: "none".into(),
                    profile,
                    network,
                    enforced: false,
                    filesystem_confined: false,
                    network_confined: false,
                    process_tree: false,
                    attested: false,
                })
            }
            SandboxMode::Bwrap => {
                #[cfg(unix)]
                {
                    let path = find_bwrap()
                        .ok_or("bubblewrap (bwrap) was requested but not found on PATH")?;
                    let version = bwrap_version(&path)?;
                    Ok(bwrap_plan(path, version, profile, network))
                }
                #[cfg(not(unix))]
                {
                    let _ = profile;
                    let _ = network;
                    Err("bubblewrap is only available on Unix hosts".into())
                }
            }
            SandboxMode::WindowsJob => {
                #[cfg(windows)]
                {
                    if isolation_required {
                        return Err(format!(
                            "profile '{}' requires filesystem confinement; a Windows Job Object only bounds the process tree",
                            profile.as_str()
                        ));
                    }
                    Ok(SandboxPlan {
                        backend: Backend::WindowsJob,
                        backend_version: "job-object".into(),
                        profile,
                        network,
                        enforced: true,
                        filesystem_confined: false,
                        network_confined: false,
                        process_tree: true,
                        attested: true,
                    })
                }
                #[cfg(not(windows))]
                {
                    let _ = profile;
                    let _ = network;
                    Err("windows-job is only available on Windows hosts".into())
                }
            }
            SandboxMode::Auto => {
                #[cfg(unix)]
                {
                    if let Some(path) = find_bwrap() {
                        let version = bwrap_version(&path)?;
                        return Ok(bwrap_plan(path, version, profile, network));
                    }
                    if isolation_required {
                        return Err(
                            "no supported OS sandbox backend found; refusing unconfined execution"
                                .into(),
                        );
                    }
                    unconfined(profile, network)
                }
                #[cfg(windows)]
                {
                    if isolation_required {
                        return Err(
                            "no supported OS sandbox backend can satisfy this profile on Windows; refusing unconfined execution"
                                .into(),
                        );
                    }
                    Ok(SandboxPlan {
                        backend: Backend::WindowsJob,
                        backend_version: "job-object".into(),
                        profile,
                        network,
                        enforced: true,
                        filesystem_confined: false,
                        network_confined: false,
                        process_tree: true,
                        attested: true,
                    })
                }
                #[cfg(not(any(unix, windows)))]
                {
                    if isolation_required {
                        return Err("no supported OS sandbox backend found".into());
                    }
                    unconfined(profile, network)
                }
            }
        }
    }

    pub(crate) fn prepare_sidecar(&self, profile: SandboxProfile) -> JResult<SandboxPlan> {
        #[cfg(target_os = "linux")]
        if matches!(
            effective_mode(self.cfg),
            SandboxMode::Auto | SandboxMode::Bwrap
        ) {
            let root = self
                .cfg
                .root
                .canonicalize()
                .map_err(|error| format!("invalid worktree root: {error}"))?;
            if SIDECAR_RUNTIME_DIRS
                .iter()
                .chain(SYSTEM_LINKS.iter())
                .filter_map(|path| Path::new(path).canonicalize().ok())
                .any(|system| root.starts_with(&system) || system.starts_with(&root))
            {
                return Err("sidecar worktree overlaps a system runtime mount".into());
            }
            let path = find_trusted_bwrap(&root).ok_or("sidecar requires Bubblewrap in a trusted absolute PATH directory outside the worktree")?;
            let mut probe = Command::new(&path);
            probe.env_clear().arg("--version");
            let output = crate::process::execute(&mut probe, Path::new("/"), 5)?;
            if output.timed_out
                || output.code != Some(0)
                || !output.stdout.starts_with("bubblewrap ")
            {
                return Err("sidecar Bubblewrap version probe failed".into());
            }
            return Ok(bwrap_plan(
                path,
                output.stdout.trim().to_owned(),
                profile,
                NetworkPolicy::Deny,
            ));
        }
        self.prepare(profile, NetworkPolicy::Deny)
    }

    /// Build the command that executes `argv` under the selected plan. Write
    /// scopes are created if absent because the operator already approved them.
    /// Build the command that executes `argv` under the selected plan, with extra
    /// worktree-relative directories the command may write (build output such as `target`). Only a confining backend uses them;
    /// a path that is absolute, climbs out, or is a symlink is ignored.
    pub fn command_with(
        &self,
        plan: &SandboxPlan,
        argv: &[String],
        extra_writable: &[PathBuf],
    ) -> JResult<(Command, ProcessIsolation)> {
        self.build_command(plan, argv, extra_writable, true)
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn command_for_sidecar(
        &self,
        plan: &SandboxPlan,
        argv: &[String],
    ) -> JResult<(Command, ProcessIsolation)> {
        self.build_command(plan, argv, &[], false)
    }

    fn build_command(
        &self,
        plan: &SandboxPlan,
        argv: &[String],
        extra_writable: &[PathBuf],
        developer_mounts: bool,
    ) -> JResult<(Command, ProcessIsolation)> {
        let (program, args) = argv.split_first().ok_or("empty executable")?;
        match &plan.backend {
            Backend::None => {
                let mut cmd = Command::new(program);
                cmd.args(args);
                Ok((cmd, ProcessIsolation::Direct))
            }
            #[cfg(windows)]
            Backend::WindowsJob => {
                let mut cmd = Command::new(program);
                cmd.args(args);
                Ok((cmd, ProcessIsolation::JobObject))
            }
            Backend::Bubblewrap(path) => {
                let canon_root = self
                    .cfg
                    .root
                    .canonicalize()
                    .map_err(|e| format!("invalid worktree root: {e}"))?;
                let mut verified_write_paths = Vec::new();
                for scope in &self.cfg.write_paths {
                    validate_relative(scope)?;
                    let full = self.cfg.root.join(scope);
                    if let Ok(meta) = std::fs::symlink_metadata(&full) {
                        if meta.file_type().is_symlink() {
                            return Err(format!(
                                "symlinked write scope refused: {}",
                                full.display()
                            ));
                        }
                    } else if scope.extension().is_none() {
                        std::fs::create_dir_all(&full).map_err(|e| {
                            format!("cannot prepare write scope {}: {e}", full.display())
                        })?;
                    } else {
                        if let Some(parent) = full.parent() {
                            std::fs::create_dir_all(parent).map_err(|e| {
                                format!("cannot prepare write parent {}: {e}", parent.display())
                            })?;
                        }
                    }
                    let check_target = if full.exists() {
                        full.clone()
                    } else {
                        full.parent().unwrap_or(&self.cfg.root).to_path_buf()
                    };
                    let real = check_target.canonicalize().map_err(|e| {
                        format!("cannot resolve write scope {}: {e}", full.display())
                    })?;
                    if !real.starts_with(&canon_root) {
                        return Err(format!(
                            "write scope resolves outside worktree: {}",
                            full.display()
                        ));
                    }
                    if full.exists() {
                        verified_write_paths.push(full);
                    }
                }
                let mut cmd = Command::new(path);
                let root = self
                    .cfg
                    .root
                    .to_str()
                    .ok_or("worktree path is not valid UTF-8")?;
                cmd.arg("--die-with-parent")
                    .arg("--new-session")
                    .arg("--unshare-pid")
                    .arg("--unshare-ipc")
                    .arg("--unshare-uts")
                    .arg("--unshare-cgroup-try")
                    .arg("--clearenv")
                    .arg("--setenv")
                    .arg("PATH")
                    .arg("/usr/local/bin:/usr/bin:/bin")
                    .arg("--setenv")
                    .arg("HOME")
                    .arg("/tmp")
                    .arg("--setenv")
                    .arg("TMPDIR")
                    .arg("/tmp")
                    .arg("--tmpfs")
                    .arg("/tmp")
                    .arg("--proc")
                    .arg("/proc")
                    .arg("--dev")
                    .arg("/dev");
                if plan.network == NetworkPolicy::Deny || plan.network == NetworkPolicy::Loopback {
                    // bwrap cannot expose loopback alone; treating loopback as deny is stricter.
                    cmd.arg("--unshare-net");
                }
                let system_dirs: &[&str] = if developer_mounts {
                    &["/usr", "/etc"]
                } else {
                    SIDECAR_RUNTIME_DIRS
                };
                for dir in system_dirs {
                    if Path::new(dir).is_dir() {
                        cmd.arg("--ro-bind-try").arg(dir).arg(dir);
                    }
                }
                for &link in SYSTEM_LINKS {
                    let path = Path::new(link);
                    if let Ok(meta) = std::fs::symlink_metadata(path) {
                        if meta.file_type().is_symlink() {
                            if let Ok(target) = std::fs::read_link(path) {
                                cmd.arg("--symlink").arg(target.as_os_str()).arg(link);
                            }
                        } else if meta.is_dir() {
                            cmd.arg("--ro-bind-try").arg(link).arg(link);
                        }
                    }
                }
                if developer_mounts {
                    for cache in toolchain_caches() {
                        if cache.is_dir() {
                            cmd.arg("--ro-bind-try").arg(&cache).arg(&cache);
                        }
                    }
                }
                cmd.arg("--ro-bind").arg(root).arg(root);
                // Mask sensitive files and directories inside the worktree before exposing writable scopes.
                for name in [
                    ".env",
                    ".git-credentials",
                    ".npmrc",
                    "credentials",
                    "credentials.toml",
                ] {
                    let candidate = self.cfg.root.join(name);
                    if candidate.is_file() && Path::new("/dev/null").exists() {
                        cmd.arg("--ro-bind").arg("/dev/null").arg(&candidate);
                    }
                }
                let jani_dir = self.cfg.root.join(".jani");
                if jani_dir.is_dir() {
                    cmd.arg("--tmpfs").arg(&jani_dir);
                }
                for full in verified_write_paths {
                    cmd.arg("--bind").arg(&full).arg(&full);
                }
                for dir in extra_writable {
                    if validate_relative(dir).is_err() {
                        continue;
                    }
                    let full = self.cfg.root.join(dir);
                    let linked = std::fs::symlink_metadata(&full)
                        .map(|m| m.file_type().is_symlink())
                        .unwrap_or(false);
                    if linked || std::fs::create_dir_all(&full).is_err() {
                        continue;
                    }
                    // Creating it must not have followed a symlinked parent out of the worktree.
                    let inside = match (full.canonicalize(), self.cfg.root.canonicalize()) {
                        (Ok(real), Ok(root)) => real.starts_with(root),
                        _ => false,
                    };
                    if inside {
                        cmd.arg("--bind").arg(&full).arg(&full);
                    }
                }
                cmd.arg("--chdir").arg(root).arg("--");
                cmd.arg(program).args(args);
                Ok((cmd, ProcessIsolation::SandboxNamespace))
            }
        }
    }
}

const SIDECAR_RUNTIME_DIRS: &[&str] = &["/usr/bin", "/usr/sbin", "/usr/lib", "/usr/lib64"];
const SYSTEM_LINKS: &[&str] = &["/bin", "/sbin", "/lib", "/lib64"];

#[cfg(unix)]
fn bwrap_plan(
    path: PathBuf,
    version: String,
    profile: SandboxProfile,
    network: NetworkPolicy,
) -> SandboxPlan {
    SandboxPlan {
        backend: Backend::Bubblewrap(path),
        backend_version: version,
        profile,
        network,
        enforced: true,
        filesystem_confined: true,
        network_confined: network_is_confined(network),
        process_tree: true,
        attested: true,
    }
}

#[cfg(not(windows))]
fn unconfined(profile: SandboxProfile, network: NetworkPolicy) -> Result<SandboxPlan, String> {
    Ok(SandboxPlan {
        backend: Backend::None,
        backend_version: "none".into(),
        profile,
        network,
        enforced: false,
        filesystem_confined: false,
        network_confined: false,
        process_tree: false,
        attested: false,
    })
}

/// The bwrap plan isolates the network namespace except when host networking is
/// explicitly requested; receipts must report the same fact.
#[cfg(unix)]
fn network_is_confined(network: NetworkPolicy) -> bool {
    network != NetworkPolicy::Host
}

fn effective_mode(cfg: &Config) -> SandboxMode {
    if cfg.is_enterprise() && cfg.sandbox_mode == SandboxMode::Off && !cfg.sandbox_explicit {
        SandboxMode::Auto
    } else {
        cfg.sandbox_mode
    }
}

#[cfg(unix)]
fn find_bwrap() -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        for name in ["bwrap", "bwrap.exe"] {
            let candidate = dir.join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

#[cfg(target_os = "linux")]
fn find_trusted_bwrap(worktree: &Path) -> Option<PathBuf> {
    use std::os::unix::fs::MetadataExt;

    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        if !dir.is_absolute() {
            continue;
        }
        let Ok(candidate) = dir.join("bwrap").canonicalize() else {
            continue;
        };
        if candidate.starts_with(worktree) {
            continue;
        }
        let Ok(file) = candidate.metadata() else {
            continue;
        };
        let owner = unsafe { libc::geteuid() };
        if file.is_file()
            && file.mode() & 0o111 != 0
            && candidate.ancestors().all(|path| {
                path.metadata().is_ok_and(|metadata| {
                    (metadata.uid() == 0 || metadata.uid() == owner) && metadata.mode() & 0o022 == 0
                })
            })
        {
            return Some(candidate);
        }
    }
    None
}

#[cfg(unix)]
fn bwrap_version(path: &Path) -> Result<String, String> {
    let output = Command::new(path)
        .arg("--version")
        .output()
        .map_err(|e| format!("cannot execute {}: {e}", path.display()))?;
    if !output.status.success() {
        return Err("bubblewrap --version failed".into());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn toolchain_caches() -> Vec<PathBuf> {
    let mut found = Vec::new();
    let push_cargo_subdirs = |base: &Path, out: &mut Vec<PathBuf>| {
        for sub in ["registry", "git", "bin"] {
            let p = base.join(sub);
            if p.is_dir() {
                out.push(p);
            }
        }
    };
    if let Some(cargo) = std::env::var_os("CARGO_HOME") {
        push_cargo_subdirs(Path::new(&cargo), &mut found);
    }
    for key in ["RUSTUP_HOME", "GOPATH"] {
        if let Some(value) = std::env::var_os(key) {
            found.push(PathBuf::from(value));
        }
    }
    if let Some(home) = crate::config::home_dir() {
        push_cargo_subdirs(&home.join(".cargo"), &mut found);
        for name in [".rustup", ".cache/go-build"] {
            found.push(home.join(name));
        }
    }
    found
}

pub fn doctor() -> serde_json::Value {
    let mut backends = Vec::new();
    backends.push(json!({"name": "none", "available": true, "confinesFilesystem": false, "confinesNetwork": false}));
    #[cfg(unix)]
    {
        let bwrap = find_bwrap();
        let version = bwrap.as_ref().and_then(|p| bwrap_version(p).ok());
        backends.push(json!({
            "name": "bubblewrap",
            "available": bwrap.is_some(),
            "version": version,
            "confinesFilesystem": bwrap.is_some(),
            "confinesNetwork": bwrap.is_some(),
        }));
    }
    #[cfg(windows)]
    {
        backends.push(json!({
            "name": "windows-job",
            "available": true,
            "version": "job-object",
            "confinesFilesystem": false,
            "confinesNetwork": false,
            "confinesProcessTree": true,
        }));
    }
    json!({
        "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "backends": backends,
    })
}

pub fn check(cfg: &Config) -> serde_json::Value {
    let manager = SandboxManager::new(cfg);
    let mut results = Vec::new();
    for profile in [
        SandboxProfile::TrustedDev,
        SandboxProfile::DevelopmentConfined,
        SandboxProfile::EnterpriseRestricted,
        SandboxProfile::EnterpriseConnected,
    ] {
        let outcome = manager.prepare(profile, NetworkPolicy::Deny);
        results.push(match outcome {
            Ok(plan) => json!({
                "profile": profile.as_str(),
                "ok": true,
                "backend": plan.backend.name(),
                "filesystemConfined": plan.filesystem_confined,
                "networkConfined": plan.network_confined,
                "processTree": plan.process_tree,
            }),
            Err(error) => json!({
                "profile": profile.as_str(),
                "ok": false,
                "error": error,
            }),
        });
    }
    json!({"mode": cfg.sandbox_mode.as_str(), "profiles": results})
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg_with(mode: SandboxMode) -> Config {
        let mut cfg = Config::baseline();
        cfg.sandbox_mode = mode;
        cfg.sandbox_explicit = true;
        cfg
    }

    #[test]
    fn isolation_profiles_never_fall_back_to_unconfined() {
        let cfg = cfg_with(SandboxMode::Off);
        let manager = SandboxManager::new(&cfg);
        assert!(manager
            .prepare(SandboxProfile::EnterpriseRestricted, NetworkPolicy::Deny)
            .is_err());
        assert!(manager
            .prepare(SandboxProfile::TrustedDev, NetworkPolicy::Deny)
            .is_ok());
    }

    #[test]
    fn auto_mode_refuses_enterprise_profile_without_backend_or_confines() {
        let cfg = cfg_with(SandboxMode::Auto);
        let manager = SandboxManager::new(&cfg);
        match manager.prepare(SandboxProfile::EnterpriseRestricted, NetworkPolicy::Deny) {
            Ok(plan) => {
                assert!(plan.filesystem_confined);
                assert!(plan.network_confined);
            }
            Err(error) => assert!(error.contains("sandbox") || error.contains("backend")),
        }
    }

    #[cfg(unix)]
    #[test]
    fn network_confinement_matches_backend_behavior() {
        assert!(network_is_confined(NetworkPolicy::Deny));
        assert!(network_is_confined(NetworkPolicy::Loopback));
        assert!(!network_is_confined(NetworkPolicy::Host));
    }

    #[test]
    fn receipts_report_network_confinement_consistently() {
        let plan = SandboxPlan {
            backend: Backend::None,
            backend_version: "none".into(),
            profile: SandboxProfile::TrustedDev,
            network: NetworkPolicy::Host,
            enforced: false,
            filesystem_confined: false,
            network_confined: false,
            process_tree: false,
            attested: false,
        };
        assert!(!plan.receipt(None).network_confined);
    }

    #[test]
    fn windows_job_does_not_claim_filesystem_confinement() {
        let cfg = cfg_with(SandboxMode::WindowsJob);
        let manager = SandboxManager::new(&cfg);
        if cfg!(windows) {
            assert!(manager
                .prepare(SandboxProfile::EnterpriseRestricted, NetworkPolicy::Deny)
                .is_err());
        } else {
            assert!(manager
                .prepare(SandboxProfile::TrustedDev, NetworkPolicy::Deny)
                .is_err());
        }
    }
}
