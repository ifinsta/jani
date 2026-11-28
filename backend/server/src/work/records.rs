use super::Task;
use crate::config::JResult;
use std::fs::{self, File, Metadata, OpenOptions, TryLockError};
use std::io::{ErrorKind, Read, Write};
use std::path::{Path, PathBuf};

pub(super) fn dir(state: &Path) -> PathBuf {
    state.join("work").join("tasks")
}

pub(super) fn lock_queue(state: &Path) -> JResult<File> {
    let root = state.join("work");
    fs::create_dir_all(state).map_err(|e| format!("{}: {e}", state.display()))?;
    create_queue_dir(&root)?;
    create_queue_dir(&dir(state))?;
    let file = open_lock(&root.join(".lock")).map_err(|e| format!("work lock: {e}"))?;
    file.lock().map_err(|e| format!("work lock: {e}"))?;
    Ok(file)
}

pub(super) fn claims_dir(state: &Path) -> JResult<PathBuf> {
    let locks = state.join("work").join("claims");
    create_queue_dir(&locks)?;
    Ok(locks)
}

pub(super) fn try_lock_task(locks: &Path, id: &str) -> JResult<Option<File>> {
    let lock =
        open_lock(&locks.join(id)).map_err(|e| format!("cannot open task lock for {id}: {e}"))?;
    match lock.try_lock() {
        Ok(()) => Ok(Some(lock)),
        Err(TryLockError::WouldBlock) => Ok(None),
        Err(TryLockError::Error(e)) => Err(format!("cannot lock task {id}: {e}")),
    }
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

fn regular(metadata: &Metadata) -> bool {
    metadata.is_file() && !redirected(metadata)
}

fn queue_dir_exists(path: &Path) -> JResult<bool> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !redirected(&metadata) => Ok(true),
        Ok(_) => Err(format!(
            "queue path is not a plain directory: {}",
            path.display()
        )),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(false),
        Err(e) => Err(format!(
            "cannot inspect queue directory {}: {e}",
            path.display()
        )),
    }
}

fn create_queue_dir(path: &Path) -> JResult<()> {
    if !queue_dir_exists(path)? {
        match fs::create_dir(path) {
            Ok(()) => (),
            Err(e) if e.kind() == ErrorKind::AlreadyExists && queue_dir_exists(path)? => (),
            Err(e) => {
                return Err(format!(
                    "cannot create queue directory {}: {e}",
                    path.display()
                ))
            }
        }
    }
    Ok(())
}

fn tasks_dir_exists(state: &Path) -> JResult<bool> {
    Ok(queue_dir_exists(&state.join("work"))? && queue_dir_exists(&dir(state))?)
}

fn record_options() -> OpenOptions {
    let mut options = OpenOptions::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    options
}

fn open_lock(path: &Path) -> JResult<File> {
    let file = record_options()
        .create(true)
        .write(true)
        .truncate(false)
        .open(path)
        .map_err(|e| e.to_string())?;
    if !regular(&file.metadata().map_err(|e| e.to_string())?) {
        return Err("lock path is not a regular file".into());
    }
    Ok(file)
}

fn valid_name(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

fn valid_id(id: &str) -> bool {
    valid_name(id)
        && id
            .strip_prefix("wrk-")
            .is_some_and(|suffix| !suffix.is_empty())
}

pub(super) fn entries(state: &Path) -> JResult<impl Iterator<Item = JResult<(String, PathBuf)>>> {
    let root = dir(state);
    let entries = if tasks_dir_exists(state)? {
        Some(
            fs::read_dir(&root)
                .map_err(|e| format!("cannot list tasks in {}: {e}", root.display()))?,
        )
    } else {
        None
    };
    Ok(entries.into_iter().flatten().filter_map(|entry| {
        let path = match entry {
            Ok(entry) => entry.path(),
            Err(e) => return Some(Err(format!("cannot list task: {e}"))),
        };
        let id = path.file_stem()?.to_str()?;
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") || !valid_name(id) {
            return None;
        }
        Some(Ok((id.to_owned(), path)))
    }))
}

pub(super) fn read(path: &Path, id: &str) -> JResult<Task> {
    if !valid_id(id) {
        return Err(format!("'{id}' is not a task id"));
    }
    let mut file = record_options().read(true).open(path).map_err(|e| {
        if e.kind() == ErrorKind::NotFound {
            format!("no task {id}")
        } else {
            format!("cannot open task {id}: {e}")
        }
    })?;
    let metadata = file
        .metadata()
        .map_err(|e| format!("cannot stat task {id}: {e}"))?;
    if !regular(&metadata) {
        return Err(format!("task {id} is not a regular file"));
    }
    let mut raw = String::new();
    file.read_to_string(&mut raw)
        .map_err(|e| format!("cannot read task {id}: {e}"))?;
    let value = serde_json::from_str(&raw).map_err(|e| format!("invalid task {id}: {e}"))?;
    Task::from_json(&value)
        .filter(|task| task.id == id)
        .ok_or_else(|| format!("invalid task record for {id}"))
}

pub(super) fn get(state: &Path, id: &str) -> JResult<Task> {
    if !valid_id(id) {
        return Err(format!("'{id}' is not a task id"));
    }
    if !tasks_dir_exists(state)? {
        return Err(format!("no task {id}"));
    }
    read(&dir(state).join(format!("{id}.json")), id)
}

pub(super) fn write(state: &Path, task: &Task) -> JResult<()> {
    let path = dir(state).join(format!("{}.json", task.id));
    let temporary = path.with_extension("json.tmp");
    let publish = || -> JResult<()> {
        let bytes = serde_json::to_vec_pretty(&task.to_json()).map_err(|e| e.to_string())?;
        // An interrupted writer can leave a staging file under the queue lock.
        match fs::symlink_metadata(&temporary) {
            Ok(metadata) if regular(&metadata) => {
                fs::remove_file(&temporary).map_err(|e| e.to_string())?
            }
            Ok(_) => return Err("staging path is not a regular file".into()),
            Err(e) if e.kind() == ErrorKind::NotFound => (),
            Err(e) => return Err(e.to_string()),
        }
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)
            .map_err(|e| e.to_string())?;
        let written = file.write_all(&bytes);
        drop(file);
        if let Err(e) = written.and_then(|()| fs::rename(&temporary, &path)) {
            let _ = fs::remove_file(&temporary);
            return Err(e.to_string());
        }
        Ok(())
    };
    publish().map_err(|e| format!("cannot write task {}: {e}", task.id))
}
