use crate::config::{Config, JResult};
use std::collections::{BTreeMap, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub file: PathBuf,
}

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
        let mut dirs = Vec::new();
        if let Ok(home) = std::env::var(if cfg!(windows) { "USERPROFILE" } else { "HOME" }) {
            dirs.push(PathBuf::from(home).join(".agents/skills"));
        }
        dirs.push(cfg.root.join(".agents/skills"));
        dirs.push(cfg.root.join(".claude/skills"));
        dirs.push(cfg.root.join("skills"));
        if let Some(extra) = std::env::var_os("JANI_SKILL_DIRS") {
            dirs.extend(std::env::split_paths(&extra));
        }
        let mut items = Self::default();
        for dir in dirs {
            if !dir.is_dir() {
                continue;
            }
            let mut queue = VecDeque::from([(dir, 0usize)]);
            while let Some((path, depth)) = queue.pop_front() {
                if items.skills.len() >= 64 {
                    break;
                }
                if depth > 2 {
                    continue;
                }
                let canonical = match path.canonicalize() {
                    Ok(x) => x,
                    Err(_) => continue,
                };
                if !canonical.is_dir() {
                    continue;
                }
                let entries = match fs::read_dir(&canonical) {
                    Ok(x) => x,
                    Err(_) => continue,
                };
                for entry in entries.flatten() {
                    let p = entry.path();
                    let kind = match entry.file_type() {
                        Ok(x) => x,
                        Err(_) => continue,
                    };
                    if kind.is_symlink() {
                        continue;
                    }
                    if kind.is_dir() && depth < 2 {
                        queue.push_back((p, depth + 1));
                    } else if kind.is_file()
                        && p.file_name().and_then(|n| n.to_str()) == Some("SKILL.md")
                    {
                        let md = match fs::metadata(&p) {
                            Ok(m) if m.len() <= 256_000 => m,
                            _ => continue,
                        };
                        let _ = md;
                        let body = match fs::read_to_string(&p) {
                            Ok(x) => x,
                            Err(_) => continue,
                        };
                        let (name, description) = yaml_header(&body);
                        let fallback = p
                            .parent()
                            .and_then(Path::file_name)
                            .unwrap_or_default()
                            .to_string_lossy()
                            .into_owned();
                        let name = name.filter(|n| !n.is_empty()).unwrap_or(fallback);
                        if items.skills.contains_key(&name) {
                            continue;
                        }
                        items.skills.insert(
                            name.clone(),
                            Skill {
                                name,
                                description: description.unwrap_or_default(),
                                file: p,
                            },
                        );
                    }
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

    pub fn read(&self, name: &str) -> JResult<String> {
        let skill = self
            .skills
            .get(name)
            .ok_or_else(|| format!("unknown skill '{name}'; use listed skill names"))?;
        let meta = fs::metadata(&skill.file).map_err(|e| e.to_string())?;
        if meta.len() > 256_000 {
            return Err("skill too large".into());
        }
        let text = fs::read_to_string(&skill.file).map_err(|e| e.to_string())?;
        Ok(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    fn test_config(root: PathBuf) -> Config {
        Config {
            command: "run".into(),
            prompt: None,
            model: "test-model".into(),
            api_base: "https://localhost/v1".into(),
            api_key: None,
            max_steps: 1,
            root,
            allow_write: false,
            allow_exec: false,
            write_paths: vec![],
            exec_programs: vec![],
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
}
