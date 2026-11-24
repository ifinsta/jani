//! Per-workspace resource limits.
//!
//! Hard limits come from cgroups where the host lets us create them (memory,
//! processes, CPU), and from rlimits everywhere (open files, largest file, core
//! dumps, and processes per Unix user). The workspace joins its cgroup and has
//! its rlimits set in the child between fork and exec, before it drops
//! privileges, so there is no window in which it runs unlimited.
//!
//! cgroup v1 (separate controller hierarchies) is implemented and tested.
//! cgroup v2 is implemented from the kernel documentation and is NOT tested.

use anyhow::{bail, Result};
use std::ffi::CString;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Limits {
    pub memory_mb: Option<u64>,
    pub pids: Option<u64>,
    /// CPUs' worth of time, e.g. 1.5.
    pub cpus: Option<f64>,
    pub open_files: Option<u64>,
    /// Soft disk quota (watched by the gateway) and the largest single file.
    pub disk_mb: Option<u64>,
}

impl Limits {
    pub fn any(&self) -> bool {
        self.memory_mb.is_some()
            || self.pids.is_some()
            || self.cpus.is_some()
            || self.disk_mb.is_some()
            || self.open_files.is_some()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Refuse to start unless every configured hard limit can be enforced.
    Require,
    /// Enforce what the host allows and log what it does not.
    Auto,
    /// No limits at all.
    Off,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Flavor {
    V1,
    V2,
}

#[derive(Clone, Debug)]
pub struct Cgroups {
    flavor: Flavor,
    root: PathBuf,
}

/// What a single workspace was given: files the child writes its pid to, and
/// directories to remove when it stops.
#[derive(Clone, Debug, Default)]
pub struct Joined {
    pub procs_files: Vec<PathBuf>,
    pub dirs: Vec<PathBuf>,
}

fn write(path: &Path, value: &str) -> Result<()> {
    std::fs::write(path, value)
        .map_err(|e| anyhow::anyhow!("cannot write {} to {}: {e}", value, path.display()))
}

impl Cgroups {
    /// Find a writable cgroup hierarchy, or None.
    pub fn detect() -> Option<Self> {
        let root = PathBuf::from("/sys/fs/cgroup");
        make_writable(&root);
        if root.join("cgroup.controllers").is_file() {
            return Some(Self {
                flavor: Flavor::V2,
                root,
            });
        }
        if root.join("memory").is_dir() || root.join("pids").is_dir() {
            return Some(Self {
                flavor: Flavor::V1,
                root,
            });
        }
        None
    }

    pub fn describe(&self) -> &'static str {
        match self.flavor {
            Flavor::V1 => "cgroup v1",
            Flavor::V2 => "cgroup v2",
        }
    }

    /// Which of the configured limits this host can enforce with cgroups.
    pub fn can_enforce(&self, limits: &Limits) -> (Vec<&'static str>, Vec<&'static str>) {
        let have = |ctl: &str| match self.flavor {
            Flavor::V1 => self.root.join(ctl).is_dir() && is_writable(&self.root.join(ctl)),
            Flavor::V2 => is_writable(&self.root),
        };
        let (mut ok, mut missing) = (Vec::new(), Vec::new());
        for (name, wanted, ctl) in [
            ("memory", limits.memory_mb.is_some(), "memory"),
            ("pids", limits.pids.is_some(), "pids"),
            ("cpu", limits.cpus.is_some(), "cpu"),
        ] {
            if wanted {
                if have(ctl) {
                    ok.push(name)
                } else {
                    missing.push(name)
                }
            }
        }
        (ok, missing)
    }

    /// Create a cgroup for one workspace and apply its limits.
    pub fn create(&self, key: &str, limits: &Limits) -> Result<Joined> {
        let mut joined = Joined::default();
        match self.flavor {
            Flavor::V1 => {
                if let Some(mb) = limits.memory_mb {
                    let dir = self.v1_dir("memory", key)?;
                    write(
                        &dir.join("memory.limit_in_bytes"),
                        &(mb * 1024 * 1024).to_string(),
                    )?;
                    // No swap: memory+swap equals memory, where the kernel has swap accounting.
                    let _ = write(
                        &dir.join("memory.memsw.limit_in_bytes"),
                        &(mb * 1024 * 1024).to_string(),
                    );
                    joined.procs_files.push(dir.join("cgroup.procs"));
                    joined.dirs.push(dir);
                }
                if let Some(n) = limits.pids {
                    let dir = self.v1_dir("pids", key)?;
                    write(&dir.join("pids.max"), &n.to_string())?;
                    joined.procs_files.push(dir.join("cgroup.procs"));
                    joined.dirs.push(dir);
                }
                if let Some(cpus) = limits.cpus {
                    let dir = self.v1_dir("cpu", key)?;
                    write(&dir.join("cpu.cfs_period_us"), "100000")?;
                    write(
                        &dir.join("cpu.cfs_quota_us"),
                        &((cpus * 100_000.0) as u64).max(1000).to_string(),
                    )?;
                    joined.procs_files.push(dir.join("cgroup.procs"));
                    joined.dirs.push(dir);
                }
            }
            Flavor::V2 => {
                let base = self.root.join("jani-cloud");
                std::fs::create_dir_all(&base)?;
                // Controllers must be enabled for children by the parent.
                let _ = write(
                    &self.root.join("cgroup.subtree_control"),
                    "+memory +pids +cpu",
                );
                let _ = write(&base.join("cgroup.subtree_control"), "+memory +pids +cpu");
                let dir = base.join(key);
                std::fs::create_dir_all(&dir)?;
                if let Some(mb) = limits.memory_mb {
                    write(&dir.join("memory.max"), &(mb * 1024 * 1024).to_string())?;
                    let _ = write(&dir.join("memory.swap.max"), "0");
                }
                if let Some(n) = limits.pids {
                    write(&dir.join("pids.max"), &n.to_string())?;
                }
                if let Some(cpus) = limits.cpus {
                    write(
                        &dir.join("cpu.max"),
                        &format!("{} 100000", ((cpus * 100_000.0) as u64).max(1000)),
                    )?;
                }
                joined.procs_files.push(dir.join("cgroup.procs"));
                joined.dirs.push(dir);
            }
        }
        Ok(joined)
    }

    fn v1_dir(&self, controller: &str, key: &str) -> Result<PathBuf> {
        let dir = self.root.join(controller).join("jani-cloud").join(key);
        std::fs::create_dir_all(&dir)?;
        Ok(dir)
    }

    /// Remove cgroups left behind by a gateway that was killed. `rmdir` only
    /// succeeds on an empty cgroup, so a live workspace is never touched.
    pub fn cleanup_stale(&self) -> usize {
        let parents: Vec<PathBuf> = match self.flavor {
            Flavor::V1 => ["memory", "pids", "cpu"]
                .iter()
                .map(|c| self.root.join(c).join("jani-cloud"))
                .collect(),
            Flavor::V2 => vec![self.root.join("jani-cloud")],
        };
        let mut removed = 0;
        for parent in parents {
            let Ok(entries) = std::fs::read_dir(&parent) else {
                continue;
            };
            for entry in entries.flatten() {
                // Page cache charged to an idle memory cgroup would keep it from being removed.
                let _ = std::fs::write(entry.path().join("memory.force_empty"), "0");
                if entry.path().is_dir() && std::fs::remove_dir(entry.path()).is_ok() {
                    removed += 1;
                }
            }
        }
        removed
    }

    /// Remove a stopped workspace's cgroup directories.
    pub fn remove(joined: &Joined) {
        for dir in &joined.dirs {
            let _ = std::fs::remove_dir(dir);
        }
    }
}

/// Containers mount the cgroup hierarchies read-only even when they hold
/// CAP_SYS_ADMIN. Root with that capability may flip its own view of them to
/// read-write (the same step Docker-in-Docker takes); this changes the mount in
/// this container's namespace only. Anything that fails is left as it was and
/// reported later as "no writable cgroup".
fn make_writable(root: &Path) {
    // SAFETY: geteuid has no preconditions.
    if unsafe { libc::geteuid() } != 0 {
        return;
    }
    let mut targets = vec![root.to_path_buf()];
    if let Ok(entries) = std::fs::read_dir(root) {
        targets.extend(entries.flatten().map(|e| e.path()).filter(|p| p.is_dir()));
    }
    for dir in targets {
        if is_writable(&dir) {
            continue;
        }
        let Ok(path) = CString::new(dir.as_os_str().as_encoded_bytes()) else {
            continue;
        };
        // SAFETY: `path` is a valid NUL-terminated string; the other pointers are null as the call allows.
        let rc = unsafe {
            libc::mount(
                std::ptr::null(),
                path.as_ptr(),
                std::ptr::null(),
                libc::MS_REMOUNT | libc::MS_BIND,
                std::ptr::null(),
            )
        };
        if rc == 0 && is_writable(&dir) {
            tracing::info!(cgroup = %dir.display(), "remounted a read-only cgroup hierarchy read-write (container)");
        }
    }
}

fn is_writable(dir: &Path) -> bool {
    let probe = dir.join(format!("jani-cloud-probe-{}", std::process::id()));
    match std::fs::create_dir(&probe) {
        Ok(()) => {
            let _ = std::fs::remove_dir(&probe);
            true
        }
        Err(_) => false,
    }
}

/// Validate the configured limits against the host, per the mode.
pub fn plan(
    mode: Mode,
    limits: &Limits,
    cg: Option<&Cgroups>,
    has_uid_isolation: bool,
) -> Result<Vec<String>> {
    let mut notes = Vec::new();
    if mode == Mode::Off || !limits.any() {
        return Ok(notes);
    }
    let wants_cgroup = limits.memory_mb.is_some() || limits.pids.is_some() || limits.cpus.is_some();
    let mut missing: Vec<&str> = Vec::new();
    match cg {
        Some(cg) => {
            let (ok, miss) = cg.can_enforce(limits);
            if !ok.is_empty() {
                notes.push(format!(
                    "{} limits enforced via {}",
                    ok.join(", "),
                    cg.describe()
                ));
            }
            missing.extend(miss);
        }
        None if wants_cgroup => {
            if limits.memory_mb.is_some() {
                missing.push("memory");
            }
            if limits.pids.is_some() {
                missing.push("pids");
            }
            if limits.cpus.is_some() {
                missing.push("cpu");
            }
        }
        None => {}
    }
    if !missing.is_empty() {
        let msg = format!("cannot enforce {} (no writable cgroup)", missing.join(", "));
        if mode == Mode::Require {
            bail!("{msg}. Run with a delegated cgroup, or set JANI_CLOUD_QUOTAS=auto to continue without them.");
        }
        notes.push(format!("{msg}: NOT ENFORCED"));
    }
    if limits.pids.is_some() && !has_uid_isolation {
        notes.push("RLIMIT_NPROC is per Unix user, so without JANI_CLOUD_UID_BASE only the cgroup pid limit applies".into());
    }
    if limits.disk_mb.is_some() {
        notes.push(
            "disk limit is a soft quota: checked every few seconds, plus a cap on any single file"
                .into(),
        );
    }
    Ok(notes)
}

/// Everything the child needs between fork and exec, prepared in advance so the
/// closure allocates nothing on the success path.
#[derive(Clone)]
pub struct PreExec {
    pub procs: Vec<CString>,
    pub rlimits: Vec<(i32, u64)>,
    pub uid: Option<u32>,
}

impl PreExec {
    pub fn new(joined: &Joined, limits: &Limits, uid: Option<u32>) -> Self {
        let mut rlimits: Vec<(i32, u64)> = vec![(libc::RLIMIT_CORE as i32, 0)];
        if let Some(n) = limits.open_files {
            rlimits.push((libc::RLIMIT_NOFILE as i32, n));
        }
        if let Some(mb) = limits.disk_mb {
            rlimits.push((libc::RLIMIT_FSIZE as i32, mb * 1024 * 1024));
        }
        if let Some(n) = limits.pids {
            rlimits.push((libc::RLIMIT_NPROC as i32, n));
        }
        let procs = joined
            .procs_files
            .iter()
            .filter_map(|p| CString::new(p.as_os_str().as_encoded_bytes()).ok())
            .collect();
        Self {
            procs,
            rlimits,
            uid,
        }
    }

    /// Runs in the forked child. Only async-signal-safe libc calls.
    ///
    /// # Safety
    /// Must be called only between fork and exec.
    pub unsafe fn apply(&self) -> std::io::Result<()> {
        let pid = libc::getpid();
        let mut buf = [0u8; 12];
        let mut n = pid as u32;
        let mut i = buf.len();
        loop {
            i -= 1;
            buf[i] = b'0' + (n % 10) as u8;
            n /= 10;
            if n == 0 {
                break;
            }
        }
        for path in &self.procs {
            let fd = libc::open(path.as_ptr(), libc::O_WRONLY | libc::O_CLOEXEC);
            if fd < 0 {
                return Err(std::io::Error::last_os_error());
            }
            let w = libc::write(fd, buf[i..].as_ptr().cast(), buf.len() - i);
            libc::close(fd);
            if w < 0 {
                return Err(std::io::Error::last_os_error());
            }
        }
        for (resource, value) in &self.rlimits {
            let lim = libc::rlimit {
                rlim_cur: *value,
                rlim_max: *value,
            };
            if libc::setrlimit(*resource as _, &lim) != 0 {
                return Err(std::io::Error::last_os_error());
            }
        }
        if let Some(uid) = self.uid {
            if libc::setgroups(0, std::ptr::null()) != 0
                || libc::setgid(uid) != 0
                || libc::setuid(uid) != 0
            {
                return Err(std::io::Error::last_os_error());
            }
        }
        // After the uid change, which would otherwise clear it.
        #[cfg(target_os = "linux")]
        if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) != 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_refuses_unenforceable_hard_limits_only_in_require_mode() {
        let limits = Limits {
            memory_mb: Some(512),
            ..Default::default()
        };
        assert!(plan(Mode::Require, &limits, None, true)
            .unwrap_err()
            .to_string()
            .contains("memory"));
        let notes = plan(Mode::Auto, &limits, None, true).unwrap();
        assert!(notes.iter().any(|n| n.contains("NOT ENFORCED")));
        assert!(plan(Mode::Off, &limits, None, true).unwrap().is_empty());
        assert!(plan(Mode::Require, &Limits::default(), None, true)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn disk_only_limits_need_no_cgroup() {
        let limits = Limits {
            disk_mb: Some(100),
            open_files: Some(256),
            ..Default::default()
        };
        let notes = plan(Mode::Require, &limits, None, false).unwrap();
        assert!(notes.iter().any(|n| n.contains("soft quota")));
    }

    #[test]
    fn nproc_without_uid_isolation_is_called_out() {
        let limits = Limits {
            pids: Some(64),
            ..Default::default()
        };
        let cg = Cgroups {
            flavor: Flavor::V1,
            root: PathBuf::from("/nonexistent"),
        };
        let notes = plan(Mode::Auto, &limits, Some(&cg), false).unwrap();
        assert!(notes.iter().any(|n| n.contains("RLIMIT_NPROC")));
    }

    #[test]
    fn preexec_sets_the_expected_rlimits() {
        let limits = Limits {
            open_files: Some(100),
            disk_mb: Some(10),
            pids: Some(32),
            ..Default::default()
        };
        let p = PreExec::new(&Joined::default(), &limits, Some(41000));
        let get = |r: i32| p.rlimits.iter().find(|(x, _)| *x == r).map(|(_, v)| *v);
        assert_eq!(get(libc::RLIMIT_NOFILE as i32), Some(100));
        assert_eq!(get(libc::RLIMIT_FSIZE as i32), Some(10 * 1024 * 1024));
        assert_eq!(get(libc::RLIMIT_NPROC as i32), Some(32));
        assert_eq!(get(libc::RLIMIT_CORE as i32), Some(0));
    }
}
