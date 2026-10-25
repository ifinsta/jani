use crate::config::{validate_relative, Config, JResult};
use crate::process::{execute, execute_with};
use crate::sandbox::{SandboxManager, SandboxPlan};
use crate::types::{NetworkPolicy, SandboxProfile, SandboxReceipt};
use serde_json::{json, Value};
use std::path::Path;
use std::process::Command;

// Allowlisted execution: command validation and direct execution share the same
// scope and executable rules. Enforcement lives here because a SKILL.md is advisory.
#[cfg(test)]
pub fn run(cfg: &Config, argv: &[String], timeout: u64) -> JResult<Value> {
    if !cfg.allow_exec {
        return Err("command execution disabled; --allow-exec required".into());
    }
    if !(1..=300).contains(&timeout) {
        return Err("command timeout must be 1..300 seconds".into());
    }
    validate(argv, cfg)?;
    if argv.first().map(String::as_str) == Some("git")
        && argv.get(1).map(String::as_str) == Some("commit")
    {
        verify_staged_paths(cfg)?;
    }
    let (program, args) = argv.split_first().ok_or("empty executable")?;
    let mut cmd = Command::new(program);
    cmd.args(args);
    let out = execute(&mut cmd, &cfg.root, timeout)?;
    Ok(json!({
        "exit_code": out.code,
        "timed_out": out.timed_out,
        "stdout": out.stdout,
        "stderr": out.stderr,
        "command_succeeded": out.code == Some(0) && !out.timed_out,
    }))
}

/// Execute an allowlisted command through the selected OS sandbox backend and
/// return both its output and an execution receipt. A profile that requires
/// confinement fails closed when no backend can satisfy it.
pub fn run_confined(
    cfg: &Config,
    argv: &[String],
    timeout: u64,
    profile: SandboxProfile,
    network: NetworkPolicy,
) -> JResult<(Value, SandboxReceipt)> {
    if !cfg.allow_exec {
        return Err("command execution disabled; --allow-exec required".into());
    }
    if !(1..=300).contains(&timeout) {
        return Err("command timeout must be 1..300 seconds".into());
    }
    validate(argv, cfg)?;
    if argv.first().map(String::as_str) == Some("git")
        && argv.get(1).map(String::as_str) == Some("commit")
    {
        verify_staged_paths(cfg)?;
    }
    let manager = SandboxManager::new(cfg);
    let plan: SandboxPlan = manager.prepare(profile, network)?;
    let (mut cmd, isolation) = manager.command(&plan, argv)?;
    let out = execute_with(&mut cmd, &cfg.root, timeout, isolation)?;
    let termination = if out.timed_out {
        Some("timeout-or-output-limit".to_string())
    } else if out.code.is_none() {
        Some("terminated-by-signal".to_string())
    } else {
        None
    };
    let succeeded = out.code == Some(0) && !out.timed_out;
    let network_confined = plan.network_confined;
    let receipt = plan.receipt(termination);
    Ok((
        json!({
            "exit_code": out.code,
            "timed_out": out.timed_out,
            "stdout": out.stdout,
            "stderr": out.stderr,
            "command_succeeded": succeeded,
            "sandbox_backend": receipt.backend,
            "filesystem_confined": receipt.filesystem_confined,
            "network_confined": network_confined,
        }),
        receipt,
    ))
}

// Refuse commits that would include files staged outside the approved edit scope.
// Read-only: runs git directly, not through a model-visible execution path.
pub fn verify_staged_paths(cfg: &Config) -> JResult<()> {
    let mut cmd = Command::new("git");
    cmd.args(["diff", "--cached", "--name-only", "-z"]);
    let out = execute(&mut cmd, &cfg.root, 15)?;
    if out.timed_out || out.code != Some(0) {
        return Err("could not inspect staged paths before commit".into());
    }
    if out.stdout.len() >= crate::process::EXCERPT_LIMIT {
        return Err(
            "staged path list exceeds the verification limit; commit smaller batches".into(),
        );
    }
    for path in out.stdout.split('\0').filter(|s| !s.is_empty()) {
        let p = Path::new(path);
        validate_relative(p)?;
        if !cfg.write_paths.iter().any(|scope| p.starts_with(scope)) {
            return Err(format!("staged file outside approved write scope: {path}"));
        }
    }
    Ok(())
}

fn short_flags(arg: &str) -> Option<&str> {
    if arg.len() > 1 && arg.starts_with('-') && !arg.starts_with("--") {
        Some(&arg[1..])
    } else {
        None
    }
}

fn validate_commit_paths(args: &[String], cfg: &Config) -> JResult<()> {
    const TAKES_VALUE: [&str; 14] = [
        "-m",
        "--message",
        "--author",
        "--date",
        "-F",
        "--file",
        "-c",
        "-C",
        "--fixup",
        "--squash",
        "--trailer",
        "-S",
        "--gpg-sign",
        "--cleanup",
    ];
    let mut expects_value = false;
    let mut after_separator = false;
    for arg in args.iter().skip(2) {
        if expects_value {
            expects_value = false;
            continue;
        }
        if arg == "--" {
            after_separator = true;
            continue;
        }
        if !after_separator && arg.starts_with('-') {
            if TAKES_VALUE.contains(&arg.as_str()) {
                expects_value = true;
            }
            continue;
        }
        let path = Path::new(arg);
        validate_relative(path)?;
        if !cfg.write_paths.iter().any(|scope| path.starts_with(scope)) {
            return Err(format!(
                "git commit path outside approved write scope: {arg}"
            ));
        }
    }
    Ok(())
}

pub fn validate(args: &[String], cfg: &Config) -> JResult<()> {
    let first = args.first().ok_or("empty executable")?.as_str();
    let sub = args.get(1).map(String::as_str).unwrap_or("");
    let allowed = match first {
        "cargo" => matches!(sub, "test" | "check" | "fmt" | "clippy"),
        "git" => matches!(
            sub,
            "status" | "diff" | "log" | "show" | "rev-parse" | "add" | "commit"
        ),
        "rustc" => sub == "--version",
        "pytest" => true,
        "python" | "python3" => sub == "-m" && args.get(2).map(String::as_str) == Some("pytest"),
        "go" => sub == "test",
        "dotnet" => sub == "test",
        "npm" => sub == "test",
        "node" => sub == "--test",
        _ => false,
    };
    if !allowed {
        return Err("unsupported executable or operation".into());
    }
    if !matches!(first, "git" | "cargo" | "rustc") && !cfg.exec_programs.iter().any(|x| x == first)
    {
        return Err(format!(
            "{first} is not an approved --exec-program test runner"
        ));
    }
    if first == "cargo" && sub == "fmt" && !args.iter().any(|a| a == "--check") {
        return Err(
            "cargo fmt requires --check; do not allow uncontrolled formatting edits".into(),
        );
    }
    if first == "cargo"
        && sub == "clippy"
        && args.iter().any(|a| a == "--fix" || a.starts_with("--fix="))
    {
        return Err("cargo clippy --fix would edit files outside the approved write scope".into());
    }
    if first == "git"
        && args
            .iter()
            .any(|a| a == "--no-index" || a.starts_with("--no-index="))
    {
        return Err("git --no-index can read files outside the worktree".into());
    }
    if first == "git" && sub == "commit" {
        if args.iter().any(|a| {
            matches!(
                a.as_str(),
                "-a" | "--all"
                    | "--amend"
                    | "--no-verify"
                    | "--allow-empty"
                    | "--file"
                    | "--pathspec-from-file"
            )
        }) {
            return Err("unsupported git commit flag".into());
        }
        if args
            .iter()
            .any(|a| a.starts_with("--file=") || a.starts_with("--pathspec-from-file="))
        {
            return Err("unsupported git commit flag".into());
        }
        // Combined short flags such as -am, -n or -Ffile must not slip past the checks above.
        if let Some(cluster) = args
            .iter()
            .filter_map(|a| short_flags(a))
            .find(|flags| flags.contains('a') || flags.contains('n') || flags.contains('F'))
        {
            return Err(format!("unsupported git commit flag cluster: -{cluster}"));
        }
    }
    if first == "git"
        && matches!(sub, "status" | "diff" | "log" | "show" | "rev-parse")
        && args
            .iter()
            .any(|a| a.starts_with("--output") || a.starts_with("--ext-diff"))
    {
        return Err("git read command cannot write output files or run external diff tools".into());
    }
    if args.iter().any(|a| {
        a.contains('\0')
            || a.starts_with("--git-dir")
            || a.starts_with("--work-tree")
            || a == "-C"
            || a == "-c"
            || a == "--exec-path"
            || a == "--manifest-path"
            || a == "--config"
            || a == "--target-dir"
            || a == "--external"
            || a == "--message-file"
            || a == "-F"
    }) {
        return Err("unsafe executable override".into());
    }
    if first == "git" && sub == "add" {
        if !cfg.allow_write {
            return Err("git add requires --allow-write".into());
        }
        for a in args.iter().skip(2) {
            if a == "--" {
                continue;
            }
            if let Some(flags) = short_flags(a) {
                if flags.contains('A') || flags.contains('u') {
                    return Err(
                        "git add requires explicit approved file paths; -A/-u are not allowed"
                            .into(),
                    );
                }
            }
            if a == "--all" || a == "." || a == "--update" {
                return Err("git add requires explicit approved file paths".into());
            }
            let p = Path::new(a);
            validate_relative(p)?;
            if !cfg.write_paths.iter().any(|scope| p.starts_with(scope)) {
                return Err("git add outside approved scope".into());
            }
            let full = cfg.root.join(p);
            if !full.is_file()
                || std::fs::symlink_metadata(&full)
                    .map_err(|e| e.to_string())?
                    .file_type()
                    .is_symlink()
            {
                return Err(
                    "git add requires explicit regular files, not directory or symlink paths"
                        .into(),
                );
            }
        }
    }
    if first == "git" && sub == "commit" {
        validate_commit_paths(args, cfg)?;
        if !cfg.allow_write {
            return Err("git commit requires --allow-write".into());
        }
    }
    if first == "git" && matches!(sub, "add" | "commit") && !cfg.allow_exec {
        return Err("git write operations require --allow-exec".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn test_config() -> Config {
        Config {
            command: "run".into(),
            allow_write: true,
            allow_exec: true,
            write_paths: vec![PathBuf::from("src")],
            exec_programs: vec![],
            ..Config::baseline()
        }
    }

    fn cmd(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn writes_outside_worktree_are_refused() {
        let cfg = test_config();
        assert!(validate(&cmd(&["git", "diff", "--no-index", "a", "b"]), &cfg).is_err());
        assert!(validate(&cmd(&["git", "diff", "--no-index=a", "b"]), &cfg).is_err());
    }

    #[test]
    fn editing_cargo_commands_are_refused() {
        let cfg = test_config();
        assert!(validate(&cmd(&["cargo", "clippy", "--fix"]), &cfg).is_err());
        assert!(validate(&cmd(&["cargo", "clippy", "--fix=all"]), &cfg).is_err());
        assert!(validate(&cmd(&["cargo", "fmt", "--", "--check"]), &cfg).is_ok());
    }

    #[test]
    fn combined_git_flags_are_refused() {
        let cfg = test_config();
        assert!(validate(&cmd(&["git", "commit", "-am", "msg"]), &cfg).is_err());
        assert!(validate(&cmd(&["git", "commit", "-n", "-m", "msg"]), &cfg).is_err());
        assert!(validate(&cmd(&["git", "add", "-A"]), &cfg).is_err());
        assert!(validate(&cmd(&["git", "add", "-u"]), &cfg).is_err());
        assert!(validate(&cmd(&["git", "commit", "--pathspec-from-file=paths"]), &cfg).is_err());
    }

    #[test]
    fn commit_pathspecs_must_stay_in_scope() {
        let cfg = test_config();
        assert!(validate(&cmd(&["git", "commit", "-m", "msg", "src/main.rs"]), &cfg).is_ok());
        assert!(validate(&cmd(&["git", "commit", "-m", "msg", "tests/cli.rs"]), &cfg).is_err());
        assert!(validate(&cmd(&["git", "commit", "-m", "msg", "../escape"]), &cfg).is_err());
        assert!(validate(
            &cmd(&["git", "commit", "-m", "msg", "--", "src/main.rs"]),
            &cfg
        )
        .is_ok());
    }

    #[test]
    fn core_run_executes_allowlisted_command() {
        let mut cfg = test_config();
        cfg.root = std::env::temp_dir();
        let v = run(&cfg, &cmd(&["rustc", "--version"]), 30).unwrap();
        assert_eq!(v["command_succeeded"], true);
        assert!(v["stdout"].as_str().unwrap_or("").contains("rustc"));
    }
}
