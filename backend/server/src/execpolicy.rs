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
    let value = match arg.strip_prefix("--") {
        Some(flag) => match flag.split_once('=') {
            Some((_, value)) => value,
            None => return false,
        },
        None => arg,
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
/// Programs with no path arguments at all worth checking.
const NO_PATHS: &[&str] = &[
    "echo", "pwd", "date", "uname", "whoami", "hostname", "basename", "dirname", "true", "false",
    "which", "where",
];
/// Flags that make an otherwise read-only tool write or run something.
const UNSAFE_FLAGS: &[(&str, &[&str])] = &[
    (
        "sort",
        &[
            "-o",
            "--output",
            "--compress-program",
            "-T",
            "--temporary-directory",
        ],
    ),
    ("tree", &["-o"]),
    ("file", &["-C", "--compile", "-m", "--magic-file"]),
    ("rg", &["--pre", "--pre-glob", "--hostname-bin"]),
    ("fd", &["-x", "--exec", "-X", "--exec-batch"]),
    ("fdfind", &["-x", "--exec", "-X", "--exec-batch"]),
    ("ag", &["--pager"]),
    ("grep", &["--pager"]),
    ("findstr", &["/f"]),
];
const FIND_WRITES: &[&str] = &[
    "-exec", "-execdir", "-ok", "-okdir", "-delete", "-fprint", "-fprint0", "-fprintf", "-fls",
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
const GIT_UNSAFE_FLAGS: &[&str] = &[
    "--output",
    "--ext-diff",
    "--textconv",
    "--open-files-in-pager",
    "-O",
    "--exec-path",
    "--upload-pack",
    "--no-index",
    "-c",
    "-C",
    "--git-dir",
    "--work-tree",
    "--config",
    "--exec",
    "--receive-pack",
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

fn classify_safe(argv: &[String]) -> Option<Verdict> {
    let program = base_name(&argv[0]);
    let p = program.as_str();
    let sub = argv.get(1).map(String::as_str).unwrap_or("");
    if p == "git" {
        if !GIT_READ.contains(&sub) {
            // branch / tag / remote listings are read-only without arguments.
            let listing = match sub {
                "branch" => argv.iter().skip(2).all(|a| {
                    matches!(
                        a.as_str(),
                        "-a" | "-r"
                            | "-v"
                            | "-vv"
                            | "--list"
                            | "--show-current"
                            | "--all"
                            | "--remotes"
                    )
                }),
                "tag" => {
                    argv.len() == 2
                        || argv
                            .iter()
                            .skip(2)
                            .all(|a| matches!(a.as_str(), "-l" | "--list" | "-n"))
                }
                "remote" => argv.len() == 2 || argv.get(2).map(String::as_str) == Some("-v"),
                "stash" => argv.get(2).map(String::as_str) == Some("list"),
                _ => false,
            };
            if !listing {
                return None;
            }
        }
        if let Some(flag) = argv.iter().skip(1).find(|a| {
            GIT_UNSAFE_FLAGS
                .iter()
                .any(|f| *a == f || a.starts_with(&format!("{f}=")))
        }) {
            return Some(ask(
                format!("git {flag} can run a program or leave the worktree"),
                argv,
            ));
        }
        if sub != "grep" {
            if let Some(why) = check_paths(argv, false, &[]) {
                return Some(ask(why, argv));
            }
        }
        return Some(Verdict::Allow(Basis::Safe));
    }
    if p == "find" {
        if let Some(flag) = argv.iter().find(|a| FIND_WRITES.contains(&a.as_str())) {
            return Some(ask(
                format!("find {flag} can change files or run a program"),
                argv,
            ));
        }
        if let Some(why) = check_paths(argv, false, FIND_PATTERN_ARGS) {
            return Some(ask(why, argv));
        }
        return Some(Verdict::Allow(Basis::Safe));
    }
    let known = SAFE_PLAIN.contains(&p) || PATTERN_FIRST.contains(&p);
    if !known {
        return None;
    }
    if let Some((_, flags)) = UNSAFE_FLAGS.iter().find(|(name, _)| *name == p) {
        if let Some(flag) = argv.iter().skip(1).find(|a| {
            flags
                .iter()
                .any(|f| a.eq_ignore_ascii_case(f) || a.starts_with(&format!("{f}=")))
        }) {
            return Some(ask(
                format!("{p} {flag} can write a file or run a program"),
                argv,
            ));
        }
    }
    if !NO_PATHS.contains(&p) {
        if let Some(why) = check_paths(argv, PATTERN_FIRST.contains(&p), &[]) {
            return Some(ask(why, argv));
        }
    }
    Some(Verdict::Allow(Basis::Safe))
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

/// Decide one command. Pure: no I/O besides the fixed-list check reading files
/// for `git add` scope.
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
    // The classic fixed list keeps working exactly as before (its git add and
    // git commit scope checks included).
    if is_bare(first) && crate::exec::validate(argv, ctx.cfg).is_ok() {
        return Verdict::Allow(Basis::Allowlist);
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
    if let Some(verdict) = classify_safe(argv) {
        return verdict;
    }
    if build_tool(argv).is_some() {
        // A build or test runs the project's own code.
        if ctx.confined {
            return match check_paths(argv, false, &[]) {
                Some(why) => ask(why, argv),
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

    fn argv(line: &str) -> Vec<String> {
        line.split_whitespace().map(str::to_string).collect()
    }

    fn verdict(line: &str, confined: bool) -> Verdict {
        let cfg = cfg();
        classify(
            &argv(line),
            &Context {
                cfg: &cfg,
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
            // The classic fixed list claims a few of these first (git status); same outcome.
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
            "cat ../secrets.txt",
            "cat /etc/passwd",
            "cat ~/.ssh/id_rsa",
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
