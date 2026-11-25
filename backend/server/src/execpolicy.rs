//! Smart command execution: decide what the model may run without asking.
//!
//! The older `--allow-exec` list is fixed: a handful of test runners and git
//! commands. That is safe and nearly useless, because real engineering needs
//! `grep`, a build, a script. `--exec smart` replaces the list with a
//! classification whose trust comes from *containment*, not from a name:
//!
//! 1. **Hard deny.** Privilege escalation, disk and system tools, deleting
//!    outside the worktree, force-pushing, publishing. Nothing overrides these.
//! 2. **Operator rules.** Prefix rules in `<state>/exec-rules.json`, written only
//!    by the operator (`jani exec allow|deny`, or "always" in the UI). The model
//!    and the settings files cannot write or widen them. A deny rule beats an
//!    allow rule.
//! 3. **The fixed list** that `--allow-exec` always had.
//! 4. **Safe inspection** (`ls`, `grep`, `rg`, `find`, `git status`...) whose
//!    paths stay inside the worktree runs on its own.
//! 5. **Builds and tests** (`cargo build`, `npm run build`, `make`, `pytest`...)
//!    run on their own *only when a sandbox confines them*: the worktree is
//!    read-only except the operator's write scope, and there is no network. With
//!    no such sandbox they ask first, because running project code unconfined is
//!    exactly as safe as the project.
//! 6. **Everything else asks** the operator once, for the session, or always.
//!
//! Commands are argv, never a shell string, so there is no `a && b`, no
//! redirection and no expansion to reason about. Programs named by a path
//! (`./ls`) are never treated as the tools they imitate.

use crate::config::{Config, JResult};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Basis {
    Allowlist,
    Safe,
    Build,
    Rule,
    Session,
    OperatorOnce,
    OperatorSession,
    OperatorAlways,
}

impl Basis {
    /// Journal reason code, so an audit shows *why* a command ran.
    pub fn code(self) -> &'static str {
        match self {
            Self::Allowlist => "exec_allowlist",
            Self::Safe => "exec_safe_inspection",
            Self::Build => "exec_confined_build",
            Self::Rule => "exec_operator_rule",
            Self::Session => "exec_session_grant",
            Self::OperatorOnce => "exec_operator_once",
            Self::OperatorSession => "exec_operator_session",
            Self::OperatorAlways => "exec_operator_always",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    Allow(Basis),
    /// Needs the operator. `suggest` is the prefix a "remember" would store.
    Ask {
        reason: String,
        suggest: Vec<String>,
    },
    Deny {
        reason: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rule {
    pub allow: bool,
    pub argv: Vec<String>,
    pub added: String,
    pub by: String,
}

pub struct Context<'a> {
    pub cfg: &'a Config,
    /// A sandbox confines both the filesystem (read-only apart from the write
    /// scope) and the network for this call.
    pub confined: bool,
    /// The sandbox cuts the network, so network tools cannot work.
    pub network_cut: bool,
    pub rules: &'a [Rule],
    pub session: &'a [Vec<String>],
}

// ---------------------------------------------------------------- rules store

fn rules_path(state_dir: &Path) -> PathBuf {
    state_dir.join("exec-rules.json")
}

pub fn load_rules(state_dir: &Path) -> JResult<Vec<Rule>> {
    let path = rules_path(state_dir);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(format!("cannot read {}: {error}", path.display())),
    };
    let value: serde_json::Value = serde_json::from_str(&text)
        .map_err(|e| format!("{} is not valid JSON: {e}", path.display()))?;
    if value["version"] != 1 {
        return Err(format!("{}: unsupported version", path.display()));
    }
    let mut rules = Vec::new();
    for entry in value["rules"]
        .as_array()
        .ok_or("exec rules: `rules` must be an array")?
    {
        let effect = entry["effect"].as_str().unwrap_or("");
        let argv: Vec<String> = entry["argv"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        if !matches!(effect, "allow" | "deny") || argv.is_empty() {
            return Err(format!("{}: malformed rule {entry}", path.display()));
        }
        rules.push(Rule {
            allow: effect == "allow",
            argv,
            added: entry["added"].as_str().unwrap_or("").to_string(),
            by: entry["by"].as_str().unwrap_or("").to_string(),
        });
    }
    Ok(rules)
}

fn save_rules(state_dir: &Path, rules: &[Rule]) -> JResult<()> {
    std::fs::create_dir_all(state_dir).map_err(|e| e.to_string())?;
    let value = serde_json::json!({
        "version": 1,
        "rules": rules.iter().map(|r| serde_json::json!({
            "effect": if r.allow { "allow" } else { "deny" },
            "argv": r.argv,
            "added": r.added,
            "by": r.by,
        })).collect::<Vec<_>>(),
    });
    let path = rules_path(state_dir);
    let tmp = path.with_extension("json.tmp");
    std::fs::write(
        &tmp,
        serde_json::to_vec_pretty(&value).map_err(|e| e.to_string())?,
    )
    .map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, &path).map_err(|e| format!("cannot replace {}: {e}", path.display()))
}

/// Add a rule. An allow rule for a hard-denied program is refused: those can
/// never be granted by anyone from inside the tool.
pub fn add_rule(state_dir: &Path, allow: bool, argv: Vec<String>, by: &str) -> JResult<Rule> {
    if argv.is_empty() || argv.iter().any(|a| a.is_empty() || a.contains('\0')) {
        return Err("a rule needs a non-empty command prefix".into());
    }
    if allow {
        if let Some(reason) = hard_deny(&argv) {
            return Err(format!("cannot be allowed by a rule: {reason}"));
        }
    }
    let mut rules = load_rules(state_dir)?;
    let rule = Rule {
        allow,
        argv,
        added: crate::types::now_unix().to_string(),
        by: by.to_string(),
    };
    if !rules
        .iter()
        .any(|r| r.allow == rule.allow && r.argv == rule.argv)
    {
        rules.push(rule.clone());
        save_rules(state_dir, &rules)?;
    }
    Ok(rule)
}

pub fn remove_rule(state_dir: &Path, index: usize) -> JResult<Rule> {
    let mut rules = load_rules(state_dir)?;
    if index == 0 || index > rules.len() {
        return Err(format!("no rule {index}; there are {}", rules.len()));
    }
    let removed = rules.remove(index - 1);
    save_rules(state_dir, &rules)?;
    Ok(removed)
}

fn prefix_matches(prefix: &[String], argv: &[String]) -> bool {
    !prefix.is_empty() && argv.len() >= prefix.len() && argv[..prefix.len()] == *prefix
}

// ------------------------------------------------------------- classification

/// The program's own name: no directory, no extension, lower case.
fn base_name(program: &str) -> String {
    let name = program.rsplit(['/', '\\']).next().unwrap_or(program);
    let lower = name.to_ascii_lowercase();
    for ext in [".exe", ".cmd", ".bat", ".com"] {
        if let Some(stem) = lower.strip_suffix(ext) {
            return stem.to_string();
        }
    }
    lower
}

fn is_bare(program: &str) -> bool {
    !program.contains('/') && !program.contains('\\') && !program.is_empty()
}

const HARD_DENIED_PROGRAMS: &[&str] = &[
    "sudo",
    "su",
    "doas",
    "runas",
    "pkexec",
    "mkfs",
    "fdisk",
    "parted",
    "diskpart",
    "format",
    "dd",
    "shutdown",
    "reboot",
    "halt",
    "poweroff",
    "init",
    "systemctl",
    "service",
    "launchctl",
    "reg",
    "regedit",
    "schtasks",
    "sc",
    "net",
    "netsh",
    "takeown",
    "icacls",
    "cacls",
    "iptables",
    "nft",
    "ufw",
    "mount",
    "umount",
    "losetup",
    "insmod",
    "modprobe",
    "crontab",
    "passwd",
    "useradd",
    "userdel",
    "usermod",
    "groupadd",
    "chpasswd",
    "visudo",
    "setcap",
];

/// Things nobody may allow from inside the tool.
pub fn hard_deny(argv: &[String]) -> Option<String> {
    let program = base_name(argv.first()?);
    if HARD_DENIED_PROGRAMS.contains(&program.as_str()) || program.starts_with("mkfs.") {
        return Some(format!("{program} changes the machine, not the project"));
    }
    let sub = argv.get(1).map(String::as_str).unwrap_or("");
    let has = |flags: &[&str]| argv.iter().skip(1).any(|a| flags.contains(&a.as_str()));
    match program.as_str() {
        "git"
            if sub == "push"
                && has(&[
                    "-f",
                    "--force",
                    "--mirror",
                    "--delete",
                    "--force-with-lease",
                ]) =>
        {
            return Some("force-pushing, mirroring or deleting remote refs is never allowed".into())
        }
        "cargo" if matches!(sub, "publish" | "login" | "yank" | "owner") => {
            return Some(format!("cargo {sub} acts on a public registry"))
        }
        "npm" | "pnpm" | "yarn" | "bun"
            if matches!(
                sub,
                "publish" | "login" | "adduser" | "token" | "unpublish" | "deprecate" | "owner"
            ) =>
        {
            return Some(format!("{program} {sub} acts on a public registry"))
        }
        "rm" | "rmdir" | "del" | "rd" | "erase"
            if argv
                .iter()
                .skip(1)
                .filter(|a| !a.starts_with('-'))
                .any(|a| escapes(a)) =>
        {
            // Deleting anything that leaves the worktree.
            return Some("deleting outside the worktree is never allowed".into());
        }
        _ => {}
    }
    None
}

/// True when the path argument could name something outside the worktree.
fn escapes(arg: &str) -> bool {
    let value = if arg.starts_with("--") {
        match arg.split_once('=') {
            Some((_, value)) => value,
            None => return false,
        }
    } else if arg.starts_with('-') && arg.len() > 2 {
        match arg.split_once('=') {
            Some((_, value)) => value,
            None => arg[2..].trim_start_matches('='),
        }
    } else {
        arg
    };
    if value.is_empty() || value == "-" {
        return false;
    }
    let bytes = value.as_bytes();
    value.starts_with('/')
        || value.starts_with('\\')
        || value.starts_with('~')
        || value.contains('\0')
        || (bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':')
        || value.split(['/', '\\']).any(|part| part == "..")
}

const SAFE_PLAIN: &[&str] = &[
    "ls",
    "dir",
    "cat",
    "head",
    "tail",
    "wc",
    "diff",
    "stat",
    "du",
    "tree",
    "nl",
    "tac",
    "which",
    "where",
    "pwd",
    "date",
    "uname",
    "whoami",
    "hostname",
    "basename",
    "dirname",
    "echo",
    "true",
    "false",
    "sort",
    "file",
    "findstr",
    "cmp",
    "md5sum",
    "sha256sum",
    "sha1sum",
    "cksum",
];
/// Programs whose first non-flag argument is a pattern, not a path.
const PATTERN_FIRST: &[&str] = &[
    "grep", "egrep", "fgrep", "rg", "ag", "fd", "fdfind", "findstr",
];
const FIND_PATTERN_ARGS: &[&str] = &[
    "-name",
    "-iname",
    "-path",
    "-ipath",
    "-wholename",
    "-iwholename",
    "-regex",
    "-iregex",
    "-lname",
    "-ilname",
];
const GIT_READ: &[&str] = &[
    "status",
    "diff",
    "log",
    "show",
    "rev-parse",
    "ls-files",
    "ls-tree",
    "blame",
    "describe",
    "shortlog",
    "grep",
    "cat-file",
    "rev-list",
    "name-rev",
    "merge-base",
    "whatchanged",
    "diff-tree",
    "diff-files",
    "diff-index",
    "show-ref",
    "count-objects",
    "check-ignore",
];
const GIT_ASK: &[&str] = &[
    "push",
    "pull",
    "fetch",
    "clone",
    "checkout",
    "switch",
    "restore",
    "reset",
    "clean",
    "rebase",
    "merge",
    "cherry-pick",
    "stash",
    "revert",
    "init",
    "submodule",
    "worktree",
    "remote",
    "config",
    "tag",
    "branch",
    "apply",
    "am",
    "rm",
    "mv",
    "gc",
    "prune",
    "reflog",
    "bisect",
    "notes",
    "lfs",
    "filter-branch",
    "update-ref",
    "symbolic-ref",
];
const SHELLS_AND_WRAPPERS: &[&str] = &[
    "sh",
    "bash",
    "zsh",
    "dash",
    "fish",
    "ksh",
    "csh",
    "tcsh",
    "cmd",
    "powershell",
    "pwsh",
    "wsl",
    "env",
    "xargs",
    "nohup",
    "timeout",
    "nice",
    "time",
    "command",
    "eval",
    "exec",
    "busybox",
    "start",
    "call",
    "script",
    "setsid",
    "stdbuf",
    "chroot",
    "unshare",
    "nsenter",
];
const MUTATORS: &[&str] = &[
    "rm", "rmdir", "mv", "cp", "mkdir", "touch", "ln", "chmod", "chown", "tee", "truncate",
    "install", "del", "rd", "erase", "move", "copy", "ren", "rename", "xcopy", "robocopy", "patch",
    "sed", "awk", "gawk", "perl", "ruby", "php", "lua", "printenv", "set", "export",
];
const PACKAGE_MANAGERS: &[&str] = &[
    "npm", "pnpm", "yarn", "bun", "pip", "pip3", "pipx", "uv", "poetry", "cargo", "go", "gem",
    "bundle", "composer", "apt", "apt-get", "brew", "dotnet", "nuget", "conda",
];
const NETWORK_PROGRAMS: &[&str] = &[
    "curl",
    "wget",
    "http",
    "https",
    "ssh",
    "scp",
    "sftp",
    "rsync",
    "nc",
    "ncat",
    "netcat",
    "telnet",
    "ftp",
    "ping",
    "nslookup",
    "dig",
    "traceroute",
    "gh",
    "aws",
    "gcloud",
    "az",
    "kubectl",
    "docker",
    "podman",
    "terraform",
    "helm",
    "ansible",
];

/// Programs and subcommands that build or test a project, and that are safe to
/// run unattended *inside a confining sandbox*. Returns the directories (relative
/// to the worktree) they write build output to.
fn build_tool(argv: &[String]) -> Option<&'static [&'static str]> {
    let program = base_name(&argv[0]);
    let sub = argv.get(1).map(String::as_str).unwrap_or("");
    let arg2 = argv.get(2).map(String::as_str).unwrap_or("");
    match program.as_str() {
        "cargo" => matches!(
            sub,
            "build"
                | "b"
                | "check"
                | "c"
                | "test"
                | "t"
                | "clippy"
                | "doc"
                | "d"
                | "bench"
                | "run"
                | "r"
                | "tree"
                | "metadata"
                | "fmt"
                | "nextest"
                | "llvm-cov"
                | "expand"
        )
        .then_some(&["target"][..]),
        "rustc" => (sub == "--version" || sub == "-V").then_some(&[][..]),
        "go" => matches!(
            sub,
            "build" | "test" | "vet" | "run" | "list" | "fmt" | "env" | "version"
        )
        .then_some(&[][..]),
        "npm" | "pnpm" | "yarn" | "bun" => {
            let runs = matches!(sub, "run" | "run-script" | "rum" | "urn")
                || (matches!(program.as_str(), "yarn" | "pnpm" | "bun")
                    && !matches!(
                        sub,
                        "install"
                            | "i"
                            | "add"
                            | "remove"
                            | "up"
                            | "upgrade"
                            | "dlx"
                            | "x"
                            | "exec"
                            | "create"
                            | "init"
                            | "link"
                            | "unlink"
                            | "update"
                            | "outdated"
                            | "audit"
                            | "info"
                            | "global"
                            | "set"
                            | "config"
                            | "cache"
                            | "publish"
                            | "login"
                            | "logout"
                    )
                    && !sub.starts_with('-')
                    && !sub.is_empty());
            let tests = matches!(sub, "test" | "t" | "tst" | "start" | "lint");
            (runs || tests).then_some(
                &[
                    "node_modules/.cache",
                    "dist",
                    "build",
                    "coverage",
                    ".next",
                    ".turbo",
                ][..],
            )
        }
        "python" | "python3" | "py" => (sub == "-m"
            && matches!(
                arg2,
                "pytest"
                    | "unittest"
                    | "mypy"
                    | "ruff"
                    | "black"
                    | "isort"
                    | "flake8"
                    | "pylint"
                    | "pyflakes"
                    | "compileall"
                    | "doctest"
                    | "coverage"
            ))
        .then_some(
            &[
                ".pytest_cache",
                ".mypy_cache",
                ".ruff_cache",
                ".coverage_data",
            ][..],
        ),
        "pytest" | "py.test" | "mypy" | "ruff" | "black" | "isort" | "flake8" | "pylint"
        | "tox" | "nox" => Some(
            &[
                ".pytest_cache",
                ".mypy_cache",
                ".ruff_cache",
                ".tox",
                ".nox",
            ][..],
        ),
        "node" => (sub == "--test").then_some(&["coverage"][..]),
        "tsc" | "eslint" | "prettier" | "jest" | "vitest" | "mocha" | "playwright" => Some(
            &[
                "node_modules/.cache",
                "dist",
                "build",
                "coverage",
                "test-results",
            ][..],
        ),
        "make" | "gmake" | "ninja" | "just" | "ctest" => {
            Some(&["build", "target", "out", "dist"][..])
        }
        "cmake" => (sub == "--build" || sub == "-S" || sub == "-B" || sub == "--version")
            .then_some(&["build"][..]),
        "mvn" | "mvnw" => matches!(
            sub,
            "compile" | "test" | "verify" | "package" | "test-compile" | "-v" | "--version"
        )
        .then_some(&["target"][..]),
        "gradle" | "gradlew" => matches!(
            sub,
            "build" | "test" | "check" | "assemble" | "classes" | "--version"
        )
        .then_some(&["build", ".gradle"][..]),
        "dotnet" => matches!(sub, "build" | "test" | "--version" | "--info")
            .then_some(&["bin", "obj", "TestResults"][..]),
        "gcc" | "g++" | "cc" | "c++" | "clang" | "clang++" => Some(&["build", "out"][..]),
        "bazel" => {
            matches!(sub, "build" | "test" | "query" | "version").then_some(&["bazel-out"][..])
        }
        _ => None,
    }
}

/// Directories the command may write build output to, in addition to the
/// operator's write scope. Only meaningful when a sandbox confines it.
///
/// A directory is created for the tool only when the tool creates it itself and
/// an empty one is harmless (cargo's `target`). For the rest (`make`, npm, ...)
/// only a directory that already exists is opened up: creating `build/` or `out/`
/// would make a `make build` target look "up to date". Those projects create
/// their output directory once (or the operator adds `--write-path`).
pub fn build_outputs(argv: &[String], root: &Path) -> Vec<PathBuf> {
    let program = base_name(argv.first().map(String::as_str).unwrap_or(""));
    let creates = matches!(
        program.as_str(),
        "cargo" | "mvn" | "mvnw" | "gradle" | "gradlew" | "dotnet" | "bazel"
    );
    build_tool(argv)
        .map(|dirs| {
            dirs.iter()
                .filter(|dir| creates || root.join(dir).is_dir())
                .map(PathBuf::from)
                .collect()
        })
        .unwrap_or_default()
}

fn flag_value<'a>(argv: &'a [String], names: &[&str]) -> Option<&'a str> {
    argv.iter().enumerate().find_map(|(i, a)| {
        names
            .iter()
            .any(|n| a == n)
            .then(|| argv.get(i + 1).map(String::as_str).unwrap_or(""))
    })
}

fn ask(reason: impl Into<String>, argv: &[String]) -> Verdict {
    Verdict::Ask {
        reason: reason.into(),
        suggest: suggest_prefix(argv),
    }
}

/// The prefix an "always allow" would remember: narrow enough to mean something.
pub fn suggest_prefix(argv: &[String]) -> Vec<String> {
    let program = base_name(argv.first().map(String::as_str).unwrap_or(""));
    let take = |n: usize| argv.iter().take(n).cloned().collect::<Vec<_>>();
    if SHELLS_AND_WRAPPERS.contains(&program.as_str()) || MUTATORS.contains(&program.as_str()) {
        return argv.to_vec();
    }
    match program.as_str() {
        "npm" | "pnpm" | "yarn" | "bun"
            if matches!(argv.get(1).map(String::as_str), Some("run" | "run-script")) =>
        {
            take(3)
        }
        "python" | "python3" | "py" if argv.get(1).map(String::as_str) == Some("-m") => take(3),
        "cargo" | "go" | "git" | "npm" | "pnpm" | "yarn" | "bun" | "dotnet" | "docker"
        | "kubectl" | "make" | "just" | "gradle" | "mvn" | "pip" | "pip3" => take(2),
        _ => match argv.get(1) {
            Some(second) if !second.starts_with('-') => take(2),
            _ => take(1),
        },
    }
}

fn check_paths(argv: &[String], pattern_first: bool, exempt_after: &[&str]) -> Option<String> {
    let mut skip_next = false;
    let mut pattern_pending =
        pattern_first && flag_value(argv, &["-e", "--regexp", "-f", "--file"]).is_none();
    let mut after_dashes = false;
    for arg in argv.iter().skip(1) {
        if skip_next {
            skip_next = false;
            continue;
        }
        if !after_dashes && arg == "--" {
            after_dashes = true;
            continue;
        }
        if !after_dashes && arg.starts_with('-') && arg.len() > 1 {
            if exempt_after.contains(&arg.as_str()) || matches!(arg.as_str(), "-e" | "--regexp") {
                skip_next = true;
                continue;
            }
            if escapes(arg) {
                return Some(format!("{arg} points outside the worktree"));
            }
            continue;
        }
        if pattern_pending {
            pattern_pending = false;
            continue;
        }
        if escapes(arg) {
            return Some(format!("`{arg}` points outside the worktree"));
        }
    }
    None
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum InspectionArg {
    Boolean,
    Pattern,
    Numeric,
    Path,
}

/// Deliberately incomplete grammars. An option is not safe just because its
/// program is read-only by default. Optional/attached values are not inferred.
fn inspection_option(program: &str, flag: &str) -> Option<InspectionArg> {
    use InspectionArg::*;
    let (short, long): (&str, &[&str]) = match program {
        "ls" | "dir" => (
            "laAhdFipQrRStUuX1",
            &[
                "--all",
                "--almost-all",
                "--directory",
                "--human-readable",
                "--recursive",
            ],
        ),
        "cat" => (
            "AbEnstTv",
            &[
                "--show-all",
                "--number",
                "--number-nonblank",
                "--squeeze-blank",
            ],
        ),
        "head" => ("qv", &["--quiet", "--silent", "--verbose"]),
        "tail" => ("qv", &["--quiet", "--silent", "--verbose"]),
        "wc" => (
            "clmwL",
            &[
                "--bytes",
                "--chars",
                "--lines",
                "--words",
                "--max-line-length",
            ],
        ),
        "grep" | "egrep" | "fgrep" | "git grep" => (
            "rnivwxEFGclLhHqsaIZzob",
            &[
                "--recursive",
                "--line-number",
                "--ignore-case",
                "--invert-match",
                "--word-regexp",
                "--line-regexp",
                "--extended-regexp",
                "--fixed-strings",
                "--count",
                "--files-with-matches",
                "--files-without-match",
                "--no-messages",
                "--text",
                "--null",
            ],
        ),
        "rg" => (
            "nivwxFclLqsaIUobH",
            &[
                "--line-number",
                "--ignore-case",
                "--case-sensitive",
                "--invert-match",
                "--word-regexp",
                "--line-regexp",
                "--fixed-strings",
                "--count",
                "--files-with-matches",
                "--files-without-match",
                "--quiet",
                "--text",
                "--hidden",
                "--no-ignore",
                "--no-config",
                "--multiline",
                "--null",
            ],
        ),
        "ag" => (
            "nivwFclLsQaH",
            &[
                "--line-numbers",
                "--ignore-case",
                "--literal",
                "--hidden",
                "--noignore",
                "--silent",
            ],
        ),
        "fd" | "fdfind" => (
            "isHIla0q",
            &[
                "--ignore-case",
                "--case-sensitive",
                "--hidden",
                "--no-ignore",
                "--list-details",
                "--absolute-path",
                "--print0",
                "--quiet",
            ],
        ),
        "diff" => (
            "abBiqrsuwy",
            &[
                "--text",
                "--ignore-space-change",
                "--ignore-all-space",
                "--ignore-blank-lines",
                "--brief",
                "--recursive",
                "--report-identical-files",
                "--unified",
            ],
        ),
        "stat" => ("", &["--terse"]),
        "du" => (
            "ahskcm",
            &["--all", "--human-readable", "--summarize", "--total"],
        ),
        "tree" => ("adfiqpshF", &["--dirsfirst", "--noreport"]),
        "nl" => ("", &[]),
        "tac" => ("br", &["--before", "--regex"]),
        "sort" => (
            "bdfghinMruVz",
            &[
                "--ignore-leading-blanks",
                "--dictionary-order",
                "--ignore-case",
                "--general-numeric-sort",
                "--human-numeric-sort",
                "--numeric-sort",
                "--reverse",
                "--unique",
                "--zero-terminated",
            ],
        ),
        "file" => (
            "bikLNrs0",
            &[
                "--brief",
                "--mime",
                "--mime-type",
                "--mime-encoding",
                "--no-pad",
                "--raw",
            ],
        ),
        "cmp" => ("ls", &["--verbose", "--silent", "--quiet"]),
        "md5sum" | "sha256sum" | "sha1sum" => ("bt", &["--binary", "--text", "--tag"]),
        "cksum" => ("", &[]),
        "date" => ("uR", &["--utc", "--universal", "--rfc-email"]),
        "uname" => (
            "asnrvmpio",
            &[
                "--all",
                "--kernel-name",
                "--nodename",
                "--kernel-release",
                "--kernel-version",
                "--machine",
                "--processor",
                "--hardware-platform",
                "--operating-system",
            ],
        ),
        "echo" => ("neE", &[]),
        "basename" => ("az", &["--multiple", "--zero"]),
        "dirname" => ("z", &["--zero"]),
        "where" => ("", &["/Q", "/F", "/T"]),
        "which" => ("a", &["--all"]),
        "git status" => (
            "sbz",
            &["--short", "--branch", "--porcelain", "--show-stash"],
        ),
        "git diff" => (
            "",
            &[
                "--stat",
                "--numstat",
                "--shortstat",
                "--name-only",
                "--name-status",
                "--summary",
                "--no-ext-diff",
                "--no-textconv",
            ],
        ),
        "git log" => (
            "",
            &[
                "--oneline",
                "--no-decorate",
                "--all",
                "--no-merges",
                "--reverse",
            ],
        ),
        "git branch" => (
            "arv",
            &[
                "--all",
                "--remotes",
                "--verbose",
                "--list",
                "--show-current",
            ],
        ),
        "git tag" => ("l", &["--list"]),
        "git remote" => ("v", &["--verbose"]),
        "git stash" => ("", &["--oneline"]),
        "git ls-files" => ("z", &["--cached", "--stage"]),
        _ => ("", &[]),
    };
    if long.contains(&flag)
        || (flag.starts_with('-') && flag.len() == 2 && short.contains(&flag[1..]))
    {
        return Some(Boolean);
    }
    match (program, flag) {
        ("grep" | "egrep" | "fgrep" | "rg" | "git grep", "-e" | "--regexp") => Some(Pattern),
        ("grep" | "egrep" | "fgrep" | "rg" | "git grep", "-f" | "--file") => Some(Path),
        (
            "grep" | "egrep" | "fgrep" | "rg",
            "-A" | "-B" | "-C" | "-m" | "--after-context" | "--before-context" | "--context"
            | "--max-count",
        ) => Some(Numeric),
        ("grep" | "egrep" | "fgrep", "--include" | "--exclude" | "--exclude-dir") => Some(Pattern),
        ("grep" | "egrep" | "fgrep", "--exclude-from") => Some(Path),
        ("rg", "-g" | "--glob" | "--iglob" | "-t" | "--type" | "-T" | "--type-not") => {
            Some(Pattern)
        }
        ("rg", "--max-depth" | "--threads") => Some(Numeric),
        ("fd" | "fdfind", "-e" | "--extension" | "-t" | "--type" | "-E" | "--exclude") => {
            Some(Pattern)
        }
        ("fd" | "fdfind", "-d" | "--max-depth" | "--min-depth") => Some(Numeric),
        ("fd" | "fdfind", "--search-path") => Some(Path),
        ("head" | "tail", "-n" | "--lines" | "-c" | "--bytes") => Some(Numeric),
        ("ls" | "dir", "-I" | "--ignore") => Some(Pattern),
        ("ls" | "dir", "-w" | "--width" | "--tabsize") => Some(Numeric),
        ("tree", "-L") | ("du", "-d" | "--max-depth") => Some(Numeric),
        ("tree", "-P" | "-I") | ("diff", "-I" | "--ignore-matching-lines" | "-x" | "--exclude") => {
            Some(Pattern)
        }
        ("diff", "-X" | "--exclude-from") => Some(Path),
        ("diff", "-U" | "--unified") => Some(Numeric),
        ("stat", "-c" | "--format" | "--printf") => Some(Pattern),
        ("nl", "-s" | "--number-separator")
        | ("tac", "-s" | "--separator")
        | ("sort", "-t" | "--field-separator") => Some(Pattern),
        (
            "nl",
            "-w" | "--number-width" | "-v" | "--starting-line-number" | "-i" | "--line-increment",
        ) => Some(Numeric),
        ("file", "-f" | "--files-from") => Some(Path),
        ("date", "-d" | "--date") | ("basename", "-s" | "--suffix") => Some(Pattern),
        ("date", "-f" | "--file" | "-r" | "--reference") | ("where", "/R") => Some(Path),
        ("git log", "-n" | "--max-count") => Some(Numeric),
        ("find", flag) if FIND_PATTERN_ARGS.contains(&flag) => Some(Pattern),
        ("find", "-type") => Some(Pattern),
        ("find", "-maxdepth" | "-mindepth") => Some(Numeric),
        ("find", "-print" | "-print0" | "-empty" | "-depth" | "-not" | "-and" | "-or") => {
            Some(Boolean)
        }
        _ => None,
    }
}

// A prior Ask (unknown grammar, missing input, budget) must never hide a known
// denial later in the same argv or traversal.
fn inspection_failure(failure: &mut Option<Verdict>, verdict: Verdict) {
    if failure.is_none() || matches!(verdict, Verdict::Deny { .. }) {
        *failure = Some(verdict);
    }
}

fn inspection_path(name: &str, cfg: &Config, argv: &[String]) -> Result<PathBuf, Verdict> {
    let normalized = name.replace('\\', "/");
    if normalized.split('/').enumerate().any(|(index, part)| {
        let drive = index == 0
            && part.len() == 2
            && part.as_bytes()[0].is_ascii_alphabetic()
            && part.ends_with(':');
        !drive && (part.contains(':') || crate::config::protected_component(part))
    }) {
        return Err(Verdict::Deny {
            reason: format!("inspection path `{name}` is protected"),
        });
    }
    if escapes(name) {
        return Err(ask(format!("`{name}` points outside the worktree"), argv));
    }
    if name.is_empty() || name == "-" {
        return Err(ask(
            "inspection inputs cannot be established; use native read",
            argv,
        ));
    }
    let relative: PathBuf = normalized
        .split('/')
        .filter(|part| !part.is_empty() && *part != ".")
        .collect();
    let relative_name = if relative.as_os_str().is_empty() {
        ".".into()
    } else {
        relative.to_string_lossy().replace('\\', "/")
    };
    let target = cfg.root.join(&relative);
    // Check Read even for absent inputs, and before any operator grant.
    let check_read = |directory| {
        if let Some(policy) = &cfg.permissions {
            let request = crate::permissions::Request::Read {
                path: &relative_name,
                directory,
            };
            match policy.check(Some(&request), crate::types::Effect::Read) {
                crate::permissions::Verdict::Deny { reason, .. } => {
                    return Err(Verdict::Deny { reason })
                }
                crate::permissions::Verdict::Ask { rule } => {
                    return Err(Verdict::Deny {
                        reason: format!(
                        "Read approval required by `{rule}`; use native read, not command approval"
                    ),
                    })
                }
                _ => (),
            }
        }
        Ok(())
    };
    if cfg!(not(windows)) && name.contains('\\') {
        check_read(false)?;
        return Err(ask(
            "backslash inspection paths are ambiguous on this platform",
            argv,
        ));
    }
    if name.contains(['*', '?', '[', ']']) {
        check_read(false)?;
        return Err(ask(
            "wildcard inspection inputs cannot be established; use native read",
            argv,
        ));
    }
    let mut cursor = cfg.root.clone();
    for part in relative.components() {
        cursor.push(part);
        match std::fs::symlink_metadata(&cursor) {
            Ok(metadata) => {
                let linked = metadata.file_type().is_symlink();
                #[cfg(windows)]
                let linked = {
                    use std::os::windows::fs::MetadataExt;
                    linked || metadata.file_attributes() & 0x400 != 0 // reparse point, including junctions
                };
                if linked {
                    return Err(Verdict::Deny {
                        reason: format!("symlinked inspection path `{name}` refused"),
                    });
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
            Err(_) => return Err(ask("cannot inspect filesystem inputs", argv)),
        }
    }
    let metadata = match std::fs::symlink_metadata(&target) {
        Ok(metadata) => metadata,
        Err(_) => {
            check_read(false)?;
            return Err(ask("cannot establish inspection input", argv));
        }
    };
    check_read(metadata.is_dir())?;
    if !metadata.is_file() && !metadata.is_dir() {
        return Err(ask(
            "inspection input is not a regular file or directory",
            argv,
        ));
    }
    let root = cfg
        .root
        .canonicalize()
        .map_err(|_| ask("cannot establish worktree root", argv))?;
    let resolved = target
        .canonicalize()
        .map_err(|_| ask("cannot resolve inspection input", argv))?;
    if !resolved.starts_with(root) {
        return Err(Verdict::Deny {
            reason: format!("inspection path `{name}` escapes through a link"),
        });
    }
    Ok(target)
}

/// All entries, including hidden/ignored ones, are checked; tool-specific
/// excludes are not proof of a permission boundary. Never follow links. This
/// preflight is bounded, but not atomic against a concurrently changing tree.
fn inspection_inputs(
    paths: &[String],
    cfg: &Config,
    argv: &[String],
    git: bool,
) -> Option<Verdict> {
    const MAX_ENTRIES: usize = 2048;
    const MAX_DEPTH: usize = 32;
    let mut failure = None;
    // Validate every explicit input before a large directory can exhaust the
    // traversal budget. A later argv Read Deny must still beat an earlier Ask.
    let mut stack = Vec::new();
    for name in paths {
        match inspection_path(name, cfg, argv) {
            Ok(_) => stack.push((name.clone(), 0)),
            Err(verdict) => inspection_failure(&mut failure, verdict),
        }
    }
    let mut count = 0;
    while let Some((name, depth)) = stack.pop() {
        count += 1;
        if count > MAX_ENTRIES || depth > MAX_DEPTH {
            inspection_failure(
                &mut failure,
                ask("inspection preflight limit exceeded; use native read", argv),
            );
            if count > MAX_ENTRIES {
                break;
            }
            continue;
        }
        let target = match inspection_path(&name, cfg, argv) {
            Ok(path) => path,
            Err(verdict) => {
                inspection_failure(&mut failure, verdict);
                continue;
            }
        };
        if target.is_dir() {
            match std::fs::read_dir(&target) {
                Ok(entries) => {
                    for entry in entries {
                        if count + stack.len() >= MAX_ENTRIES {
                            inspection_failure(
                                &mut failure,
                                ask("inspection preflight limit exceeded; use native read", argv),
                            );
                            break;
                        }
                        match entry {
                            Ok(entry) => {
                                // Git's worktree scans do not traverse its own administrative directory.
                                // An explicit .git path is still protected, like every other input.
                                if git && target == cfg.root && entry.file_name() == ".git" {
                                    continue;
                                }
                                let relative = entry
                                    .path()
                                    .strip_prefix(&cfg.root)
                                    .ok()
                                    .and_then(|p| p.to_str())
                                    .map(str::to_owned);
                                match relative {
                                    Some(path) => stack.push((path, depth + 1)),
                                    None => inspection_failure(
                                        &mut failure,
                                        ask("non-UTF-8 inspection path cannot be checked", argv),
                                    ),
                                }
                            }
                            Err(_) => inspection_failure(
                                &mut failure,
                                ask("cannot enumerate inspection inputs", argv),
                            ),
                        }
                    }
                }
                Err(_) => inspection_failure(
                    &mut failure,
                    ask("cannot enumerate inspection directory", argv),
                ),
            }
        }
    }
    failure
}

fn classify_safe(argv: &[String], cfg: &Config) -> Option<Verdict> {
    let program = base_name(&argv[0]);
    let p = program.as_str();
    let sub = argv.get(1).map(String::as_str).unwrap_or("");
    let git = p == "git";
    if git {
        let permitted = GIT_READ.contains(&sub)
            || matches!(sub, "branch" | "tag" | "remote")
            || (sub == "stash" && argv.get(2).map(String::as_str) == Some("list"));
        if !permitted {
            return None;
        }
    }
    if !git && p != "find" && !SAFE_PLAIN.contains(&p) && !PATTERN_FIRST.contains(&p) {
        return None;
    }
    let grammar = if git {
        format!("git {sub}")
    } else {
        program.clone()
    };
    let mut failure = None;
    let mut paths = Vec::new();
    let mut input_paths = Vec::new();
    let mut pattern_pending = PATTERN_FIRST.contains(&p) || (git && sub == "grep");
    let mut pattern_seen = false;
    let mut recursive = false;
    let mut git_metadata = git
        && matches!(
            sub,
            "status" | "branch" | "tag" | "remote" | "stash" | "ls-files" | "grep"
        );
    let literal_operands = matches!(
        p,
        "echo"
            | "basename"
            | "dirname"
            | "uname"
            | "pwd"
            | "whoami"
            | "hostname"
            | "true"
            | "false"
            | "date"
            | "where"
            | "which"
    );
    let mut after_dashes = false;
    let mut i = if git {
        if sub == "stash" {
            3
        } else {
            2
        }
    } else {
        1
    };
    while i < argv.len() {
        let arg = &argv[i];
        i += 1;
        if !after_dashes && arg == "--" {
            after_dashes = true;
            continue;
        }
        if p == "find" && matches!(arg.as_str(), "(" | ")" | "!") {
            continue;
        }
        let is_flag = !after_dashes
            && ((arg.starts_with('-') && arg.len() > 1)
                || (p == "where" && arg.starts_with('/') && arg.len() == 2));
        if is_flag {
            let (flag, attached) = arg
                .split_once('=')
                .map_or((arg.as_str(), None), |(f, v)| (f, Some(v)));
            let kind = inspection_option(&grammar, flag);
            if kind == Some(InspectionArg::Boolean) && attached.is_none() {
                recursive |= matches!(flag, "-r" | "--recursive");
                git_metadata |= git
                    && ((sub == "log" && flag == "--oneline")
                        || (sub == "diff"
                            && matches!(
                                flag,
                                "--stat"
                                    | "--numstat"
                                    | "--shortstat"
                                    | "--name-only"
                                    | "--name-status"
                                    | "--summary"
                            )));
                continue;
            }
            if kind.is_none()
                && !arg.starts_with("--")
                && p != "find"
                && arg.starts_with('-')
                && arg[1..].chars().all(|c| {
                    inspection_option(&grammar, &format!("-{c}")) == Some(InspectionArg::Boolean)
                })
            {
                recursive |= arg[1..].contains('r');
                continue;
            }
            let Some(kind) = kind.filter(|kind| *kind != InspectionArg::Boolean) else {
                inspection_failure(
                    &mut failure,
                    ask(
                        format!("{grammar} option `{arg}` is not a known safe option"),
                        argv,
                    ),
                );
                // Attached short path options never qualify as Safe, but their
                // unambiguous path suffix must not evade a known Read denial.
                if !arg.starts_with("--") && p != "find" {
                    for (offset, c) in arg[1..].char_indices() {
                        match inspection_option(&grammar, &format!("-{c}")) {
                            Some(InspectionArg::Boolean) => continue,
                            Some(InspectionArg::Path) => {
                                let value =
                                    arg[1 + offset + c.len_utf8()..].trim_start_matches('=');
                                if !value.is_empty() {
                                    paths.push(value.to_string());
                                }
                            }
                            _ => (),
                        }
                        break;
                    }
                }
                continue;
            };
            if let Some(value) = attached.filter(|_| !flag.starts_with("--")) {
                inspection_failure(
                    &mut failure,
                    ask("attached short option values require approval", argv),
                );
                if kind == InspectionArg::Path {
                    paths.push(value.to_string());
                }
                continue;
            }
            let value = match attached {
                Some(value) => value,
                None => match argv.get(i) {
                    Some(value) => {
                        i += 1;
                        value.as_str()
                    }
                    None => {
                        inspection_failure(
                            &mut failure,
                            ask("inspection option is missing its value", argv),
                        );
                        continue;
                    }
                },
            };
            match kind {
                InspectionArg::Path => {
                    paths.push(value.to_string());
                    if matches!(flag, "-f" | "--file") && pattern_pending {
                        pattern_pending = false;
                        pattern_seen = true;
                    }
                    if matches!(p, "file" | "where") {
                        inspection_failure(
                            &mut failure,
                            ask("indirect inspection input list requires approval", argv),
                        );
                    }
                    if matches!(flag, "--search-path") {
                        input_paths.push(value.to_string());
                    }
                }
                InspectionArg::Pattern => {
                    if matches!(flag, "-e" | "--regexp")
                        && (matches!(p, "grep" | "egrep" | "fgrep" | "rg")
                            || (git && sub == "grep"))
                    {
                        pattern_pending = false;
                        pattern_seen = true;
                    }
                }
                InspectionArg::Numeric => {
                    if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
                        inspection_failure(
                            &mut failure,
                            ask("inspection numeric option needs an unsigned number", argv),
                        );
                    }
                }
                InspectionArg::Boolean => unreachable!(),
            }
            continue;
        }
        if pattern_pending {
            pattern_pending = false;
            pattern_seen = true;
            continue;
        }
        if literal_operands {
            if (p == "date" && !arg.starts_with('+')) || p == "hostname" {
                inspection_failure(
                    &mut failure,
                    ask("machine-setting operands require approval", argv),
                );
            }
            continue;
        }
        if git && matches!(sub, "branch" | "tag" | "remote" | "stash") {
            inspection_failure(
                &mut failure,
                ask("git listing operands require approval", argv),
            );
        }
        // Ref:path syntax reads historical content. Check the path independently
        // of whether the revision can be resolved; never mistake it for a literal.
        let path = if git {
            if let Some((revision, path)) = arg.split_once(':') {
                inspection_failure(
                    &mut failure,
                    ask("git historical inputs require approval", argv),
                );
                if revision == "HEAD"
                    || revision.strip_prefix("HEAD").is_some_and(|r| {
                        r.chars()
                            .all(|c| c.is_ascii_digit() || matches!(c, '~' | '^'))
                    })
                {
                    path
                } else {
                    arg.as_str()
                }
            } else {
                arg.as_str()
            }
        } else {
            arg.as_str()
        };
        paths.push(path.to_string());
        input_paths.push(path.to_string());
    }
    if (matches!(p, "grep" | "egrep" | "fgrep" | "rg" | "ag" | "findstr") || (git && sub == "grep"))
        && !pattern_seen
    {
        inspection_failure(&mut failure, ask("search pattern is missing", argv));
    }
    if matches!(p, "where" | "which" | "findstr") {
        inspection_failure(
            &mut failure,
            ask("search inputs cannot be confined by this grammar", argv),
        );
    }
    if git && !git_metadata {
        inspection_failure(
            &mut failure,
            ask(
                "git content/revision inputs cannot be proven; use native read",
                argv,
            ),
        );
    }
    if (!literal_operands || git) && input_paths.is_empty() {
        // Pattern files do not establish the files searched. Only documented
        // default-directory forms qualify; stdin/indirect inputs ask instead.
        paths.push(".".into());
        let default_directory =
            git || matches!(
                p,
                "ls" | "dir" | "du" | "tree" | "find" | "rg" | "ag" | "fd" | "fdfind"
            ) || (matches!(p, "grep" | "egrep" | "fgrep") && recursive);
        if !default_directory {
            inspection_failure(
                &mut failure,
                ask(
                    "inspection file inputs are not established; use native read",
                    argv,
                ),
            );
        }
    }
    if let Some(verdict) = inspection_inputs(&paths, cfg, argv, git) {
        inspection_failure(&mut failure, verdict);
    }
    Some(failure.unwrap_or(Verdict::Allow(Basis::Safe)))
}

/// Whether a sandbox confines this call, and whether it cuts the network:
/// `(filesystem and network confined, network cut)`. A profile no backend can
/// satisfy counts as unconfined; execution would refuse it anyway.
pub fn confinement(
    cfg: &Config,
    profile: crate::types::SandboxProfile,
    network: crate::types::NetworkPolicy,
) -> (bool, bool) {
    match crate::sandbox::SandboxManager::new(cfg).prepare(profile, network) {
        Ok(plan) => (
            plan.filesystem_confined && plan.network_confined,
            plan.network_confined,
        ),
        Err(_) => (false, false),
    }
}

/// The profile a local session starts with, as the agent picks it.
pub fn default_profile(cfg: &Config) -> crate::types::SandboxProfile {
    if cfg.sandbox_mode == crate::types::SandboxMode::Off {
        crate::types::SandboxProfile::TrustedDev
    } else {
        crate::types::SandboxProfile::DevelopmentConfined
    }
}

/// Decide one command, including bounded filesystem preflight for inspections
/// and the fixed-list check for `git add` scope.
pub fn classify(argv: &[String], ctx: &Context) -> Verdict {
    let Some(first) = argv.first() else {
        return Verdict::Deny {
            reason: "empty command".into(),
        };
    };
    if argv.iter().any(|a| a.contains('\0')) {
        return Verdict::Deny {
            reason: "arguments cannot contain NUL".into(),
        };
    }
    if let Some(reason) = hard_deny(argv) {
        return Verdict::Deny { reason };
    }
    if let Some(rule) = ctx
        .rules
        .iter()
        .find(|r| !r.allow && prefix_matches(&r.argv, argv))
    {
        return Verdict::Deny {
            reason: format!("denied by operator rule `{}`", rule.argv.join(" ")),
        };
    }
    let inspection = is_bare(first)
        .then(|| classify_safe(argv, ctx.cfg))
        .flatten();
    if let Some(Verdict::Deny { .. }) = &inspection {
        return inspection.unwrap();
    }
    if ctx
        .rules
        .iter()
        .any(|r| r.allow && prefix_matches(&r.argv, argv))
    {
        return Verdict::Allow(Basis::Rule);
    }
    if ctx.session.iter().any(|s| prefix_matches(s, argv)) {
        return Verdict::Allow(Basis::Session);
    }
    let program = base_name(first);
    if !is_bare(first) {
        return ask(
            format!("`{first}` is a path, not a known tool; a file in the project can pretend to be any program"),
            argv,
        );
    }
    if SHELLS_AND_WRAPPERS.contains(&program.as_str()) {
        return ask(
            format!("{program} runs another command line; give the command itself instead"),
            argv,
        );
    }
    if NETWORK_PROGRAMS.contains(&program.as_str())
        || (program == "git"
            && matches!(
                argv.get(1).map(String::as_str),
                Some("push" | "pull" | "fetch" | "clone")
            ))
    {
        if ctx.network_cut {
            return Verdict::Deny {
                reason: format!("{program} needs the network, and the sandbox has none. The operator can change that with --sandbox-net"),
            };
        }
        return ask(format!("{program} talks to the network"), argv);
    }
    if let Some(verdict) = inspection {
        return verdict;
    }
    // The classic list must not bypass inspection grammar/Read checks or the
    // confinement and override checks for builds. Explicit --allow-exec remains
    // a separate execution mode outside this module.
    if (build_tool(argv).is_none() || program == "rustc")
        && crate::exec::validate(argv, ctx.cfg).is_ok()
        && check_paths(argv, false, &[]).is_none()
    {
        return Verdict::Allow(Basis::Allowlist);
    }
    if build_tool(argv).is_some() {
        if let Some(flag) = argv.iter().skip(1).find(|arg| {
            ["--manifest-path", "--config", "--target-dir"]
                .iter()
                .any(|flag| arg.as_str() == *flag || arg.starts_with(&format!("{flag}=")))
        }) {
            return ask(
                format!("{program} {flag} overrides build inputs or execution settings"),
                argv,
            );
        }
        // A build or test runs the project's own code.
        if ctx.confined {
            return match check_paths(argv, false, &[]) {
                Some(why) => ask(why, argv),
                None if crate::exec::validate(argv, ctx.cfg).is_ok() => {
                    Verdict::Allow(Basis::Allowlist)
                }
                None => Verdict::Allow(Basis::Build),
            };
        }
        return ask(
            format!("{program} runs project code, and no sandbox confines it here. Enable one (--sandbox auto) to run builds and tests without asking"),
            argv,
        );
    }
    if MUTATORS.contains(&program.as_str()) {
        return ask(
            format!("{program} changes files or exposes the environment"),
            argv,
        );
    }
    if PACKAGE_MANAGERS.contains(&program.as_str()) {
        let sub = argv.get(1).map(String::as_str).unwrap_or("");
        return ask(
            format!("{program} {sub} installs or changes dependencies (it downloads and may run third-party code), or is not a build or test step"),
            argv,
        );
    }
    if program == "git" {
        let sub = argv.get(1).map(String::as_str).unwrap_or("");
        let why = if GIT_ASK.contains(&sub) {
            format!("git {sub} changes the repository")
        } else {
            format!("git {sub} is not a recognised read-only command")
        };
        return ask(why, argv);
    }
    ask(format!("{program} is not a recognised tool"), argv)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> Config {
        let mut cfg = Config::baseline();
        cfg.allow_exec = true;
        cfg.exec_mode = crate::config::ExecMode::Smart;
        cfg
    }

    struct TestTree {
        cfg: Config,
    }

    impl TestTree {
        fn new() -> Self {
            let mut cfg = cfg();
            cfg.root = std::env::temp_dir()
                .join(format!("jani-inspection-{}", crate::process::unique_id()));
            cfg.permissions = None;
            std::fs::create_dir_all(cfg.root.join("src/private")).unwrap();
            for name in [
                "src/main.rs",
                "src/lib.rs",
                "src/routes.rs",
                "src/private/info.txt",
                "README.md",
                "Cargo.toml",
                "main",
                "patterns.txt",
                "in.txt",
                "out.txt",
                "--stat",
            ] {
                std::fs::write(cfg.root.join(name), "TODO fn /api/users\n").unwrap();
            }
            Self { cfg }
        }

        fn verdict(&self, line: &str) -> Verdict {
            classify(
                &argv(line),
                &Context {
                    cfg: &self.cfg,
                    confined: false,
                    network_cut: false,
                    rules: &[],
                    session: &[],
                },
            )
        }

        fn read_rules(&mut self, allow: &[&str], ask: &[&str], deny: &[&str]) {
            let owned = |rules: &[&str]| rules.iter().map(|s| s.to_string()).collect::<Vec<_>>();
            self.cfg.permissions = Some(
                crate::permissions::Policy::compile(
                    crate::permissions::Mode::Default,
                    &owned(allow),
                    &owned(ask),
                    &owned(deny),
                )
                .unwrap(),
            );
        }
    }

    impl Drop for TestTree {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.cfg.root);
        }
    }

    fn argv(line: &str) -> Vec<String> {
        line.split_whitespace().map(str::to_string).collect()
    }

    fn verdict(line: &str, confined: bool) -> Verdict {
        let tree = TestTree::new();
        classify(
            &argv(line),
            &Context {
                cfg: &tree.cfg,
                confined,
                network_cut: confined,
                rules: &[],
                session: &[],
            },
        )
    }

    fn allowed(line: &str, confined: bool) -> Option<Basis> {
        match verdict(line, confined) {
            Verdict::Allow(basis) => Some(basis),
            _ => None,
        }
    }

    fn asks(line: &str, confined: bool) -> bool {
        matches!(verdict(line, confined), Verdict::Ask { .. })
    }

    fn denied(line: &str, confined: bool) -> bool {
        matches!(verdict(line, confined), Verdict::Deny { .. })
    }

    #[test]
    fn inspection_runs_on_its_own_even_without_a_sandbox() {
        for line in [
            "ls -la src",
            "grep -rn TODO src",
            "rg fn main",
            "rg --glob *.rs unsafe",
            "find . -name *.rs",
            "find src -type f -name /api/*",
            "cat README.md",
            "git status",
            "git diff --stat",
            "git log --oneline -n 5",
            "git branch -a",
            "wc -l src/main.rs",
            "head -n 20 Cargo.toml",
        ] {
            // Filesystem-sensitive expectations use a clean, isolated worktree.
            assert!(
                matches!(allowed(line, false), Some(Basis::Safe | Basis::Allowlist)),
                "{line}"
            );
        }
    }

    #[test]
    fn a_pattern_may_look_like_a_path_but_a_path_may_not_escape() {
        assert_eq!(
            allowed("grep /api/users src/routes.rs", false),
            Some(Basis::Safe)
        );
        assert_eq!(allowed("rg -e ../x -- src", false), Some(Basis::Safe));
        for line in [
            "cat ../outside.txt",
            "cat /etc/passwd",
            "cat ~/outside.txt",
            "ls C:\\Users",
            "grep -r token ..",
            "git diff --no-index a b",
            "head --file=/etc/shadow x",
        ] {
            assert!(asks(line, false), "{line}");
        }
    }

    #[test]
    fn read_only_tools_do_not_become_writers_through_flags() {
        for line in [
            "find . -delete",
            "find . -exec rm {} ;",
            "find . -fprint out.txt",
            "sort -o out.txt in.txt",
            "rg --pre ./evil pattern",
            "git diff --output=x.patch",
            "git log -c core.pager=evil",
            "git branch newbranch",
            "git tag v1",
        ] {
            assert!(asks(line, false), "{line}");
        }
    }

    #[test]
    fn inspection_options_are_per_program_and_never_guess_values() {
        let tree = TestTree::new();
        for line in [
            "fd -x rm",
            "fd -X rm",
            "fd -ix rm",
            "fd -xrm",
            "fdfind -ix rm",
            "sort -o out.txt in.txt",
            "sort -osrc/lib.rs in.txt",
            "tree -osrc/lib.rs src",
            "tree --output=src/lib.rs src",
            "rg --unknown pattern src",
            "ls --unknown src",
            "fd -rn TODO src",
            "hostname changed",
            "git diff --stat --check",
            "git diff -- --stat",
            "grep -e -r",
            "file -z src/lib.rs",
            "rg -z TODO src",
            "grep -rnZQ TODO src",
            "grep -n20 TODO src",
            "rg -g*.rs TODO src",
            "date -f /etc/shadow",
            "date --file=/etc/shadow",
            "where /R C:\\ *",
            "git grep -f /etc/shadow TODO src",
            "git grep --file=/etc/shadow src",
            "git status --unknown",
            "git diff --output=src/lib.rs",
            "git log -ccore.pager=evil",
            "file -f patterns.txt",
            "head --file=/etc/shadow src/lib.rs",
            "sh -c ls",
            "unknown-tool src/lib.rs",
        ] {
            assert!(
                matches!(tree.verdict(line), Verdict::Ask { .. }),
                "{line}: {:?}",
                tree.verdict(line)
            );
        }
        for line in [
            "ls -la src",
            "grep -rn TODO src",
            "grep -in TODO src/lib.rs",
            "grep -rn TODO",
            "rg -ni TODO src",
            "file -bi src/lib.rs",
            "fd -i TODO src",
            "fd -e rs /api/* src",
            "head --lines=2 src/lib.rs",
            "grep --context=2 TODO src",
            "git grep -ni TODO src",
            "git status -sb",
            "date -u",
        ] {
            assert_eq!(tree.verdict(line), Verdict::Allow(Basis::Safe), "{line}");
        }
    }

    #[test]
    fn patterns_and_literals_are_not_inspection_paths() {
        let tree = TestTree::new();
        for line in [
            "grep /etc/shadow src/lib.rs",
            "rg ../outside src",
            "rg --regexp=.env src",
            "grep -e .git/config src/lib.rs",
            "rg --glob=/api/* TODO src",
            "grep --exclude-dir=../outside TODO src",
            "find src -name /etc/* -type f",
            "echo .env /etc/shadow",
            "basename /etc/shadow",
            "dirname .git/config",
            "git grep /etc/shadow src",
            "git grep -e .env src",
            "git grep -f patterns.txt src",
            "git grep --file=patterns.txt -- src",
            "rg --file=patterns.txt src",
        ] {
            assert_eq!(tree.verdict(line), Verdict::Allow(Basis::Safe), "{line}");
        }
        for line in [
            "git grep TODO ../outside",
            "git grep -e TODO /etc/shadow",
            "grep TODO ../outside",
            "rg --file=../outside src",
        ] {
            assert!(matches!(tree.verdict(line), Verdict::Ask { .. }), "{line}");
        }
    }

    #[test]
    fn every_inspection_path_rejects_protected_components_and_ref_paths() {
        let tree = TestTree::new();
        for line in [
            "cat .env",
            "file server.key",
            "ls .git/config",
            "stat src/lib.rs:stream",
            "grep TODO .env",
            "rg TODO .env",
            "grep -f .env TODO src",
            "rg --file=.env src",
            "grep --exclude-from=.env TODO src",
            "git grep -f .env src",
            "git grep --file=.env src",
            "git grep TODO .env",
            "git show HEAD:.env",
            "git show HEAD~1:server.key",
            "git diff HEAD:.git/config",
            "date -f .env",
            "date --reference=.env",
            "fd --search-path=.env TODO",
            "grep -f.env TODO src",
            "grep -f=.env TODO src",
            "grep -nf.env TODO src",
            "git grep -f.env src",
        ] {
            assert!(
                matches!(tree.verdict(line), Verdict::Deny { .. }),
                "{line}: {:?}",
                tree.verdict(line)
            );
        }
        for line in ["git show HEAD:src/lib.rs", "git diff", "git log -p"] {
            assert!(matches!(tree.verdict(line), Verdict::Ask { .. }), "{line}");
        }
    }

    #[test]
    fn read_deny_and_ask_fail_closed_for_paths_and_pattern_files() {
        for permission in ["deny", "ask"] {
            let mut tree = TestTree::new();
            let rules = ["Read(src/private/**)", "Read(patterns.txt)"];
            tree.read_rules(
                &[],
                if permission == "ask" { &rules } else { &[] },
                if permission == "deny" { &rules } else { &[] },
            );
            for line in [
                "cat src/private/info.txt",
                "file src/private/info.txt",
                "grep TODO src/private/info.txt",
                "rg TODO src/private/info.txt",
                "ls src/private",
                "git diff --stat src/private/info.txt",
                "git show HEAD:src/private/info.txt",
                "grep -f patterns.txt TODO src/lib.rs",
                "rg --file=patterns.txt src/lib.rs",
                "git grep -f patterns.txt src/lib.rs",
                "grep -fpatterns.txt TODO src/lib.rs",
                "git grep --file=patterns.txt src/lib.rs",
                "cat missing.txt src/private/info.txt",
                "git grep TODO src/private/*",
            ] {
                assert!(
                    matches!(tree.verdict(line), Verdict::Deny { .. }),
                    "{permission}: {line}"
                );
            }
            assert_eq!(
                tree.verdict("rg --regexp=src/private/info.txt src/lib.rs"),
                Verdict::Allow(Basis::Safe)
            );
        }
        let mut tree = TestTree::new();
        tree.read_rules(&["Read(src/**)"], &[], &[]);
        assert_eq!(tree.verdict("file src/lib.rs"), Verdict::Allow(Basis::Safe));
        assert_eq!(tree.verdict("ls -la src"), Verdict::Allow(Basis::Safe));
        assert_eq!(tree.verdict("rg TODO src"), Verdict::Allow(Basis::Safe));
        assert!(matches!(
            tree.verdict("cat README.md"),
            Verdict::Deny { .. }
        ));
    }

    #[test]
    fn recursive_and_default_scans_check_hidden_and_denied_inputs() {
        for entry in ["src/.env", "src/sub/server.key", "src/.hidden/info.txt"] {
            let mut tree = TestTree::new();
            let path = tree.cfg.root.join(entry);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, "TODO\n").unwrap();
            if entry.contains(".hidden") {
                tree.read_rules(&[], &[], &["Read(src/.hidden/**)"]);
            }
            for line in [
                "grep -rn TODO src",
                "rg TODO src",
                "rg --glob=*.rs TODO src",
                "rg TODO",
                "ag TODO src",
                "git grep TODO",
                "ls -la src",
                "find src -name *.rs",
                "fd TODO src",
            ] {
                assert!(
                    matches!(tree.verdict(line), Verdict::Deny { .. }),
                    "{entry}: {line}"
                );
            }
            assert_eq!(
                tree.verdict("grep TODO src/lib.rs"),
                Verdict::Allow(Basis::Safe)
            );
        }
        let mut tree = TestTree::new();
        tree.read_rules(&[], &["Read(src/private/**)"], &[]);
        assert!(matches!(tree.verdict("rg TODO"), Verdict::Deny { .. }));
        assert!(matches!(tree.verdict("ls src"), Verdict::Deny { .. }));
    }

    #[test]
    fn inspection_denials_precede_rules_session_and_classic_allowlist() {
        for permission in ["deny", "ask"] {
            let mut tree = TestTree::new();
            tree.read_rules(
                &[],
                if permission == "ask" {
                    &["Read(src/private/**)"]
                } else {
                    &[]
                },
                if permission == "deny" {
                    &["Read(src/private/**)"]
                } else {
                    &[]
                },
            );
            for line in [
                "cat src/private/info.txt",
                "rg --unknown TODO src/private/info.txt",
                "git diff --unknown src/private/info.txt",
                "cat .env",
                "git show HEAD:src/private/info.txt",
                "git diff --stat src/private/info.txt",
                "git status",
                "rg TODO",
            ] {
                let args = argv(line);
                let rules = [Rule {
                    allow: true,
                    argv: args[..1].to_vec(),
                    added: String::new(),
                    by: "test".into(),
                }];
                let session = [args[..1].to_vec()];
                for grants in 0..3 {
                    let result = classify(
                        &args,
                        &Context {
                            cfg: &tree.cfg,
                            confined: true,
                            network_cut: true,
                            rules: if grants == 1 { &rules } else { &[] },
                            session: if grants == 2 { &session } else { &[] },
                        },
                    );
                    assert!(
                        matches!(result, Verdict::Deny { .. }),
                        "{permission}, {grants}: {line}: {result:?}"
                    );
                }
            }
        }
        // An operator may still approve unknown grammar when no Read denial is known.
        let tree = TestTree::new();
        for line in ["rg --unknown TODO src/lib.rs", "unknown-tool --flag"] {
            let args = argv(line);
            let rules = [Rule {
                allow: true,
                argv: args[..1].to_vec(),
                added: String::new(),
                by: "test".into(),
            }];
            let session = [args[..1].to_vec()];
            assert_eq!(
                classify(
                    &args,
                    &Context {
                        cfg: &tree.cfg,
                        confined: false,
                        network_cut: false,
                        rules: &rules,
                        session: &[]
                    }
                ),
                Verdict::Allow(Basis::Rule)
            );
            assert_eq!(
                classify(
                    &args,
                    &Context {
                        cfg: &tree.cfg,
                        confined: false,
                        network_cut: false,
                        rules: &[],
                        session: &session
                    }
                ),
                Verdict::Allow(Basis::Session)
            );
        }
    }

    #[test]
    fn preflight_limits_and_unavailable_inputs_ask_without_claiming_safe() {
        let mut tree = TestTree::new();
        std::fs::create_dir_all(tree.cfg.root.join("large")).unwrap();
        for i in 0..2050 {
            std::fs::write(tree.cfg.root.join(format!("large/{i}.txt")), "TODO\n").unwrap();
        }
        for line in [
            "rg TODO large",
            "ls large",
            "file missing.txt",
            "rg TODO src/*.rs",
        ] {
            assert!(matches!(tree.verdict(line), Verdict::Ask { .. }), "{line}");
        }
        tree.read_rules(&[], &[], &["Read(src/private/**)"]);
        assert!(matches!(
            tree.verdict("rg TODO src/private/info.txt large"),
            Verdict::Deny { .. }
        ));
    }

    #[test]
    fn symlinked_external_files_and_directories_never_qualify_as_inspection() {
        let tree = TestTree::new();
        let external = TestTree::new();
        let link = tree.cfg.root.join("src/link.rs");
        let external_file = external.cfg.root.join("src/lib.rs");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&external_file, &link).unwrap();
        #[cfg(windows)]
        if let Err(error) = std::os::windows::fs::symlink_file(&external_file, &link) {
            if matches!(
                error.kind(),
                std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::Unsupported
            ) || error.raw_os_error() == Some(1314)
            {
                return;
            }
            panic!("cannot create test symlink: {error}");
        }
        for line in [
            "cat src/link.rs",
            "file src/link.rs",
            "grep TODO src/link.rs",
            "rg TODO src",
            "ls src",
            "git grep TODO src",
        ] {
            assert!(matches!(tree.verdict(line), Verdict::Deny { .. }), "{line}");
        }
        let dir_link = tree.cfg.root.join("linked");
        #[cfg(unix)]
        std::os::unix::fs::symlink(external.cfg.root.join("src"), &dir_link).unwrap();
        #[cfg(windows)]
        if let Err(error) =
            std::os::windows::fs::symlink_dir(external.cfg.root.join("src"), &dir_link)
        {
            if matches!(
                error.kind(),
                std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::Unsupported
            ) || error.raw_os_error() == Some(1314)
            {
                return;
            }
            panic!("cannot create test directory symlink: {error}");
        }
        assert!(matches!(
            tree.verdict("file linked/lib.rs"),
            Verdict::Deny { .. }
        ));
        assert!(matches!(
            tree.verdict("file linked/missing.rs"),
            Verdict::Deny { .. }
        ));
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&external_file, tree.cfg.root.join("src\\lib.rs")).unwrap();
            assert!(matches!(
                tree.verdict("file src\\lib.rs"),
                Verdict::Ask { .. }
            ));
        }
    }

    #[test]
    fn builds_and_tests_run_on_their_own_only_inside_a_sandbox() {
        for line in [
            "cargo build --release",
            "cargo clippy --fix",
            "npm run build",
            "yarn lint",
            "make test",
            "python3 -m pytest -q",
            "go test ./...",
            "gcc -o build/a main.c",
        ] {
            assert_eq!(allowed(line, true), Some(Basis::Build), "{line} confined");
            assert!(asks(line, false), "{line} unconfined must ask");
        }
        // Paths still may not leave the worktree, sandbox or not.
        assert!(asks("cargo test --manifest-path ../x/Cargo.toml", true));
        assert!(asks("gcc -o /usr/bin/a main.c", true));
    }

    #[test]
    fn fixed_list_builds_cannot_bypass_confinement_or_override_checks() {
        for line in ["cargo test", "cargo check", "cargo clippy"] {
            assert!(asks(line, false), "{line}");
            assert!(allowed(line, true).is_some(), "{line}");
        }
        for line in [
            "cargo check --manifest-path=../other/Cargo.toml",
            "cargo check --config=build.rustc-wrapper=arbitrary",
            "cargo check --target-dir=../outside",
        ] {
            assert!(asks(line, false), "{line}");
            assert!(asks(line, true), "{line}");
        }
    }

    #[test]
    fn running_a_script_or_installing_is_not_a_build() {
        for line in [
            "python3 scripts/fix.py",
            "python3 -c print(1)",
            "node server.js",
            "npm install left-pad",
            "npm ci",
            "cargo install ripgrep",
            "pip install requests",
            "./gradlew build",
            "./ls",
        ] {
            assert!(asks(line, true) || denied(line, true), "{line}");
        }
    }

    #[test]
    fn a_program_named_by_path_is_never_mistaken_for_the_tool_it_imitates() {
        assert!(asks("./ls -la", false));
        assert!(asks("node_modules/.bin/eslint .", true));
        assert!(asks("/usr/bin/cat x", false));
    }

    #[test]
    fn shells_and_wrappers_ask_because_they_hide_the_real_command() {
        for line in [
            "sh -c rm",
            "bash -lc make",
            "env FOO=1 cargo test",
            "timeout 5 cargo test",
            "powershell -Command x",
            "xargs rm",
        ] {
            assert!(asks(line, true), "{line}");
        }
    }

    #[test]
    fn nothing_can_grant_the_hard_denies() {
        for line in [
            "sudo make install",
            "mkfs.ext4 /dev/sda",
            "dd if=/dev/zero of=/dev/sda",
            "shutdown now",
            "rm -rf /",
            "rm -rf ../..",
            "rm -rf ~",
            "git push --force origin main",
            "git push -f",
            "cargo publish",
            "npm publish",
        ] {
            assert!(denied(line, true), "{line}");
        }
        // Not even an allow rule or a session grant.
        let cfg = cfg();
        let rule = Rule {
            allow: true,
            argv: argv("sudo"),
            added: String::new(),
            by: "t".into(),
        };
        let session = vec![argv("rm -rf /")];
        for line in ["sudo make install", "rm -rf /"] {
            assert!(matches!(
                classify(
                    &argv(line),
                    &Context {
                        cfg: &cfg,
                        confined: true,
                        network_cut: true,
                        rules: std::slice::from_ref(&rule),
                        session: &session
                    }
                ),
                Verdict::Deny { .. }
            ));
        }
        let dir = std::env::temp_dir().join(format!("jani-rules-{}", std::process::id()));
        assert!(add_rule(&dir, true, argv("sudo make"), "t").is_err());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn network_tools_are_denied_when_the_sandbox_has_no_network_and_ask_otherwise() {
        assert!(denied("curl https://example.com", true));
        assert!(denied("git push origin main", true));
        assert!(asks("curl https://example.com", false));
        assert!(asks("git pull", false));
    }

    #[test]
    fn mutating_and_unknown_commands_ask_with_a_useful_suggestion() {
        match verdict("rm src/old.rs", true) {
            Verdict::Ask { suggest, .. } => assert_eq!(suggest, argv("rm src/old.rs")),
            other => panic!("{other:?}"),
        }
        match verdict("docker compose up", false) {
            Verdict::Ask { suggest, .. } => assert_eq!(suggest, argv("docker compose")),
            other => panic!("{other:?}"),
        }
        match verdict("npm install left-pad", true) {
            Verdict::Ask { suggest, .. } => assert_eq!(suggest, argv("npm install")),
            other => panic!("{other:?}"),
        }
        assert_eq!(
            suggest_prefix(&argv("npm run build -- --prod")),
            argv("npm run build")
        );
        assert_eq!(
            suggest_prefix(&argv("python3 -m black src")),
            argv("python3 -m black")
        );
        assert!(asks("frobnicate --all", true));
    }

    #[test]
    fn operator_rules_widen_and_narrow_but_deny_wins() {
        let cfg = cfg();
        let allow = Rule {
            allow: true,
            argv: argv("npm install"),
            added: String::new(),
            by: "t".into(),
        };
        let deny = Rule {
            allow: false,
            argv: argv("npm install left-pad"),
            added: String::new(),
            by: "t".into(),
        };
        let rules = [allow, deny];
        let with = |line: &str| {
            classify(
                &argv(line),
                &Context {
                    cfg: &cfg,
                    confined: true,
                    network_cut: false,
                    rules: &rules,
                    session: &[],
                },
            )
        };
        assert_eq!(with("npm install react"), Verdict::Allow(Basis::Rule));
        assert!(matches!(with("npm install left-pad"), Verdict::Deny { .. }));
        assert!(matches!(with("npm uninstall react"), Verdict::Ask { .. }));
        // Whole-token prefixes only: `npm installer` is not `npm install`.
        assert!(matches!(with("npm installer"), Verdict::Ask { .. }));
    }

    #[test]
    fn session_grants_apply_to_their_prefix_only() {
        let cfg = cfg();
        let session = vec![argv("docker compose")];
        let with = |line: &str| {
            classify(
                &argv(line),
                &Context {
                    cfg: &cfg,
                    confined: false,
                    network_cut: false,
                    rules: &[],
                    session: &session,
                },
            )
        };
        assert_eq!(with("docker compose up"), Verdict::Allow(Basis::Session));
        assert!(matches!(with("docker run x"), Verdict::Ask { .. }));
    }

    #[test]
    fn rules_persist_atomically_and_malformed_files_fail_closed() {
        let dir = std::env::temp_dir().join(format!("jani-rules-store-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        assert!(load_rules(&dir).unwrap().is_empty());
        add_rule(&dir, true, argv("cargo build"), "ana").unwrap();
        add_rule(&dir, false, argv("npm install evil"), "ana").unwrap();
        add_rule(&dir, true, argv("cargo build"), "ana").unwrap(); // duplicate is a no-op
        let rules = load_rules(&dir).unwrap();
        assert_eq!(rules.len(), 2);
        assert!(rules[0].allow && rules[0].argv == argv("cargo build") && rules[0].by == "ana");
        assert!(!rules[1].allow);
        assert!(remove_rule(&dir, 3).is_err());
        assert_eq!(remove_rule(&dir, 1).unwrap().argv, argv("cargo build"));
        assert_eq!(load_rules(&dir).unwrap().len(), 1);
        std::fs::write(dir.join("exec-rules.json"), "{ nope").unwrap();
        assert!(
            load_rules(&dir).is_err(),
            "a corrupt file must not read as 'no rules'"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn build_output_directories_are_per_tool_and_only_created_when_harmless() {
        let root = std::env::temp_dir().join(format!("jani-outputs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("dist")).unwrap();
        // cargo makes its own `target`, so it is opened up even when absent.
        assert_eq!(
            build_outputs(&argv("cargo test"), &root),
            vec![PathBuf::from("target")]
        );
        // make/npm: only what already exists (never create `build/` for a `make build`).
        assert!(!build_outputs(&argv("make build"), &root).contains(&PathBuf::from("build")));
        assert_eq!(
            build_outputs(&argv("npm run build"), &root),
            vec![PathBuf::from("dist")]
        );
        assert!(build_outputs(&argv("ls"), &root).is_empty());
        let _ = std::fs::remove_dir_all(root);
    }
}
