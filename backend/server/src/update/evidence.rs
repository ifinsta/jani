//! Signed release manifests retained for configured update targets.
use super::{KnownRelease, Manifest, MAX_METADATA_BYTES};
use crate::config::{Config, JResult};
use ring::digest::{Context, SHA256};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs::{self, File, Metadata, OpenOptions, TryLockError};
use std::io::{ErrorKind, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

const MAX_RECORD_BYTES: u64 = MAX_METADATA_BYTES * 2 + 4096;
const SCHEMA: &str = "jani.update-proof/v1";

pub(super) struct Store<'a> {
    cfg: &'a Config,
    source: &'a str,
    root: PathBuf,
}

pub(super) struct Proof {
    pub(super) known: KnownRelease,
    pub(super) manifest: Arc<Manifest>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Record<M> {
    schema: String,
    binding: String,
    incumbent: M,
    candidate: M,
}

impl<'a> Store<'a> {
    pub(super) fn new(cfg: &'a Config, source: &'a str) -> Self {
        Self {
            cfg,
            source,
            root: cfg.state_dir.join("update-proofs"),
        }
    }

    fn binding(&self, target: &Path) -> JResult<String> {
        let path = canonical_target(target)?;
        let mut hash = Context::new(&SHA256);
        hash.update(b"jani.update-proof-target/v1\0");
        hash.update(self.source.as_bytes());
        hash.update(b"\0");
        hash.update(path.as_os_str().as_encoded_bytes());
        Ok(hex::encode(hash.finish()))
    }

    pub(super) fn staging_path(&self, directory: &Path, name: &str) -> JResult<PathBuf> {
        let root = self
            .root
            .canonicalize()
            .map_err(|error| error.to_string())?;
        let digest = ring::digest::digest(&SHA256, root.as_os_str().as_encoded_bytes());
        Ok(directory.join(format!(
            ".{name}.jani-update-{}.tmp",
            hex::encode(&digest.as_ref()[..16])
        )))
    }

    pub(super) fn lock(&self) -> JResult<File> {
        fs::create_dir_all(&self.cfg.state_dir)
            .map_err(|error| format!("cannot create updater state: {error}"))?;
        if !self.root_exists()? {
            #[cfg(unix)]
            let created = {
                use std::os::unix::fs::DirBuilderExt;
                fs::DirBuilder::new().mode(0o700).create(&self.root)
            };
            #[cfg(not(unix))]
            let created = fs::create_dir(&self.root);
            match created {
                Ok(()) => (),
                Err(error) if error.kind() == ErrorKind::AlreadyExists && self.root_exists()? => (),
                Err(error) => return Err(format!("cannot create update proof directory: {error}")),
            }
            #[cfg(unix)]
            File::open(&self.cfg.state_dir)
                .and_then(|dir| dir.sync_all())
                .map_err(|error| format!("cannot flush update proof directory entry: {error}"))?;
        }
        let file = file_options()
            .write(true)
            .create(true)
            .truncate(false)
            .open(self.root.join(".lock"))
            .map_err(|error| format!("cannot open update lock: {error}"))?;
        if !regular(&file.metadata().map_err(|error| error.to_string())?) {
            return Err("update lock is not a plain file".into());
        }
        match file.try_lock() {
            Ok(()) => Ok(file),
            Err(TryLockError::WouldBlock) => {
                Err("another updater is using this state directory".into())
            }
            Err(TryLockError::Error(error)) => Err(format!("cannot lock updater state: {error}")),
        }
    }

    pub(super) fn read_lock(&self) -> JResult<Option<File>> {
        if !self.root_exists()? {
            return Ok(None);
        }
        let lock = match file_options().read(true).open(self.root.join(".lock")) {
            Ok(lock) => lock,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(format!("cannot open update lock: {error}")),
        };
        if !regular(&lock.metadata().map_err(|error| error.to_string())?) {
            return Err("update lock is not a plain file".into());
        }
        match lock.try_lock_shared() {
            Ok(()) => Ok(Some(lock)),
            Err(TryLockError::WouldBlock) => {
                Err("another updater is using this state directory".into())
            }
            Err(TryLockError::Error(error)) => Err(format!("cannot lock updater state: {error}")),
        }
    }

    pub(super) fn read(&self, target: &Path) -> JResult<[Option<Proof>; 2]> {
        let binding = self.binding(target)?;
        let Some(bytes) = self.read_record(&format!("{binding}.json"), MAX_RECORD_BYTES)? else {
            return Ok([None, None]);
        };
        let parsed = serde_json::from_slice::<Record<Value>>(&bytes)
            .map_err(|error| format!("invalid update proof record: {error}"))
            .and_then(|record| {
                if record.schema != SCHEMA || record.binding != binding {
                    return Err("update proof schema or target binding does not match".into());
                }
                let parse_slot = |slot| {
                    serde_json::from_value::<Option<Manifest>>(slot)
                        .map_err(|error| format!("invalid manifest in update proof: {error}"))
                };
                Ok([parse_slot(record.incumbent)?, parse_slot(record.candidate)?])
            });
        let slots = match parsed {
            Ok(slots) => slots,
            Err(_) if self.cfg.update_force => return Ok([None, None]),
            Err(error) => return Err(error),
        };
        Ok(slots.map(|slot| {
            let manifest = slot?;
            let artifact = manifest
                .trusted_artifact(self.cfg, &manifest.tag, "local update proof")
                .ok()?;
            Some(Proof {
                known: KnownRelease::new(&manifest.version, &artifact),
                manifest: Arc::new(manifest),
            })
        }))
    }

    /// Called only while holding the state writer lock.
    pub(super) fn write(
        &self,
        target: &Path,
        incumbent: Option<&Manifest>,
        candidate: Option<&Manifest>,
    ) -> JResult<()> {
        let binding = self.binding(target)?;
        let name = format!("{binding}.json");
        let record = Record {
            schema: SCHEMA.into(),
            binding,
            incumbent,
            candidate,
        };
        let bytes = serde_json::to_vec(&record).map_err(|error| error.to_string())?;
        self.write_record(&name, &bytes, MAX_RECORD_BYTES)
    }

    pub(super) fn read_record(&self, name: &str, limit: u64) -> JResult<Option<Vec<u8>>> {
        if !self.root_exists()? {
            return Ok(None);
        }
        let file = match file_options().read(true).open(self.root.join(name)) {
            Ok(file) => file,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(format!("cannot open update proof: {error}")),
        };
        let metadata = file.metadata().map_err(|error| error.to_string())?;
        if !regular(&metadata) || metadata.len() > limit {
            return Err("update proof must be a bounded plain file".into());
        }
        let mut bytes = Vec::new();
        file.take(limit + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| format!("cannot read update proof: {error}"))?;
        if bytes.len() as u64 > limit {
            return Err("update proof exceeds its size limit".into());
        }
        Ok(Some(bytes))
    }

    /// Called only while holding the state writer lock.
    pub(super) fn write_record(&self, name: &str, bytes: &[u8], limit: u64) -> JResult<()> {
        if bytes.len() as u64 > limit {
            return Err("update proof exceeds its size limit".into());
        }
        let temporary = self.root.join(".pending");
        clear_staging(&temporary)?;
        let mut file = file_options()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(|error| format!("cannot create update proof: {error}"))?;
        let result = (|| -> std::io::Result<()> {
            file.write_all(bytes)?;
            file.sync_all()?;
            drop(file);
            fs::rename(&temporary, self.root.join(name))?;
            #[cfg(unix)]
            File::open(&self.root)?.sync_all()?;
            Ok(())
        })();
        if let Err(error) = result {
            let _ = fs::remove_file(temporary);
            return Err(format!("cannot publish update proof: {error}"));
        }
        Ok(())
    }

    fn root_exists(&self) -> JResult<bool> {
        match fs::symlink_metadata(&self.root) {
            Ok(metadata) if metadata.is_dir() && !redirected(&metadata) => Ok(true),
            Ok(_) => Err("update proof directory is redirected or is not a directory".into()),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
            Err(error) => Err(format!("cannot inspect update proof directory: {error}")),
        }
    }
}

pub(super) fn clear_staging(path: &Path) -> JResult<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!(
            "cannot clear staged update file {}: {error}",
            path.display()
        )),
    }
}

pub(super) fn canonical_target(target: &Path) -> JResult<PathBuf> {
    match fs::symlink_metadata(target) {
        Ok(metadata) if regular(&metadata) => {
            target.canonicalize().map_err(|error| error.to_string())
        }
        Ok(_) => Err("update target is not a plain file".into()),
        Err(error) if error.kind() == ErrorKind::NotFound => {
            let parent = target.parent().ok_or("update target has no parent")?;
            let name = target.file_name().ok_or("update target has no filename")?;
            Ok(parent
                .canonicalize()
                .map_err(|error| error.to_string())?
                .join(name))
        }
        Err(error) => Err(error.to_string()),
    }
}

pub(super) fn file_options() -> OpenOptions {
    let mut options = OpenOptions::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .mode(0o600);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    options
}

fn redirected(metadata: &Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            return true;
        }
    }
    metadata.file_type().is_symlink()
}

pub(super) fn regular(metadata: &Metadata) -> bool {
    metadata.is_file() && !redirected(metadata)
}
