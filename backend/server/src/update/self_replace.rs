//! Recovery of the Windows running image at its configured executable path.
use super::{
    evidence, file_digest, open_sized_target, print_json, MAX_ARTIFACT_BYTES, UPDATE_SCHEMA,
};
use crate::config::{Config, JResult};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::ffi::OsString;
use std::fs::{self, File, TryLockError};
use std::io::{self, ErrorKind};
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::ptr::null_mut;
use windows_sys::Win32::Storage::FileSystem::{MoveFileExW, ReplaceFileW};

const BACKUP_PREFIX: &str = ".jani-self-backup-";
const LOCK_PREFIX: &str = ".jani-self-lock-";
const SCHEMA: &str = "jani.self-update-recovery/v1";
const RECORD_LIMIT: u64 = 4096;

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Fingerprint {
    size: u64,
    digest: String,
}

impl Fingerprint {
    fn read(path: &Path) -> JResult<Option<Self>> {
        let metadata = match fs::symlink_metadata(path) {
            Ok(metadata) if evidence::regular(&metadata) => metadata,
            Ok(_) => {
                return Err(format!(
                    "self-update path {} is not a plain file",
                    path.display()
                ))
            }
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(format!("cannot inspect {} ({error})", path.display())),
        };
        let size = metadata.len();
        let file = open_sized_target(path, size)?;
        Ok(Some(Self {
            size,
            digest: file_digest(&file, size)?,
        }))
    }

    fn valid(&self) -> bool {
        (1..=MAX_ARTIFACT_BYTES).contains(&self.size)
            && self.digest.len() == 64
            && self
                .digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Intent {
    schema: String,
    binding: String,
    incumbent: Fingerprint,
    candidate: Fingerprint,
    candidate_version: String,
    pending: bool,
}

struct Target {
    path: PathBuf,
    backup: PathBuf,
    binding: String,
}

impl Target {
    fn new(raw: &Path) -> JResult<Self> {
        let path = evidence::canonical_target(raw)?;
        let name = path
            .file_name()
            .ok_or("self-update target has no filename")?;
        if reserved(&path)
            || name
                .encode_wide()
                .any(|unit| unit == u16::from(b':') || unit == 0)
        {
            return Err("self-update recovery requires the original executable path".into());
        }
        let binding = crate::types::sha256_hex(path.as_os_str().as_encoded_bytes());
        let backup = sibling(&path, BACKUP_PREFIX)?;
        Ok(Self {
            path,
            backup,
            binding,
        })
    }

    fn record_name(&self) -> String {
        format!("self-{}.json", self.binding)
    }

    fn lock(&self) -> JResult<File> {
        let path = sibling(&self.path, LOCK_PREFIX)?;
        let file = evidence::file_options()
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(|error| {
                format!(
                    "cannot open self-update target lock {} ({error})",
                    path.display()
                )
            })?;
        if !evidence::regular(&file.metadata().map_err(|error| error.to_string())?) {
            return Err("self-update target lock is not a plain file".into());
        }
        match file.try_lock() {
            Ok(()) => Ok(file),
            Err(TryLockError::WouldBlock) => {
                Err("another self-update is using this executable path".into())
            }
            Err(TryLockError::Error(error)) => {
                Err(format!("cannot lock self-update target ({error})"))
            }
        }
    }

    fn load(&self, store: &evidence::Store<'_>) -> JResult<Option<Intent>> {
        let Some(bytes) = store.read_record(&self.record_name(), RECORD_LIMIT)? else {
            return Ok(None);
        };
        let intent: Intent = serde_json::from_slice(&bytes)
            .map_err(|error| format!("invalid self-update recovery record ({error})"))?;
        if intent.schema != SCHEMA
            || intent.binding != self.binding
            || !intent.incumbent.valid()
            || !intent.candidate.valid()
            || super::Version::parse(&intent.candidate_version).is_none()
        {
            return Err("self-update recovery record does not match its target or schema".into());
        }
        Ok(Some(intent))
    }

    fn save(&self, store: &evidence::Store<'_>, intent: &Intent) -> JResult<()> {
        let bytes = serde_json::to_vec(intent).map_err(|error| error.to_string())?;
        store.write_record(&self.record_name(), &bytes, RECORD_LIMIT)
    }

    fn settle(&self, store: &evidence::Store<'_>, intent: &mut Intent) -> JResult<()> {
        intent.pending = false;
        self.save(store, intent)
    }

    fn restore(&self, intent: &Intent) -> JResult<()> {
        if Fingerprint::read(&self.backup)?.as_ref() != Some(&intent.incumbent) {
            return Err(format!(
                "self-update backup {} does not match the recorded incumbent",
                self.backup.display()
            ));
        }
        let from = wide(&self.backup)?;
        let to = wide(&self.path)?;
        // SAFETY: Both buffers are live, NUL-terminated UTF-16 paths. The flags
        // deliberately exclude replacement and cross-volume copy.
        let moved = unsafe { MoveFileExW(from.as_ptr(), to.as_ptr(), 0) };
        if moved == 0 {
            let error = io::Error::last_os_error();
            return Err(format!(
                "cannot restore self-update backup {} ({error})",
                self.backup.display()
            ));
        }
        if Fingerprint::read(&self.path)?.as_ref() != Some(&intent.incumbent) {
            return Err(
                "restored self-update executable does not match its recovery record".into(),
            );
        }
        Ok(())
    }
}

fn sibling(path: &Path, prefix: &str) -> JResult<PathBuf> {
    let mut name = OsString::from(prefix);
    name.push(
        path.file_name()
            .ok_or("self-update target has no filename")?,
    );
    Ok(path.with_file_name(name))
}

fn wide(path: &Path) -> JResult<Vec<u16>> {
    let mut name: Vec<_> = path.as_os_str().encode_wide().collect();
    if name.contains(&0) {
        return Err("self-update path contains an embedded NUL".into());
    }
    name.push(0);
    Ok(name)
}

pub(super) fn reserved(path: &Path) -> bool {
    path.file_name().is_some_and(|name| {
        let name = name.to_string_lossy().to_ascii_lowercase();
        name.starts_with(BACKUP_PREFIX) || name.starts_with(LOCK_PREFIX)
    })
}

pub(super) fn recovery_required(
    store: &evidence::Store<'_>,
    path: &Path,
    present: bool,
) -> JResult<Option<String>> {
    let target = Target::new(path)?;
    let intent = target.load(store)?;
    let backup_exists = match fs::symlink_metadata(&target.backup) {
        Ok(_) => true,
        Err(error) if error.kind() == ErrorKind::NotFound => false,
        Err(error) => {
            return Err(format!(
                "cannot inspect self-update backup {} ({error})",
                target.backup.display()
            ))
        }
    };
    if backup_exists && intent.is_none() {
        return Ok(Some(format!(
            "preserving unverified self-update backup {}",
            target.backup.display()
        )));
    }
    if intent.as_ref().is_some_and(|intent| intent.pending) || (!present && backup_exists) {
        return Ok(Some(format!(
            "self-update recovery is required for {}",
            target.path.display()
        )));
    }
    Ok(None)
}

/// The caller holds the state writer lock and has verified the staged candidate.
pub(super) fn replace(
    store: &evidence::Store<'_>,
    path: &Path,
    staged: &Path,
    release: &super::Release,
    recovery_required: &mut bool,
) -> JResult<Option<PathBuf>> {
    let target = Target::new(path)?;
    let _lock = target.lock()?;
    let incumbent = Fingerprint::read(&target.path)?
        .ok_or("self-update executable disappeared before replacement")?;
    let previous = target.load(store)?;
    if previous.as_ref().is_some_and(|intent| intent.pending) {
        *recovery_required = true;
        return Err(format!(
            "self-update recovery is required for {}",
            target.path.display()
        ));
    }
    if previous.as_ref().is_some_and(|intent| {
        intent.candidate == incumbent && intent.candidate_version != env!("CARGO_PKG_VERSION")
    }) {
        return Err("the installed executable has already changed. Restart the running Jani before updating it again".into());
    }
    if let Some(existing) = Fingerprint::read(&target.backup)? {
        let backup_is_known = previous
            .as_ref()
            .is_some_and(|intent| intent.incumbent == existing);
        let current_is_known = previous
            .as_ref()
            .is_some_and(|intent| intent.candidate == incumbent || intent.incumbent == incumbent)
            || store
                .read(&target.path)?
                .iter()
                .flatten()
                .any(|proof| proof.known.matches(incumbent.size, &incumbent.digest));
        if !backup_is_known || !current_is_known {
            return Err(format!(
                "preserving unverified self-update backup {}",
                target.backup.display()
            ));
        }
        fs::remove_file(&target.backup).map_err(|error| {
            format!(
                "cannot retire the verified previous self-update backup {} ({error})",
                target.backup.display()
            )
        })?;
    }
    let mut intent = Intent {
        schema: SCHEMA.into(),
        binding: target.binding.clone(),
        incumbent,
        candidate: Fingerprint {
            size: release.artifact.size_bytes,
            digest: release.artifact.sha256.to_ascii_lowercase(),
        },
        candidate_version: release.version.clone(),
        pending: true,
    };
    let installed = wide(&target.path)?;
    let staged = wide(&evidence::canonical_target(staged)?)?;
    let backup = wide(&target.backup)?;
    target.save(store, &intent)?;

    *recovery_required = true;
    // SAFETY: The three path buffers remain live through the call. Reserved
    // pointers are null and no metadata-merge errors are ignored.
    let replaced = unsafe {
        ReplaceFileW(
            installed.as_ptr(),
            staged.as_ptr(),
            backup.as_ptr(),
            0,
            null_mut(),
            null_mut(),
        )
    };
    if replaced != 0 {
        if Fingerprint::read(&target.path)?.as_ref() != Some(&intent.candidate)
            || Fingerprint::read(&target.backup)?.as_ref() != Some(&intent.incumbent)
        {
            return Err("Windows reported self-update success but the executable or backup did not match the recovery record".into());
        }
        target.settle(store, &mut intent)?;
        *recovery_required = false;
        return Ok(Some(target.backup));
    }

    // ReplaceFileW can move the incumbent even when it returns failure.
    let error = io::Error::last_os_error();
    match Fingerprint::read(&target.path) {
        Ok(Some(current)) if current == intent.incumbent => {
            target.settle(store, &mut intent)?;
            *recovery_required = false;
            Err(format!(
                "Windows could not replace the running image ({error}). The incumbent is intact"
            ))
        }
        Ok(None) => match target.restore(&intent) {
            Ok(()) => {
                target.settle(store, &mut intent)?;
                *recovery_required = false;
                Err(format!("Windows could not replace the running image ({error}). The verified incumbent was restored"))
            }
            Err(restore) => Err(format!(
                "Windows could not replace the running image ({error}). {restore}"
            )),
        },
        _ => Err(format!(
            "Windows could not verify the outcome of self-replacement ({error}). Backup path is {}",
            target.backup.display()
        )),
    }
}

pub(super) fn recover(cfg: &Config) -> JResult<()> {
    let [requested] = cfg.update_targets.as_slice() else {
        return Err("update recover requires exactly one --to ORIGINAL argument".into());
    };
    let target = Target::new(&std::path::absolute(requested).map_err(|error| error.to_string())?)?;
    let store = evidence::Store::new(cfg, "");
    let _state_lock = store.lock()?;
    let _target_lock = target.lock()?;
    let mut intent = target
        .load(&store)?
        .ok_or("no prepared self-update recovery record for this executable and state directory")?;
    let status = match Fingerprint::read(&target.path)? {
        Some(current) if current == intent.incumbent || current == intent.candidate => {
            "already-present"
        }
        Some(_) => {
            return Err("self-update recovery refused to overwrite an unexpected executable".into())
        }
        None => {
            target.restore(&intent)?;
            "restored"
        }
    };
    if intent.pending {
        target.settle(&store, &mut intent)?;
    }
    print_json(&json!({
        "schemaVersion": UPDATE_SCHEMA,
        "action": "recover",
        "status": status,
        "target": target.path.display().to_string(),
    }))
}
