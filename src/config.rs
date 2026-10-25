use std::env;
use std::path::{Path, PathBuf};

pub type JResult<T> = Result<T, String>;

#[derive(Clone)]
pub struct Config {
    pub command: String,
    pub prompt: Option<String>,
    pub model: String,
    pub api_base: String,
    pub api_key: Option<String>,
    pub max_steps: usize,
    pub root: PathBuf,
    pub allow_write: bool,
    pub allow_exec: bool,
    pub write_paths: Vec<PathBuf>,
    pub exec_programs: Vec<String>,
}

fn env_nonempty(name: &str) -> Option<String> {
    env::var(name).ok().filter(|s| !s.trim().is_empty())
}

fn take_value(args: &[String], i: &mut usize, flag: &str) -> JResult<String> {
    *i += 1;
    args.get(*i)
        .cloned()
        .ok_or_else(|| format!("{flag} requires a value"))
}

pub fn help() -> &'static str {
    r#"Jani — Rust-native, skill-aware AI coding agent

USAGE:
  jani --version
  jani --help
  jani doctor [OPTIONS]
  jani skills [OPTIONS]
  jani run "TASK" [OPTIONS]
  jani --print "TASK" [OPTIONS]

Jani has no embedded coordination engine. It works directly in a Git
worktree, reading discovered SKILL.md guidance and honoring only the
Rust-enforced permissions below.

OPTIONS:
  --model MODEL           OpenAI-compatible tool-call-capable model (JANI_MODEL)
  --api-base URL          API base ending in /v1 (JANI_API_BASE)
  --repo PATH             Assigned Git worktree; defaults to current directory
  --max-steps N           Maximum model turns (default 20, ceiling 80)
  --allow-write           Enable gated file editing
  --write-path PATH       Approved relative edit scope; repeat as necessary
  --allow-exec            Enable allowlisted test/git commands
  --exec-program NAME     Additional test runner: pytest/python/node/go/dotnet/npm

ENVIRONMENT:
  JANI_API_KEY or OPENAI_API_KEY    AI provider credential
  JANI_API_BASE                     https://api.openai.com/v1 by default
  JANI_MODEL or OPENAI_MODEL        Model ID (default gpt-5)
  JANI_WRITE_PATHS                  Comma-separated relative edit scope
  JANI_EXEC_PROGRAMS                Optional extra test runners
  JANI_SKILL_DIRS                   Extra skill directories, path-list separated

Skills are advisory instructions, not an executable security policy.
Tool flags are enforced in Rust. Execution is NOT an OS sandbox.
"#
}

impl Config {
    pub fn parse() -> JResult<Self> {
        Self::from_args(&env::args().skip(1).collect::<Vec<_>>())
    }

    pub fn from_args(args: &[String]) -> JResult<Self> {
        let mut command = "help".to_owned();
        let mut prompt: Option<String> = None;
        let mut model = env_nonempty("JANI_MODEL")
            .or_else(|| env_nonempty("OPENAI_MODEL"))
            .unwrap_or_else(|| "gpt-5".to_string());
        let mut api_base = env_nonempty("JANI_API_BASE")
            .unwrap_or_else(|| "https://api.openai.com/v1".to_string());
        let api_key = env_nonempty("JANI_API_KEY").or_else(|| env_nonempty("OPENAI_API_KEY"));
        let mut root = env::current_dir().map_err(|e| e.to_string())?;
        let mut max_steps = 20usize;
        let mut allow_write = false;
        let mut allow_exec = false;
        let mut exec_programs: Vec<String> = env_nonempty("JANI_EXEC_PROGRAMS")
            .map(|s| {
                s.split(',')
                    .map(|x| x.trim().to_string())
                    .filter(|x| !x.is_empty())
                    .collect()
            })
            .unwrap_or_default();
        let mut write_paths: Vec<PathBuf> = env_nonempty("JANI_WRITE_PATHS")
            .map(|s| {
                s.split(',')
                    .map(|x| PathBuf::from(x.trim()))
                    .filter(|x| !x.as_os_str().is_empty())
                    .collect()
            })
            .unwrap_or_default();
        let mut i = 0;
        while i < args.len() {
            let a = args[i].as_str();
            match a {
                "--version" | "version" => command = "version".into(),
                "--help" | "-h" | "help" => command = "help".into(),
                "doctor" => command = "doctor".into(),
                "skills" => command = "skills".into(),
                "run" => {
                    command = "run".into();
                    if let Some(next) = args.get(i + 1) {
                        if !next.starts_with("--") {
                            prompt = Some(take_value(args, &mut i, "run")?);
                        }
                    }
                }
                "--print" => {
                    command = "run".into();
                    let value = take_value(args, &mut i, a)?;
                    if value.starts_with("--") {
                        return Err("--print requires a prompt, not an option".into());
                    }
                    prompt = Some(value);
                }
                "--model" => model = take_value(args, &mut i, a)?,
                "--api-base" => api_base = take_value(args, &mut i, a)?,
                "--repo" => root = PathBuf::from(take_value(args, &mut i, a)?),
                "--max-steps" => {
                    max_steps = take_value(args, &mut i, a)?
                        .parse()
                        .map_err(|_| "--max-steps must be a positive integer".to_string())?;
                    if !(1..=80).contains(&max_steps) {
                        return Err("--max-steps must be 1..80".into());
                    }
                }
                "--allow-write" => allow_write = true,
                "--allow-exec" => allow_exec = true,
                "--write-path" => write_paths.push(PathBuf::from(take_value(args, &mut i, a)?)),
                "--exec-program" => exec_programs.push(take_value(args, &mut i, a)?),
                unknown => return Err(format!("unknown option: {unknown}")),
            }
            i += 1;
        }
        if matches!(command.as_str(), "version" | "help") {
            api_base = api_base.trim_end_matches('/').to_string();
            return Ok(Self {
                command,
                prompt,
                model,
                api_base,
                api_key,
                max_steps,
                root,
                allow_write,
                allow_exec,
                write_paths,
                exec_programs,
            });
        }
        root = root
            .canonicalize()
            .map_err(|e| format!("invalid worktree {}: {e}", root.display()))?;
        if !root.is_dir() {
            return Err("--repo is not a directory".into());
        }
        if command == "run" && prompt.as_ref().is_none_or(|p| p.trim().is_empty()) {
            return Err("run requires a non-empty prompt".into());
        }
        for p in &write_paths {
            validate_relative(p)?;
        }
        write_paths.dedup();
        for program in &exec_programs {
            if !matches!(
                program.as_str(),
                "pytest" | "python" | "python3" | "node" | "go" | "dotnet" | "npm"
            ) {
                return Err(format!("unsupported opt-in test runner {program}"));
            }
        }
        if allow_write && write_paths.is_empty() {
            return Err("--allow-write requires --write-path or JANI_WRITE_PATHS; no implicit full-repository editing".into());
        }
        if !api_base.starts_with("https://")
            && !api_base.starts_with("http://localhost:")
            && !api_base.starts_with("http://127.0.0.1:")
        {
            return Err("API base must be HTTPS or an explicit localhost URL".into());
        }
        if api_base.contains('?') || api_base.contains('#') {
            return Err("API base must not contain query or fragment".into());
        }
        api_base = api_base.trim_end_matches('/').to_string();
        Ok(Self {
            command,
            prompt,
            model,
            api_base,
            api_key,
            max_steps,
            root,
            allow_write,
            allow_exec,
            write_paths,
            exec_programs,
        })
    }
}

pub fn protected_component(component: &str) -> bool {
    let s = component.to_ascii_lowercase();
    matches!(
        s.as_str(),
        ".git"
            | ".agents"
            | ".ssh"
            | ".env"
            | ".netrc"
            | ".npmrc"
            | ".git-credentials"
            | ".aws"
            | ".azure"
            | "id_rsa"
            | "id_dsa"
            | "id_ecdsa"
            | "id_ed25519"
            | "credentials"
            | "credentials.txt"
    ) || s.starts_with(".env.")
        || s.ends_with(".pem")
        || s.ends_with(".key")
        || s.ends_with(".p12")
        || s.ends_with(".pfx")
        || s.ends_with(".keystore")
        || s.ends_with(".jks")
        || s.contains("private_key")
        || s.contains("secret")
}

pub fn validate_relative(p: &Path) -> JResult<()> {
    use std::path::Component;
    if p.as_os_str().is_empty() || p.components().any(|c| !matches!(c, Component::Normal(_))) {
        return Err(format!(
            "relative path with no traversal required: {}",
            p.display()
        ));
    }
    for part in p.components() {
        let s = part.as_os_str().to_string_lossy().to_ascii_lowercase();
        if protected_component(&s) {
            return Err(format!("protected path component: {s}"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn traversal_is_rejected() {
        assert!(validate_relative(Path::new("../secrets")).is_err());
        assert!(validate_relative(Path::new(".git/config")).is_err());
        assert!(validate_relative(Path::new("src/main.rs")).is_ok());
    }
    #[test]
    fn protected_components() {
        assert!(protected_component(".env"));
        assert!(protected_component(".env.production"));
        assert!(protected_component("id_rsa"));
        assert!(protected_component("server.key"));
        assert!(protected_component("bundle.p12"));
        assert!(protected_component(".netrc"));
        assert!(protected_component("my_secret_notes.md"));
        assert!(!protected_component("src"));
        assert!(!protected_component("main.rs"));
        assert!(!protected_component("keyboard.rs"));
    }
}
