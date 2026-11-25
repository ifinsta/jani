//! Fail-closed org/tenant kill switch persisted in `state_dir/kill-switch.json`.
use crate::process::unique_id;
use crate::types::now_unix;
use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

pub const KILL_SWITCH_FILE: &str = "kill-switch.json";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct KillSwitchRecord {
    #[serde(default)]
    pub tenant: Option<String>,
    pub reason: String,
    pub actor: String,
    pub activated_at: u64,
}

fn switch_path(state_dir: &Path) -> PathBuf {
    state_dir.join(KILL_SWITCH_FILE)
}

pub fn is_killed(state_dir: &Path, tenant: &str) -> Result<Option<KillSwitchRecord>, String> {
    let path = switch_path(state_dir);
    let meta = match fs::symlink_metadata(&path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("cannot stat {}: {e}", path.display())),
    };
    if meta.file_type().is_symlink() || !meta.is_file() {
        return Err(format!(
            "kill switch {} must be a regular non-symlink file",
            path.display()
        ));
    }
    let raw = fs::read_to_string(&path)
        .map_err(|e| format!("cannot read kill switch {}: {e}", path.display()))?;
    let rec: KillSwitchRecord = serde_json::from_str(&raw).map_err(|e| {
        format!(
            "corrupt kill switch {} (failing closed): {e}",
            path.display()
        )
    })?;
    if rec.tenant.is_none() || rec.tenant.as_deref() == Some(tenant) {
        Ok(Some(rec))
    } else {
        Ok(None)
    }
}

pub fn activate(
    state_dir: &Path,
    tenant: Option<&str>,
    actor: &str,
    reason: &str,
) -> Result<KillSwitchRecord, String> {
    if actor.trim().is_empty() {
        return Err("kill switch actor cannot be empty".into());
    }
    fs::create_dir_all(state_dir).map_err(|e| e.to_string())?;
    let rec = KillSwitchRecord {
        tenant: tenant.map(str::to_string),
        reason: if reason.trim().is_empty() {
            "emergency operator kill switch".into()
        } else {
            reason.trim().to_string()
        },
        actor: actor.trim().to_string(),
        activated_at: now_unix(),
    };
    let encoded = serde_json::to_vec_pretty(&rec).map_err(|e| e.to_string())?;
    let path = switch_path(state_dir);
    let tmp = state_dir.join(format!(".{KILL_SWITCH_FILE}.tmp-{}", unique_id()));
    let mut opts = OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut file = opts.open(&tmp).map_err(|e| e.to_string())?;
    file.write_all(&encoded).map_err(|e| e.to_string())?;
    let _ = file.sync_all();
    drop(file);
    fs::rename(&tmp, &path).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        e.to_string()
    })?;
    Ok(rec)
}

pub fn clear(state_dir: &Path) -> Result<bool, String> {
    let path = switch_path(state_dir);
    match fs::remove_file(&path) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(format!("cannot clear {}: {e}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn global_and_tenant_kill_switch_and_corrupt_fails_closed() {
        let dir = std::env::temp_dir().join(format!("jani-kill-{}", unique_id()));
        assert!(is_killed(&dir, "acme").unwrap().is_none());

        activate(&dir, Some("acme"), "sec-admin", "incident-1").unwrap();
        assert!(is_killed(&dir, "acme").unwrap().is_some());
        assert!(is_killed(&dir, "other").unwrap().is_none());

        clear(&dir).unwrap();
        assert!(is_killed(&dir, "acme").unwrap().is_none());

        activate(&dir, None, "sec-admin", "fleet-halt").unwrap();
        assert!(is_killed(&dir, "other").unwrap().is_some());

        fs::write(switch_path(&dir), b"{broken").unwrap();
        assert!(is_killed(&dir, "acme").is_err());
        let _ = fs::remove_dir_all(&dir);
    }
}
