use crate::config::{validate_relative, Config, JResult};
use crate::process::unique_id;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

const MAX_FILE_BYTES: u64 = 180_000;
const MAX_LIST_ENTRIES: usize = 240;
const MAX_LIST_DEPTH: u8 = 2;

pub struct Workspace<'a> {
    pub cfg: &'a Config,
}

fn is_secret(p: &Path) -> bool {
    p.components().any(|component| {
        crate::config::protected_component(&component.as_os_str().to_string_lossy())
    })
}

impl<'a> Workspace<'a> {
    fn target(&self, name: &str, must_exist: bool) -> JResult<PathBuf> {
        let path = Path::new(name);
        validate_relative(path)?;
        if is_secret(path) {
            return Err("path is protected or potentially sensitive".into());
        }
        let target = self.cfg.root.join(path);
        // Resolve every existing path component; deny symlink traversal. Avoids ordinary
        // escape-through-symlink paths, but is not atomic against hostile racing processes.
        let canon_root = self
            .cfg
            .root
            .canonicalize()
            .map_err(|e| format!("invalid worktree root: {e}"))?;
        let mut cursor = self.cfg.root.clone();
        for component in path.components() {
            cursor.push(component.as_os_str());
            match fs::symlink_metadata(&cursor) {
                Ok(meta) => {
                    if meta.file_type().is_symlink() {
                        return Err("symlinked path component refused".into());
                    }
                    #[cfg(windows)]
                    {
                        use std::os::windows::fs::MetadataExt;
                        if meta.file_attributes() & 0x400 != 0 {
                            return Err("reparse-point path component refused".into());
                        }
                    }
                    let real = cursor.canonicalize().map_err(|e| e.to_string())?;
                    if !real.starts_with(&canon_root) {
                        return Err("path resolves outside worktree".into());
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
                Err(e) => return Err(e.to_string()),
            }
        }
        if must_exist && !target.is_file() {
            return Err(format!("file not found: {name}"));
        }
        Ok(target)
    }

    pub fn list(&self, dir: &str) -> JResult<String> {
        let base = if dir.is_empty() || dir == "." {
            self.cfg.root.clone()
        } else {
            self.target(dir, false)?
        };
        if !base.is_dir() {
            return Err("not a directory".into());
        }
        let mut found = Vec::new();
        let mut queue = std::collections::VecDeque::from([(base, 0u8)]);
        while let Some((path, depth)) = queue.pop_front() {
            let mut entries = fs::read_dir(path)
                .map_err(|e| e.to_string())?
                .flatten()
                .collect::<Vec<_>>();
            entries.sort_by_key(|e| e.path());
            for e in entries {
                if found.len() >= MAX_LIST_ENTRIES {
                    break;
                }
                let p = e.path();
                let name = p.strip_prefix(&self.cfg.root).map_err(|e| e.to_string())?;
                if is_secret(name) {
                    continue;
                }
                let kind = e.file_type().map_err(|e| e.to_string())?;
                if kind.is_symlink() {
                    continue;
                }
                let relative = name.to_string_lossy().replace('\\', "/");
                if let Some(policy) = &self.cfg.permissions {
                    let request = crate::permissions::Request::Read {
                        path: &relative,
                        directory: kind.is_dir(),
                    };
                    if matches!(
                        policy.check(Some(&request), crate::types::Effect::Read),
                        crate::permissions::Verdict::Deny { .. }
                            | crate::permissions::Verdict::Ask { .. }
                    ) {
                        continue;
                    }
                }
                if kind.is_dir() {
                    found.push(format!("{relative}/"));
                    if depth < MAX_LIST_DEPTH {
                        queue.push_back((p, depth + 1));
                    }
                } else if kind.is_file() {
                    found.push(relative);
                }
            }
            if found.len() >= MAX_LIST_ENTRIES {
                break;
            }
        }
        Ok(found.join("\n"))
    }

    pub fn read(&self, name: &str) -> JResult<String> {
        let target = self.target(name, true)?;
        let info = fs::metadata(&target).map_err(|e| e.to_string())?;
        if info.len() > MAX_FILE_BYTES {
            return Err("file too large; limit 180 KB".into());
        }
        fs::read_to_string(target).map_err(|e| e.to_string())
    }

    pub fn write(&self, name: &str, text: &str) -> JResult<String> {
        if !self.cfg.allow_write {
            return Err("file editing disabled; use --allow-write".into());
        }
        if text.len() as u64 > MAX_FILE_BYTES {
            return Err("write exceeds 180 KB limit".into());
        }
        let rel = Path::new(name);
        if !self
            .cfg
            .write_paths
            .iter()
            .any(|scope| rel.starts_with(scope))
        {
            return Err(format!("file outside allowed --write-path scope: {name}"));
        }
        let target = self.target(name, false)?;
        let parent = target.parent().ok_or("invalid file path")?;
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        let temp = parent.join(format!(".jani-{}.tmp", unique_id()));
        let result = (|| -> JResult<()> {
            let mut opts = OpenOptions::new();
            opts.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                opts.mode(0o600);
            }
            let mut f = opts.open(&temp).map_err(|e| e.to_string())?;
            f.write_all(text.as_bytes()).map_err(|e| e.to_string())?;
            f.sync_all().map_err(|e| e.to_string())?;
            // std::fs::rename replaces the destination on both POSIX and Windows
            // (MoveFileExW with MOVEFILE_REPLACE_EXISTING).
            fs::rename(&temp, &target).map_err(|e| e.to_string())?;
            if let Ok(dir) = File::open(parent) {
                let _ = dir.sync_all();
            }
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temp);
        }
        result?;
        Ok(format!(
            "wrote {name} ({} bytes); not proof of task completion",
            text.len()
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    fn test_config(root: PathBuf) -> Config {
        Config {
            command: "run".into(),
            root,
            allow_write: true,
            allow_exec: false,
            write_paths: vec![PathBuf::from("src")],
            exec_programs: vec![],
            ..Config::baseline()
        }
    }

    fn temp_root() -> PathBuf {
        let root = std::env::temp_dir().join(format!("jani-workspace-{}", unique_id()));
        fs::create_dir_all(root.join("src")).unwrap();
        root
    }

    #[test]
    fn secrets_are_blocked() {
        assert!(is_secret(Path::new("config/.env.production")));
        assert!(is_secret(Path::new(".git/config")));
        assert!(is_secret(Path::new("certs/server.key")));
        assert!(is_secret(Path::new("home/.netrc")));
        assert!(!is_secret(Path::new("src/main.rs")));
    }

    #[test]
    fn root_listings_filter_permission_denied_subtrees() {
        let root = temp_root();
        fs::create_dir_all(root.join("src/private")).unwrap();
        fs::write(root.join("src/private/hidden.txt"), "fixture-hidden").unwrap();
        fs::write(root.join("src/public.txt"), "public").unwrap();
        let mut cfg = test_config(root.clone());
        cfg.permissions = Some(
            crate::permissions::Policy::compile(
                crate::permissions::Mode::Default,
                &[],
                &[],
                &["Read(./src/private/**)".into()],
            )
            .unwrap(),
        );
        let ws = Workspace { cfg: &cfg };
        for directory in ["", ".", "src"] {
            let listing = ws.list(directory).unwrap();
            assert!(listing.contains("public.txt"), "{listing}");
            assert!(!listing.contains("private"), "{listing}");
            assert!(!listing.contains("hidden.txt"), "{listing}");
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn ntfs_default_stream_cannot_alias_a_protected_file() {
        let root = temp_root();
        fs::write(root.join(".env"), "fixture-env-content").unwrap();
        assert_eq!(
            fs::read_to_string(root.join(".env::$DATA")).unwrap(),
            "fixture-env-content"
        );
        let cfg = test_config(root.clone());
        let ws = Workspace { cfg: &cfg };
        for path in [".env", ".ENV", ".env::$DATA", ".ENV::$DATA"] {
            assert!(ws.read(path).is_err(), "{path}");
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn overwrites_existing_file_in_scope() {
        let root = temp_root();
        let cfg = test_config(root.clone());
        let ws = Workspace { cfg: &cfg };
        ws.write("src/lib.rs", "first").unwrap();
        ws.write("src/lib.rs", "second").unwrap();
        assert_eq!(
            fs::read_to_string(root.join("src/lib.rs")).unwrap(),
            "second"
        );
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn write_outside_scope_is_refused() {
        let root = temp_root();
        let cfg = test_config(root.clone());
        let ws = Workspace { cfg: &cfg };
        assert!(ws.write("tests/escape.rs", "x").is_err());
        assert!(ws.write("src/../escape.rs", "x").is_err());
        fs::remove_dir_all(&root).unwrap();
    }
}
