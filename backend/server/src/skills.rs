use crate::config::{Config, JResult};
use std::collections::{BTreeMap, VecDeque};
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};

const MAX_SKILLS: usize = 64;
const MAX_DEPTH: usize = 2;
const MAX_SKILL_BYTES: u64 = 256_000;
const SKILL_FILE: &str = "SKILL.md";

fn metadata_is_link(metadata: &fs::Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        false
    }
}

fn valid_skill_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name.as_bytes()[0].is_ascii_alphanumeric()
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

fn read_limited(path: &Path, limit: u64) -> JResult<Option<String>> {
    let file = File::open(path).map_err(|e| e.to_string())?;
    let mut bytes = Vec::new();
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > limit {
        return Ok(None);
    }
    String::from_utf8(bytes)
        .map(Some)
        .map_err(|e| e.to_string())
}

#[derive(Clone, Debug)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub file: PathBuf,
    pub boundary: PathBuf,
    pub worktree_path: Option<String>,
}

/// Skill search locations relative to the user home or repository root. Both
/// agent convention directories and the legacy plain `skills/` dir are probed.
const AGENTS_SKILLS_DIR: &str = ".agents/skills";
const CLAUDE_SKILLS_DIR: &str = ".claude/skills";
const PLAIN_SKILLS_DIR: &str = "skills";

#[derive(Default)]
pub struct Catalog {
    pub skills: BTreeMap<String, Skill>,
}

fn yaml_header(content: &str) -> (Option<String>, Option<String>) {
    let mut name = None;
    let mut description = None;
    if !content.starts_with("---\n") && !content.starts_with("---\r\n") {
        return (name, description);
    }
    for line in content.lines().skip(1) {
        if line.trim() == "---" {
            break;
        }
        if let Some((key, val)) = line.split_once(':') {
            let v = val.trim().trim_matches('"').trim_matches('\'').to_string();
            match key.trim() {
                "name" => name = Some(v),
                "description" => description = Some(v),
                _ => {}
            }
        }
    }
    (name, description)
}

impl Catalog {
    pub fn load(cfg: &Config) -> JResult<Self> {
        // User-level skills win over repository copies so an untrusted checkout cannot
        // shadow a skill the operator installed deliberately.
        let worktree_root = cfg.root.canonicalize().map_err(|e| e.to_string())?;
        let mut dirs = Vec::new();
        if let Some(home) = crate::config::home_dir() {
            dirs.push((home.join(AGENTS_SKILLS_DIR), false));
        }
        dirs.push((worktree_root.join(AGENTS_SKILLS_DIR), true));
        dirs.push((worktree_root.join(CLAUDE_SKILLS_DIR), true));
        dirs.push((worktree_root.join(PLAIN_SKILLS_DIR), true));
        if let Some(extra) = std::env::var_os(crate::env::SKILL_DIRS) {
            dirs.extend(std::env::split_paths(&extra).map(|path| (path, false)));
        }
        let mut items = Self::default();
        for (dir, worktree) in dirs {
            let root_metadata = match fs::symlink_metadata(&dir) {
                Ok(metadata) => metadata,
                Err(_) => continue,
            };
            if worktree && metadata_is_link(&root_metadata) {
                continue;
            }
            let canonical = match dir.canonicalize() {
                Ok(path) => path,
                Err(_) => continue,
            };
            if !canonical.is_dir() || (worktree && !canonical.starts_with(&worktree_root)) {
                continue;
            }
            let boundary = if worktree {
                worktree_root.clone()
            } else {
                canonical.clone()
            };
            let mut queue = VecDeque::from([(canonical, 0usize)]);
            while let Some((path, depth)) = queue.pop_front() {
                if items.skills.len() >= MAX_SKILLS {
                    break;
                }
                if depth > MAX_DEPTH {
                    continue;
                }
                let entries = match fs::read_dir(&path) {
                    Ok(entries) => entries,
                    Err(_) => continue,
                };
                for entry in entries.flatten() {
                    let p = entry.path();
                    let metadata = match fs::symlink_metadata(&p) {
                        Ok(metadata) => metadata,
                        Err(_) => continue,
                    };
                    if metadata_is_link(&metadata) {
                        continue;
                    }
                    let kind = metadata.file_type();
                    if kind.is_dir() && depth < MAX_DEPTH {
                        queue.push_back((p, depth + 1));
                        continue;
                    }
                    if !kind.is_file() || p.file_name().and_then(|n| n.to_str()) != Some(SKILL_FILE)
                    {
                        continue;
                    }
                    let body = match read_limited(&p, MAX_SKILL_BYTES) {
                        Ok(Some(body)) => body,
                        _ => continue,
                    };
                    let (name, description) = yaml_header(&body);
                    let fallback = p
                        .parent()
                        .and_then(Path::file_name)
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned();
                    let name = name.filter(|n| !n.is_empty()).unwrap_or(fallback);
                    if !valid_skill_name(&name)
                        || description.as_ref().is_some_and(|value| {
                            value.len() > 500 || value.chars().any(char::is_control)
                        })
                        || items.skills.contains_key(&name)
                    {
                        continue;
                    }
                    let worktree_path = if worktree {
                        let Some(relative) = p.strip_prefix(&worktree_root).ok() else {
                            continue;
                        };
                        let Some(relative) = relative.to_str() else {
                            continue;
                        };
                        Some(relative.replace('\\', "/"))
                    } else {
                        None
                    };
                    items.skills.insert(
                        name.clone(),
                        Skill {
                            name,
                            description: description.unwrap_or_default(),
                            file: p,
                            boundary: boundary.clone(),
                            worktree_path,
                        },
                    );
                }
            }
        }
        Ok(items)
    }

    pub fn listing(&self) -> String {
        if self.skills.is_empty() {
            return "No SKILL.md files discovered".into();
        }
        self.skills
            .values()
            .map(|s| format!("{} — {} [{}]", s.name, s.description, s.file.display()))
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub fn worktree_path(&self, name: &str) -> Option<&str> {
        self.skills
            .get(name)
            .and_then(|skill| skill.worktree_path.as_deref())
    }

    pub fn read(&self, name: &str) -> JResult<String> {
        let skill = self
            .skills
            .get(name)
            .ok_or_else(|| format!("unknown skill '{name}'; use listed skill names"))?;
        let relative = skill
            .file
            .strip_prefix(&skill.boundary)
            .map_err(|_| "skill path is outside its trusted discovery root".to_string())?;
        let boundary_metadata = fs::symlink_metadata(&skill.boundary).map_err(|e| e.to_string())?;
        if metadata_is_link(&boundary_metadata) || !boundary_metadata.is_dir() {
            return Err("skill discovery root was replaced by a link".into());
        }
        let mut cursor = skill.boundary.clone();
        for component in relative.components() {
            cursor.push(component.as_os_str());
            let metadata = fs::symlink_metadata(&cursor).map_err(|e| e.to_string())?;
            if metadata_is_link(&metadata) {
                return Err("symlinked skill path component refused".into());
            }
        }
        let metadata = fs::symlink_metadata(&skill.file).map_err(|e| e.to_string())?;
        if !metadata.is_file() || metadata.len() > MAX_SKILL_BYTES {
            return Err("skill is not a bounded regular file".into());
        }
        let resolved = skill.file.canonicalize().map_err(|e| e.to_string())?;
        let boundary = skill.boundary.canonicalize().map_err(|e| e.to_string())?;
        if !resolved.starts_with(boundary) {
            return Err("skill path escapes its trusted discovery root".into());
        }
        read_limited(&skill.file, MAX_SKILL_BYTES)?.ok_or_else(|| "skill too large".to_string())
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
            allow_write: false,
            allow_exec: false,
            write_paths: vec![],
            exec_programs: vec![],
            ..Config::baseline()
        }
    }

    fn write_skill(dir: &Path, name: &str) -> PathBuf {
        let file = dir.join("SKILL.md");
        fs::create_dir_all(dir).unwrap();
        fs::write(
            &file,
            format!("---\nname: {name}\ndescription: test\n---\n# Rules\n"),
        )
        .unwrap();
        file
    }

    #[test]
    fn header_parser() {
        let (a, b) = yaml_header("---\nname: foo\ndescription: bar\n---\n# Hello");
        assert_eq!(a.as_deref(), Some("foo"));
        assert_eq!(b.as_deref(), Some("bar"));
    }

    #[test]
    fn earlier_skill_directory_wins() {
        let root =
            std::env::temp_dir().join(format!("jani-skills-{}", crate::process::unique_id()));
        let name = format!("jani-order-{}", std::process::id());
        let first = write_skill(&root.join(".agents/skills/a"), &name);
        write_skill(&root.join("skills/b"), &name);
        let cfg = test_config(root.clone());
        let catalog = Catalog::load(&cfg).unwrap();
        assert_eq!(
            fs::canonicalize(&catalog.skills[&name].file).unwrap(),
            fs::canonicalize(&first).unwrap()
        );
        let _ = fs::remove_dir_all(&root);
    }

    fn symlink_dir(target: &Path, link: &Path) -> bool {
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(target, link).unwrap();
            true
        }
        #[cfg(windows)]
        match std::os::windows::fs::symlink_dir(target, link) {
            Ok(()) => true,
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::Unsupported
                ) || error.raw_os_error() == Some(1314) =>
            {
                false
            }
            Err(error) => panic!("cannot create test directory symlink: {error}"),
        }
    }

    fn symlink_file(target: &Path, link: &Path) -> bool {
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(target, link).unwrap();
            true
        }
        #[cfg(windows)]
        match std::os::windows::fs::symlink_file(target, link) {
            Ok(()) => true,
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::Unsupported
                ) || error.raw_os_error() == Some(1314) =>
            {
                false
            }
            Err(error) => panic!("cannot create test file symlink: {error}"),
        }
    }

    #[test]
    fn frontmatter_names_cannot_inject_prompt_text() {
        let root =
            std::env::temp_dir().join(format!("jani-skill-names-{}", crate::process::unique_id()));
        fs::create_dir_all(&root).unwrap();
        for (directory, name) in [
            ("space", "bad name"),
            ("colon", "bad:name"),
            ("traversal", "../escape"),
            ("safe", "safe-skill"),
        ] {
            write_skill(&root.join("skills").join(directory), name);
        }
        let catalog = Catalog::load(&test_config(root.clone())).unwrap();
        assert!(catalog.skills.contains_key("safe-skill"));
        for name in ["bad name", "bad:name", "../escape"] {
            assert!(!catalog.skills.contains_key(name), "{name}");
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn repository_skill_roots_cannot_escape_through_symlinks() {
        let root =
            std::env::temp_dir().join(format!("jani-skill-root-{}", crate::process::unique_id()));
        let external = std::env::temp_dir().join(format!(
            "jani-skill-external-{}",
            crate::process::unique_id()
        ));
        fs::create_dir_all(root.join(".claude")).unwrap();
        let name = format!("external-{}", std::process::id());
        write_skill(&external, &name);
        if !symlink_dir(&external, &root.join(".claude/skills")) {
            fs::remove_dir_all(&root).unwrap();
            fs::remove_dir_all(&external).unwrap();
            return;
        }
        let catalog = Catalog::load(&test_config(root.clone())).unwrap();
        assert!(!catalog.skills.contains_key(&name));
        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(external).unwrap();
    }

    #[test]
    fn skill_reads_reject_a_leaf_swapped_for_a_symlink() {
        let root =
            std::env::temp_dir().join(format!("jani-skill-leaf-{}", crate::process::unique_id()));
        let external = std::env::temp_dir().join(format!(
            "jani-skill-leaf-ext-{}",
            crate::process::unique_id()
        ));
        let name = format!("leaf-{}", std::process::id());
        let file = write_skill(&root.join("skills").join(&name), &name);
        let outside = write_skill(&external, &name);
        let catalog = Catalog::load(&test_config(root.clone())).unwrap();
        assert!(catalog.read(&name).is_ok());
        fs::remove_file(&file).unwrap();
        if !symlink_file(&outside, &file) {
            fs::remove_dir_all(&root).unwrap();
            fs::remove_dir_all(&external).unwrap();
            return;
        }
        assert!(catalog.read(&name).is_err());
        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(external).unwrap();
    }
}
