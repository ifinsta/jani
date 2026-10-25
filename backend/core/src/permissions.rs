//! Claude-style project permission rules compiled from `.jani/settings.json`.
//!
//! Rules are a local narrowing layer, never an authority source. A settings
//! `allow` rule cannot enable a write or command that the operator did not
//! already enable on the CLI, and it cannot override a signed policy denial.
//! `deny` and `ask` only add refusal or approval requirements.

use crate::schema::glob_match;
use crate::types::Effect;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    #[default]
    Default,
    AcceptEdits,
    Plan,
}

impl Mode {
    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "default" => Ok(Self::Default),
            "acceptEdits" => Ok(Self::AcceptEdits),
            "plan" => Ok(Self::Plan),
            "bypassPermissions" => Err(
                "permissions.defaultMode 'bypassPermissions' is refused: Jani never bypasses policy"
                    .into(),
            ),
            other => Err(format!(
                "unknown permissions.defaultMode '{other}'; use default, acceptEdits or plan"
            )),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::AcceptEdits => "acceptEdits",
            Self::Plan => "plan",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Category {
    Read,
    Write,
    Bash,
}

impl Category {
    fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Write => "write",
            Self::Bash => "command",
        }
    }
}

#[derive(Clone, Debug)]
struct Rule {
    category: Category,
    pattern: String,
    raw: String,
}

/// A tool call normalized for permission matching. Paths are worktree-relative.
#[derive(Clone, Debug)]
pub enum Request<'a> {
    Read { path: &'a str, directory: bool },
    Write { path: &'a str },
    Command { line: String },
}

impl Request<'_> {
    fn category(&self) -> Category {
        match self {
            Self::Read { .. } => Category::Read,
            Self::Write { .. } => Category::Write,
            Self::Command { .. } => Category::Bash,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// No settings rule applies; operator flags and policy decide.
    Unrestricted,
    /// A settings allow rule matched. Outer layers still decide.
    Allow,
    /// A settings ask rule matched; an external approval is required.
    Ask { rule: String },
    /// A settings deny rule or the read-only plan mode refused the call.
    Deny { rule: String, reason: String },
}

#[derive(Clone, Debug, Default)]
pub struct Policy {
    mode: Mode,
    allow: Vec<Rule>,
    ask: Vec<Rule>,
    deny: Vec<Rule>,
}

impl Policy {
    pub fn compile(
        mode: Mode,
        allow: &[String],
        ask: &[String],
        deny: &[String],
    ) -> Result<Self, String> {
        Ok(Self {
            mode,
            allow: compile_rules("allow", allow)?,
            ask: compile_rules("ask", ask)?,
            deny: compile_rules("deny", deny)?,
        })
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }

    pub fn is_empty(&self) -> bool {
        self.allow.is_empty() && self.ask.is_empty() && self.deny.is_empty()
    }

    pub fn describe(&self) -> String {
        format!(
            "mode={} allow={} ask={} deny={}",
            self.mode.as_str(),
            self.allow.len(),
            self.ask.len(),
            self.deny.len()
        )
    }

    pub fn check(&self, request: Option<&Request<'_>>, effect: Effect) -> Verdict {
        if let Some(request) = request {
            if let Some(rule) = self.deny.iter().find(|rule| rule.matches(request)) {
                return Verdict::Deny {
                    rule: rule.raw.clone(),
                    reason: format!("denied by settings permission rule '{}'", rule.raw),
                };
            }
        }

        if self.mode == Mode::Plan && !matches!(effect, Effect::Read | Effect::ModelInvoke) {
            return Verdict::Deny {
                rule: "defaultMode:plan".into(),
                reason: "plan mode is read-only; mutating actions are disabled".into(),
            };
        }

        let Some(request) = request else {
            return Verdict::Unrestricted;
        };

        if let Some(rule) = self.ask.iter().find(|rule| rule.matches(request)) {
            return Verdict::Ask {
                rule: rule.raw.clone(),
            };
        }

        let category = request.category();
        if self.allow.iter().any(|rule| rule.category == category) {
            return match self.allow.iter().find(|rule| rule.matches(request)) {
                Some(_) => Verdict::Allow,
                None => Verdict::Deny {
                    rule: format!("{} allowlist", category.as_str()),
                    reason: format!(
                        "settings allowlist does not include this {} action",
                        category.as_str()
                    ),
                },
            };
        }

        Verdict::Unrestricted
    }
}

fn compile_rules(section: &str, rules: &[String]) -> Result<Vec<Rule>, String> {
    rules.iter().map(|raw| compile_rule(section, raw)).collect()
}

fn compile_rule(section: &str, raw: &str) -> Result<Rule, String> {
    let text = raw.trim();
    if text.is_empty() {
        return Err(format!("permissions.{section} contains an empty rule"));
    }
    if text.chars().any(char::is_control) {
        return Err(format!(
            "permissions.{section} rule '{text}' contains control characters"
        ));
    }
    let open = text.find('(').ok_or_else(|| {
        format!("permissions.{section} rule '{text}' must look like Tool(pattern)")
    })?;
    if !text.ends_with(')') || open + 1 >= text.len() {
        return Err(format!(
            "permissions.{section} rule '{text}' must look like Tool(pattern)"
        ));
    }
    let tool = text[..open].trim().to_ascii_lowercase();
    let pattern = text[open + 1..text.len() - 1].trim();
    if pattern.is_empty() {
        return Err(format!(
            "permissions.{section} rule '{text}' has an empty pattern"
        ));
    }
    let category = match tool.as_str() {
        "read" | "list" => Category::Read,
        "write" | "edit" => Category::Write,
        "bash" | "command" => Category::Bash,
        other => {
            return Err(format!(
                "permissions.{section} rule '{text}' uses unsupported tool '{other}'; use Read, Write, Edit or Bash"
            ))
        }
    };
    let pattern = match category {
        Category::Read | Category::Write => normalize_rule_path(section, text, pattern)?,
        Category::Bash => normalize_command_rule(section, text, pattern)?,
    };
    Ok(Rule {
        category,
        pattern,
        raw: text.to_string(),
    })
}

fn normalize_rule_path(section: &str, raw: &str, pattern: &str) -> Result<String, String> {
    let mut normalized = pattern.replace('\\', "/");
    while let Some(rest) = normalized.strip_prefix("./") {
        normalized = rest.to_string();
    }
    let normalized = normalized.trim_end_matches('/').to_string();
    if normalized.is_empty() {
        return Err(format!(
            "permissions.{section} rule '{raw}' has an empty path"
        ));
    }
    if normalized.starts_with('/')
        || normalized.starts_with("//")
        || normalized
            .split('/')
            .next()
            .is_some_and(|first| first.len() >= 2 && first.as_bytes()[1] == b':')
    {
        return Err(format!(
            "permissions.{section} rule '{raw}' must be relative to the worktree"
        ));
    }
    if normalized
        .split('/')
        .any(|part| part == ".." || part.is_empty())
    {
        return Err(format!(
            "permissions.{section} rule '{raw}' must not contain '..' or empty path segments"
        ));
    }
    Ok(normalized)
}

fn normalize_command_rule(section: &str, raw: &str, pattern: &str) -> Result<String, String> {
    let pattern = pattern.split_whitespace().collect::<Vec<_>>().join(" ");
    if pattern.is_empty() || pattern.contains('\0') {
        return Err(format!(
            "permissions.{section} rule '{raw}' has an invalid command pattern"
        ));
    }
    Ok(pattern)
}

impl Rule {
    fn matches(&self, request: &Request<'_>) -> bool {
        match (self.category, request) {
            (Category::Read, Request::Read { path, directory }) => {
                path_matches(&self.pattern, path, *directory, true)
            }
            (Category::Write, Request::Write { path }) => {
                path_matches(&self.pattern, path, false, false)
            }
            (Category::Bash, Request::Command { line }) => command_matches(&self.pattern, line),
            _ => false,
        }
    }
}

fn normalize_path(path: &str) -> String {
    let mut normalized = path.replace('\\', "/");
    while let Some(rest) = normalized.strip_prefix("./") {
        normalized = rest.to_string();
    }
    normalized.trim_end_matches('/').to_string()
}

fn has_glob(pattern: &str) -> bool {
    pattern.contains('*') || pattern.contains('?')
}

/// `ancestor_rules` is used for deny rules: denying `secrets` also denies a
/// directory listing of `secrets` and reads below `secrets/...`.
fn path_matches(pattern: &str, path: &str, directory: bool, ancestor_rules: bool) -> bool {
    let path = normalize_path(path);
    if path.is_empty() {
        return false;
    }
    if glob_match(pattern, &path) {
        return true;
    }
    if !has_glob(pattern) && (path == pattern || path.starts_with(&format!("{pattern}/"))) {
        return true;
    }
    if directory {
        if glob_match(pattern, &format!("{path}/x")) {
            return true;
        }
        if ancestor_rules && !has_glob(pattern) && pattern.starts_with(&format!("{path}/")) {
            return true;
        }
    }
    false
}

fn command_matches(pattern: &str, line: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    // Claude accepts both `Bash(cargo test:*)` and `Bash(cargo test *)` as a
    // prefix grant. Match the command itself too, so `cargo test` is covered by
    // a rule that names its arguments.
    if let Some(prefix) = pattern
        .strip_suffix(":*")
        .or_else(|| pattern.strip_suffix(" *"))
    {
        let prefix = prefix.trim_end();
        return line == prefix
            || line.starts_with(&format!("{prefix} "))
            || line.starts_with(&format!("{prefix}:"));
    }
    glob_match(pattern, line)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(mode: Mode, allow: &[&str], ask: &[&str], deny: &[&str]) -> Policy {
        let owned = |items: &[&str]| {
            items
                .iter()
                .map(|item| item.to_string())
                .collect::<Vec<_>>()
        };
        Policy::compile(mode, &owned(allow), &owned(ask), &owned(deny)).unwrap()
    }

    fn read(path: &str) -> Request<'_> {
        Request::Read {
            path,
            directory: false,
        }
    }

    fn write(path: &str) -> Request<'_> {
        Request::Write { path }
    }

    fn command(line: &str) -> Request<'static> {
        Request::Command {
            line: line.to_string(),
        }
    }

    #[test]
    fn deny_wins_over_ask_and_allow() {
        let policy = policy(
            Mode::Default,
            &["Read(./src/**)"],
            &["Read(./src/private/**)"],
            &["Read(./src/private/key.txt)"],
        );
        assert!(matches!(
            policy.check(Some(&read("src/private/key.txt")), Effect::Read),
            Verdict::Deny { .. }
        ));
        assert!(matches!(
            policy.check(Some(&read("src/private/notes.txt")), Effect::Read),
            Verdict::Ask { .. }
        ));
        assert_eq!(
            policy.check(Some(&read("src/main.rs")), Effect::Read),
            Verdict::Allow
        );
    }

    #[test]
    fn allow_is_a_per_category_allowlist() {
        let policy = policy(Mode::Default, &["Bash(cargo test *)"], &[], &[]);
        assert_eq!(
            policy.check(Some(&command("cargo test --workspace")), Effect::Control),
            Verdict::Allow
        );
        assert_eq!(
            policy.check(Some(&command("cargo test")), Effect::Control),
            Verdict::Allow
        );
        assert!(matches!(
            policy.check(Some(&command("cargo fmt --check")), Effect::Control),
            Verdict::Deny { .. }
        ));
        // A Bash allowlist does not restrict reads.
        assert_eq!(
            policy.check(Some(&read("README.md")), Effect::Read),
            Verdict::Unrestricted
        );
    }

    #[test]
    fn plan_mode_blocks_mutations_but_not_reads() {
        let plan = policy(Mode::Plan, &[], &[], &[]);
        assert!(matches!(
            plan.check(Some(&write("src/main.rs")), Effect::LocalWrite),
            Verdict::Deny { .. }
        ));
        assert!(matches!(
            plan.check(Some(&command("cargo test")), Effect::Control),
            Verdict::Deny { .. }
        ));
        assert_eq!(
            plan.check(Some(&read("src/main.rs")), Effect::Read),
            Verdict::Unrestricted
        );
        assert!(matches!(
            plan.check(None, Effect::Control),
            Verdict::Deny { .. }
        ));
        assert!(matches!(
            plan.check(None, Effect::ExternalWrite),
            Verdict::Deny { .. }
        ));
        assert!(matches!(
            plan.check(None, Effect::Irreversible),
            Verdict::Deny { .. }
        ));
        assert_eq!(plan.check(None, Effect::ModelInvoke), Verdict::Unrestricted);
    }

    #[test]
    fn deny_reads_cover_directories_and_subtrees() {
        let policy = policy(Mode::Default, &[], &[], &["Read(./secrets)"]);
        assert!(matches!(
            policy.check(Some(&read("secrets")), Effect::Read),
            Verdict::Deny { .. }
        ));
        assert!(matches!(
            policy.check(Some(&read("secrets/token.txt")), Effect::Read),
            Verdict::Deny { .. }
        ));
        let listing = Request::Read {
            path: "secrets",
            directory: true,
        };
        assert!(matches!(
            policy.check(Some(&listing), Effect::Read),
            Verdict::Deny { .. }
        ));
        assert_eq!(
            policy.check(Some(&read("src/main.rs")), Effect::Read),
            Verdict::Unrestricted
        );
    }

    #[test]
    fn claude_style_command_prefixes_match() {
        let policy = policy(
            Mode::Default,
            &["Bash(cargo test:*)", "Bash(git status *)"],
            &[],
            &[],
        );
        assert_eq!(
            policy.check(Some(&command("cargo test --all")), Effect::Control),
            Verdict::Allow
        );
        assert_eq!(
            policy.check(Some(&command("git status --short")), Effect::Control),
            Verdict::Allow
        );
        assert!(matches!(
            policy.check(Some(&command("git status")), Effect::Control),
            Verdict::Allow
        ));
        assert!(matches!(
            policy.check(Some(&command("git push origin main")), Effect::Control),
            Verdict::Deny { .. }
        ));
    }

    #[test]
    fn malformed_and_bypass_rules_fail_closed() {
        assert!(Mode::parse("bypassPermissions")
            .unwrap_err()
            .contains("never bypasses"));
        assert!(Policy::compile(Mode::Default, &["Read(./.env".into()], &[], &[]).is_err());
        assert!(Policy::compile(Mode::Default, &["Fetch(https://x)".into()], &[], &[]).is_err());
        assert!(Policy::compile(Mode::Default, &["Read(/etc/passwd)".into()], &[], &[]).is_err());
        assert!(Policy::compile(Mode::Default, &["Read(./../outside)".into()], &[], &[]).is_err());
        assert!(Policy::compile(Mode::Default, &["Bash()".into()], &[], &[]).is_err());
    }

    #[test]
    fn accepting_edits_does_not_grant_writes_by_itself() {
        let policy = policy(Mode::AcceptEdits, &[], &[], &[]);
        assert_eq!(
            policy.check(Some(&write("src/main.rs")), Effect::LocalWrite),
            Verdict::Unrestricted
        );
    }
}
