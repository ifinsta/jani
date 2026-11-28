//! Delivery: the difference between "the model says it is done" and "the work
//! is done and someone other than the model checked".
//!
//! `jani deliver "TASK" --accept "python3 -m unittest"` runs one task end to end:
//!
//! 1. **Isolate.** The work happens on its own branch in a git worktree, so the
//!    operator's checkout is never touched. Without git, it happens in place.
//! 2. **Work.** The agent runs as usual (same policy, sandbox, consent, journal).
//! 3. **Verify, independently.** The *host* runs each `--accept` command after
//!    the agent finishes. The model cannot run, skip, edit or report them: they
//!    are not a tool, and each one is journaled (`acceptance.run`) with its exit
//!    status. "Verified" means those commands exited 0, nothing else.
//! 4. **Repair.** A failing check is fed back (command, exit code, output tail)
//!    for up to `--repair-rounds` more turns of the *same* conversation.
//! 5. **Commit.** Changes inside the operator's write scope are committed on the
//!    branch (hooks disabled: repository hooks would run unconfined on the host),
//!    marked with the outcome. Optionally pushed (`--push`) and opened as a pull
//!    request (`--pr`, via `gh`).
//! 6. **Report.** `<state>/deliveries/<id>/report.md` and `report.json`: outcome,
//!    every check on every round, the diff, files that look like tests (an agent
//!    can "pass" by weakening them), tokens, and whether the audit chain verifies.
//!
//! Outcomes: `verified` (checks declared, all passed), `unverified` (none
//! declared; the agent's word is all there is), `failed`.

use crate::agent::Agent;
use crate::config::{Config, JResult};
use crate::consent::SharedConsent;
use crate::exec::{self, ExecGrant};
use crate::execpolicy;
use crate::identity;
use crate::journal::{self, decision, reason, EventInput, Journal};
use crate::process::execute;
use crate::skills::Catalog;
use crate::tools::action;
use crate::types::{sha256_hex, Classification};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

mod completion;
mod queued_commit;
mod worktree;

const CHECK_TIMEOUT_SECS: u64 = 300;
const FEEDBACK_TAIL_BYTES: usize = 4000;

pub(crate) fn saved_queue_finalization(
    state: &Path,
    delivery_id: &str,
    generation: u64,
) -> JResult<bool> {
    let path = completion::path(&state.join("deliveries").join(delivery_id), generation);
    match std::fs::symlink_metadata(&path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!(
            "cannot inspect finalization for {delivery_id}: {error}"
        )),
    }
}

pub(crate) fn queue_finalization_intent(
    state: &Path,
    delivery_id: &str,
    generation: u64,
) -> JResult<bool> {
    completion::intent_exists(state, delivery_id, generation)
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Verified,
    Unverified,
    Failed,
    /// The agent asked a question nobody could answer yet. Nothing is committed or
    /// checked; the worktree and conversation are kept for when someone does.
    Blocked,
}

impl Outcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Verified => "verified",
            Self::Unverified => "unverified",
            Self::Failed => "failed",
            Self::Blocked => "blocked",
        }
    }

    fn reason(self) -> &'static str {
        match self {
            Self::Verified => reason::DELIVERY_VERIFIED,
            Self::Unverified => reason::DELIVERY_UNVERIFIED,
            Self::Failed => reason::DELIVERY_FAILED,
            Self::Blocked => reason::NEEDS_INPUT,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Spec {
    pub prompt: String,
    /// Commands the host runs after the agent finishes, each as argv.
    pub accept: Vec<Vec<String>>,
    pub repair_rounds: usize,
    pub branch: Option<String>,
    pub use_worktree: bool,
    pub commit: bool,
    pub push_remote: Option<String>,
    pub open_pr: bool,
    /// An id to reuse (a queued task's), otherwise one is made.
    pub id: Option<String>,
    /// Continue an interrupted delivery: reuse its worktree and saved conversation.
    pub resume: bool,
    /// A queue generation survives lease loss and changes on a new attempt requested by the operator.
    pub queue_generation: Option<u64>,
    /// Extra context for the agent, such as the operator's answers to its questions.
    pub context: Option<String>,
    /// Set from elsewhere (the work queue's heartbeat) to stop at the next safe point.
    pub cancel: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
}

impl Spec {
    pub fn from_config(cfg: &Config) -> JResult<Self> {
        let prompt = cfg
            .positional
            .first()
            .cloned()
            .or_else(|| cfg.prompt.clone())
            .ok_or("deliver needs the task, e.g. jani deliver \"fix the failing test\" --accept \"cargo test\"")?;
        let accept = cfg
            .accept
            .iter()
            .map(|line| split_command(line))
            .collect::<JResult<Vec<_>>>()?;
        Ok(Self {
            prompt,
            accept,
            repair_rounds: cfg.repair_rounds,
            branch: cfg.branch.clone(),
            use_worktree: !cfg.no_worktree,
            commit: !cfg.no_commit,
            push_remote: cfg.push_remote.clone(),
            open_pr: cfg.open_pr,
            id: None,
            resume: false,
            queue_generation: None,
            context: None,
            cancel: None,
        })
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Check {
    pub round: usize,
    pub command: String,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub passed: bool,
    pub millis: u128,
    #[serde(skip)]
    pub output_tail: String,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Delivery {
    pub id: String,
    pub outcome: Outcome,
    pub rounds: usize,
    pub checks: Vec<Check>,
    pub answer: String,
    pub error: Option<String>,
    pub branch: Option<String>,
    pub commit: Option<String>,
    pub workdir: PathBuf,
    pub report_dir: PathBuf,
    pub changed_files: Vec<String>,
    pub test_files_changed: Vec<String>,
    pub pushed: bool,
    pub pr: Option<String>,
    /// What the agent is waiting to be told, when `outcome` is `Blocked`.
    pub question: Option<(String, Vec<String>)>,
    pub agent_task_id: String,
    pub tokens: (u64, u64),
}

/// Split a command line into argv. Quotes group words; there is no shell, so no
/// pipes, `&&`, redirection or expansion. A lone backslash is literal (Windows paths).
pub fn split_command(line: &str) -> JResult<Vec<String>> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut started = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), '\\') if matches!(chars.peek(), Some('"' | '\\')) => {
                current.push(chars.next().unwrap_or('\\'))
            }
            (Some(_), c) => current.push(c),
            (None, '"' | '\'') => {
                quote = Some(c);
                started = true;
            }
            (None, c) if c.is_whitespace() => {
                if started || !current.is_empty() {
                    args.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            (None, c) => current.push(c),
        }
    }
    if quote.is_some() {
        return Err(format!("unterminated quote in command: {line}"));
    }
    if started || !current.is_empty() {
        args.push(current);
    }
    if args.is_empty() {
        return Err("an empty command cannot be an acceptance check".into());
    }
    Ok(args)
}

fn git_command(hooks_off: &Path, args: &[&str]) -> Command {
    let mut cmd = Command::new("git");
    for variable in [
        "GIT_DIR",
        "GIT_COMMON_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_NAMESPACE",
        "GIT_CEILING_DIRECTORIES",
        "GIT_PREFIX",
        "GIT_LITERAL_PATHSPECS",
        "GIT_GLOB_PATHSPECS",
        "GIT_NOGLOB_PATHSPECS",
        "GIT_ICASE_PATHSPECS",
    ] {
        cmd.env_remove(variable);
    }
    // Hooks and fsmonitor run arbitrary repository-supplied programs on the host.
    cmd.arg("-c")
        .arg(format!("core.hooksPath={}", hooks_off.display()))
        .arg("-c")
        .arg("core.fsmonitor=false")
        .args(args);
    cmd
}

fn git(cwd: &Path, hooks_off: &Path, args: &[&str]) -> JResult<crate::process::ProcessOutput> {
    execute(&mut git_command(hooks_off, args), cwd, 120)
}

fn git_ok(cwd: &Path, hooks_off: &Path, args: &[&str]) -> JResult<String> {
    git_result(git(cwd, hooks_off, args)?, args)
}

fn git_result(out: crate::process::ProcessOutput, args: &[&str]) -> JResult<String> {
    if out.timed_out || out.code != Some(0) {
        return Err(format!(
            "git {} failed: {}",
            args.join(" "),
            out.stderr.trim()
        ));
    }
    Ok(out.stdout)
}

fn tail(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let mut start = text.len() - max;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    format!("…{}", &text[start..])
}

fn looks_like_test(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    lower
        .split(['/', '\\'])
        .any(|part| part.contains("test") || part.contains("spec") || part == "tests")
}

/// One journal line about a delivery, described once so call sites stay readable.
struct Event<'a> {
    task_id: &'a str,
    run_id: &'a str,
    action: &'a str,
    resource: &'a str,
    decision: &'a str,
    reason: &'a str,
}

fn record(journal: &mut Journal, cfg: &Config, event: Event<'_>) -> JResult<()> {
    let who = identity::load(cfg)?;
    journal
        .record(EventInput {
            tenant_id: &who.tenant,
            task_id: event.task_id,
            run_id: event.run_id,
            invocation_id: "delivery",
            actor_id: &who.principal,
            worker_id: &who.principal,
            action: event.action,
            resource_digest: &sha256_hex(event.resource.as_bytes()),
            decision: event.decision,
            reason_code: event.reason,
            policy_revision: journal::RUNTIME_REVISION,
            approval_id: None,
            sandbox_receipt_id: None,
            data_classification: Classification::Internal,
        })
        .map(|_| ())
}

/// Run one acceptance command on the host, in the sandbox the session uses.
pub(crate) fn run_check(cfg: &Config, round: usize, argv: &[String]) -> Check {
    let command = argv.join(" ");
    let started = Instant::now();
    let finish =
        |code: Option<i32>, timed_out: bool, output: String, error: Option<String>| Check {
            round,
            command: command.clone(),
            exit_code: code,
            timed_out,
            passed: code == Some(0) && !timed_out && error.is_none(),
            millis: started.elapsed().as_millis(),
            output_tail: tail(&output, FEEDBACK_TAIL_BYTES),
            error,
        };
    // These are the operator's commands, not the model's: allowed for this call only.
    let mut host = cfg.clone();
    host.allow_exec = true;
    let grant = ExecGrant {
        preauthorized: true,
        extra_writable: execpolicy::build_outputs(argv, &cfg.root),
        ..Default::default()
    };
    match exec::run_confined(
        &host,
        argv,
        CHECK_TIMEOUT_SECS,
        execpolicy::default_profile(cfg),
        cfg.sandbox_net,
        &grant,
        None,
    ) {
        Ok((value, _receipt)) => {
            let text = format!(
                "{}{}",
                value["stdout"].as_str().unwrap_or(""),
                value["stderr"].as_str().unwrap_or("")
            );
            finish(
                value["exit_code"].as_i64().map(|c| c as i32),
                value["timed_out"].as_bool().unwrap_or(false),
                text,
                None,
            )
        }
        Err(error) => finish(None, false, String::new(), Some(error)),
    }
}

/// What the agent is told when picking an interrupted delivery back up.
fn resumed_prompt(spec: &Spec, had_history: bool) -> String {
    let context = spec
        .context
        .as_deref()
        .map(|c| format!("\n\n{c}"))
        .unwrap_or_default();
    if had_history {
        format!(
            "[jani runtime] The previous attempt at this task was interrupted (the worker stopped). Your conversation is restored. Check what is already in the worktree with git status and git diff before redoing anything, then continue and finish the task.{context}"
        )
    } else {
        format!(
            "[jani runtime] A previous attempt at this task was interrupted and its conversation was not saved. Earlier changes may already be in the worktree: check git status and git diff first, do not redo finished work.\n\n{}",
            task_prompt(spec)
        )
    }
}

fn task_prompt(spec: &Spec) -> String {
    let context = spec
        .context
        .as_deref()
        .map(|c| format!("\n\n{c}"))
        .unwrap_or_default();
    format!("{}{context}", task_prompt_base(spec))
}

fn task_prompt_base(spec: &Spec) -> String {
    if spec.accept.is_empty() {
        return format!(
            "{}\n\nWhen you finish, report what you changed and what you verified. No acceptance checks were declared, so say plainly what is unverified.",
            spec.prompt
        );
    }
    let checks = spec
        .accept
        .iter()
        .map(|argv| format!("- `{}`", argv.join(" ")))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "{}\n\nAcceptance: after you finish, the operator runs these checks themselves. The work is accepted only if every one exits 0:\n{checks}\nRun them yourself first and fix what they show. Do not change the checks or the tests to make them pass; fix the code. Finish with a short report of what you changed and what you verified.",
        spec.prompt
    )
}

fn repair_prompt(failed: &[&Check]) -> String {
    let mut text = String::from(
        "The operator ran the acceptance checks and they did not all pass. The work is not accepted yet.\n",
    );
    for check in failed {
        let status = match (check.timed_out, check.exit_code, check.error.as_ref()) {
            (_, _, Some(error)) => format!("could not run: {error}"),
            (true, _, _) => "timed out".to_string(),
            (_, Some(code), _) => format!("exited {code}"),
            _ => "was killed by a signal".to_string(),
        };
        text.push_str(&format!(
            "\n`{}` {status}. Output (tail):\n{}\n",
            check.command, check.output_tail
        ));
    }
    text.push_str("\nFix the code, not the checks or the tests, run the checks yourself, and report what you changed.");
    text
}

fn ensure_active(cancel: &AtomicBool) -> JResult<()> {
    if cancel.load(Ordering::SeqCst) {
        Err("delivery cancelled".into())
    } else {
        Ok(())
    }
}

pub fn deliver(
    cfg: &Config,
    catalog: &Catalog,
    spec: &Spec,
    consent: Option<SharedConsent>,
) -> JResult<Delivery> {
    deliver_with_guard(cfg, catalog, spec, consent, &|| Ok(()))
}

pub(crate) fn deliver_with_guard(
    cfg: &Config,
    catalog: &Catalog,
    spec: &Spec,
    consent: Option<SharedConsent>,
    check_claim: &dyn Fn() -> JResult<()>,
) -> JResult<Delivery> {
    let never_cancelled = AtomicBool::new(false);
    let cancel = spec.cancel.as_deref().unwrap_or(&never_cancelled);
    let check_finalization = || {
        ensure_active(cancel)?;
        check_claim()
    };
    ensure_active(cancel)?;
    let id = spec
        .id
        .clone()
        .unwrap_or_else(|| format!("dlv-{}", crate::types::new_id("d").trim_start_matches("d-")));
    let report_dir = cfg.state_dir.join("deliveries").join(&id);
    let saved = spec
        .queue_generation
        .map(|generation| completion::load(cfg, &report_dir, &id, generation, spec.resume))
        .transpose()?
        .flatten();
    if saved.as_ref().is_some_and(completion::Saved::linked)
        && (!report_dir.join("tree").is_dir() || !report_dir.join("workspace.json").is_file())
    {
        return Err(
            "saved delivery has lost its original worktree; reconcile it before retrying".into(),
        );
    }
    std::fs::create_dir_all(&report_dir)
        .map_err(|e| format!("cannot create {}: {e}", report_dir.display()))?;
    ensure_active(cancel)?;
    let hooks_off = report_dir.join("no-hooks");
    std::fs::create_dir_all(&hooks_off).map_err(|e| e.to_string())?;
    let branch = spec
        .branch
        .clone()
        .unwrap_or_else(|| format!("jani/{}", id.trim_start_matches("dlv-")));
    let workspace = worktree::prepare(
        &cfg.root,
        &report_dir,
        &hooks_off,
        branch,
        saved
            .as_ref()
            .map_or(spec.use_worktree, completion::Saved::linked),
        spec.resume || saved.is_some(),
    )?;
    if let Some(saved) = saved {
        return completion::finish(
            cfg,
            spec,
            &workspace,
            &report_dir,
            &hooks_off,
            saved,
            &check_finalization,
        );
    }
    let binding = spec
        .queue_generation
        .map(|_| completion::worker_binding(cfg, spec).map(|(_, digest)| digest))
        .transpose()?;

    let mut run_cfg = cfg.clone();
    run_cfg.root = workspace.root.clone();
    run_cfg.task_id = Some(format!("task-{}", id.trim_start_matches("dlv-")));
    // An interrupted delivery comes back to its saved conversation (the task store's
    // checkpoint, fenced by epoch). If it never got as far as a checkpoint, start over.
    let mut agent = if spec.resume {
        Agent::recover(&run_cfg, catalog)?
    } else {
        Agent::new(&run_cfg, catalog)?
    };
    if let Some(consent) = consent {
        agent.set_consent(consent);
    }
    let agent_task = agent.task_id().to_string();
    let mut journal = Journal::open(&cfg.state_dir)?;

    let mut checks: Vec<Check> = Vec::new();
    let mut answer = String::new();
    let mut error: Option<String> = None;
    let mut rounds = 0;
    let mut prompt = if spec.resume {
        resumed_prompt(spec, agent.has_history())
    } else {
        task_prompt(spec)
    };
    let mut passed = false;
    let mut question: Option<(String, Vec<String>)> = None;
    for round in 0..=spec.repair_rounds {
        rounds = round + 1;
        ensure_active(cancel)?;
        match agent.run_turn_cancellable(&prompt, cancel) {
            Ok(text) => answer = text,
            Err(e) => {
                error = Some(format!("the agent's turn failed: {e}"));
                break;
            }
        }
        if let Some(q) = agent.take_question() {
            question = Some((q.text, q.options));
            break;
        }
        if spec.accept.is_empty() {
            break;
        }
        let mut results = Vec::with_capacity(spec.accept.len());
        for argv in &spec.accept {
            ensure_active(cancel)?;
            let check = run_check(&run_cfg, round + 1, argv);
            record(
                &mut journal,
                cfg,
                Event {
                    task_id: &agent_task,
                    run_id: &id,
                    action: action::ACCEPTANCE_RUN,
                    resource: &check.command,
                    decision: if check.passed {
                        decision::EXECUTED
                    } else {
                        decision::FAILED
                    },
                    reason: if check.passed {
                        reason::ACCEPTANCE_PASSED
                    } else {
                        reason::ACCEPTANCE_FAILED
                    },
                },
            )?;
            results.push(check);
        }
        let failed: Vec<&Check> = results.iter().filter(|c| !c.passed).collect();
        passed = failed.is_empty();
        if !passed && round < spec.repair_rounds {
            prompt = repair_prompt(&failed);
        }
        checks.extend(results);
        if passed {
            break;
        }
    }

    ensure_active(cancel)?;
    let outcome = if question.is_some() && error.is_none() {
        Outcome::Blocked
    } else if error.is_some() {
        Outcome::Failed
    } else if spec.accept.is_empty() {
        Outcome::Unverified
    } else if passed {
        Outcome::Verified
    } else {
        Outcome::Failed
    };

    // What changed, and whether the change touches anything that checks it.
    let hooks = &hooks_off;
    let changes = if workspace.in_git {
        workspace.verify_scope(hooks)?;
        worktree::scoped_changes(&run_cfg.root, hooks, &run_cfg.write_paths)?
    } else {
        Vec::new()
    };
    let should_commit =
        workspace.in_git && spec.commit && !changes.is_empty() && outcome != Outcome::Blocked;
    let first_line = spec.prompt.lines().next().unwrap_or("task");
    let subject: String = first_line.chars().take(72).collect();
    let message = format!(
        "jani: {subject}\n\nOutcome: {} ({} acceptance check(s), {} round(s))\nDelivery: {id}\nAcceptance was run by the host, not by the model.",
        outcome.as_str().to_uppercase(), spec.accept.len(), rounds,
    );
    if let Some(binding) = binding.filter(|_| workspace.branch.is_some() || !should_commit) {
        let commit = if should_commit {
            ensure_active(cancel)?;
            worktree::check_implicit_deletions(&run_cfg.root, hooks, &changes)?;
            let root = workspace
                .linked_root()
                .ok_or("queued commit requires a linked worktree")?;
            queued_commit::prepare(root, hooks, &report_dir, &changes, &message, cancel)?
        } else {
            None
        };
        let changed_files: Vec<_> = if should_commit && commit.is_none() {
            Vec::new()
        } else {
            changes.into_iter().map(|change| change.path).collect()
        };
        let test_files_changed = changed_files
            .iter()
            .filter(|path| looks_like_test(path))
            .cloned()
            .collect();
        let budget = agent.budget().unwrap_or(Value::Null);
        let delivery = Delivery {
            id,
            outcome,
            rounds,
            checks,
            answer,
            error,
            commit: None,
            branch: workspace.branch.clone(),
            workdir: workspace.root.clone(),
            report_dir: report_dir.clone(),
            changed_files,
            test_files_changed,
            pushed: false,
            pr: None,
            question,
            agent_task_id: agent_task,
            tokens: (
                budget["inputTokens"].as_u64().unwrap_or(0),
                budget["outputTokens"].as_u64().unwrap_or(0),
            ),
        };
        check_finalization()?;
        let saved = completion::seal(cfg, spec, &workspace, hooks, delivery, commit, &binding)?;
        return completion::finish(
            cfg,
            spec,
            &workspace,
            &report_dir,
            hooks,
            saved,
            &check_finalization,
        );
    }
    let (changed_files, commit) = if workspace.in_git {
        let mut commit_id = None;
        if should_commit {
            ensure_active(cancel)?;
            worktree::check_implicit_deletions(&run_cfg.root, hooks, &changes)?;
            let untracked: Vec<String> = changes
                .iter()
                .filter(|change| change.untracked)
                .map(worktree::Change::pathspec)
                .collect();
            if !untracked.is_empty() {
                let mut add = vec!["add", "--"];
                add.extend(untracked.iter().map(String::as_str));
                git_ok(&run_cfg.root, hooks, &add)?;
            }
            ensure_active(cancel)?;
            let paths: Vec<String> = changes.iter().map(worktree::Change::pathspec).collect();
            let mut commit_args = vec![
                "-c",
                "user.name=Jani",
                "-c",
                "user.email=jani@localhost",
                "commit",
                "--only",
                "--no-verify",
                "--no-gpg-sign",
                "-m",
                &message,
                "--",
            ];
            commit_args.extend(paths.iter().map(String::as_str));
            git_ok(&run_cfg.root, hooks, &commit_args)?;
            commit_id = Some(
                git_ok(&run_cfg.root, hooks, &["rev-parse", "HEAD"])?
                    .trim()
                    .to_string(),
            );
        }
        (
            changes.into_iter().map(|change| change.path).collect(),
            commit_id,
        )
    } else {
        (Vec::new(), None)
    };
    let test_files_changed: Vec<String> = changed_files
        .iter()
        .filter(|p| looks_like_test(p))
        .cloned()
        .collect();

    let mut pushed = false;
    let mut pr = None;
    if let (Some(remote), Some(branch), Some(_)) = (&spec.push_remote, &workspace.branch, &commit) {
        ensure_active(cancel)?;
        git_ok(&run_cfg.root, hooks, &["push", "-u", remote, branch])?;
        pushed = true;
        if spec.open_pr {
            ensure_active(cancel)?;
            let title = format!("jani: {}", spec.prompt.lines().next().unwrap_or("task"));
            let body = format!(
                "Outcome: **{}**. Acceptance was run by the host, not the model. See the delivery report in the state directory (`deliveries/{id}/report.md`).",
                outcome.as_str().to_uppercase()
            );
            let mut gh = Command::new("gh");
            gh.args([
                "pr", "create", "--head", branch, "--title", &title, "--body", &body,
            ]);
            match execute(&mut gh, &run_cfg.root, 120) {
                Ok(out) if out.code == Some(0) => pr = Some(out.stdout.trim().to_string()),
                Ok(out) => {
                    error = error.or(Some(format!("gh pr create failed: {}", out.stderr.trim())))
                }
                Err(e) => error = error.or(Some(format!("gh pr create could not run: {e}"))),
            }
        }
    }

    let budget = agent.budget().unwrap_or(Value::Null);
    let tokens = (
        budget["inputTokens"].as_u64().unwrap_or(0),
        budget["outputTokens"].as_u64().unwrap_or(0),
    );
    let chain_ok = journal::verify(&cfg.state_dir)
        .map(|v| v["ok"] == true)
        .unwrap_or(false);

    ensure_active(cancel)?;
    record(
        &mut journal,
        cfg,
        Event {
            task_id: &agent_task,
            run_id: &id,
            action: action::DELIVERY_RUN,
            resource: &spec.prompt,
            decision: if outcome == Outcome::Failed {
                decision::FAILED
            } else {
                decision::COMPLETED
            },
            reason: outcome.reason(),
        },
    )?;

    let delivery = Delivery {
        id: id.clone(),
        outcome,
        rounds,
        checks,
        answer,
        error,
        branch: workspace.branch.clone(),
        commit,
        workdir: workspace.root.clone(),
        report_dir: report_dir.clone(),
        changed_files,
        test_files_changed,
        pushed,
        pr,
        question,
        agent_task_id: agent_task,
        tokens,
    };
    ensure_active(cancel)?;
    write_report(&delivery, spec, workspace.base.as_deref(), chain_ok, hooks)?;
    Ok(delivery)
}

fn write_report(
    d: &Delivery,
    spec: &Spec,
    base: Option<&str>,
    chain_ok: bool,
    hooks: &Path,
) -> JResult<()> {
    let diff_stat = match (base, d.commit.as_ref()) {
        (Some(base), Some(commit)) => git(&d.workdir, hooks, &["diff", "--stat", base, commit])
            .map(|o| o.stdout)
            .unwrap_or_default(),
        _ => String::new(),
    };
    let headline = match d.outcome {
        Outcome::Verified => "VERIFIED: every acceptance check the host ran passed.",
        Outcome::Unverified => "UNVERIFIED: no acceptance checks were declared. The agent's report below is its own word.",
        Outcome::Failed => "FAILED: the work was not accepted.",
        Outcome::Blocked => "BLOCKED: the agent asked a question and is waiting for an answer. Nothing was checked or committed; the worktree and conversation are kept.",
    };
    let mut md = format!("# Delivery {}\n\n**{headline}**\n\n", d.id);
    md.push_str(&format!("Task: {}\n\n", spec.prompt));
    if d.branch.is_some() && base.is_none() {
        md.push_str("The original comparison commit is unknown.\n\n");
    }
    if let Some(error) = &d.error {
        md.push_str(&format!("Problem: {error}\n\n"));
    }
    if let Some((text, options)) = &d.question {
        md.push_str(&format!("**The agent asks:** {text}\n\n"));
        for (i, option) in options.iter().enumerate() {
            md.push_str(&format!("{}. {option}\n", i + 1));
        }
        md.push('\n');
    }
    md.push_str("## Acceptance\n\n");
    if d.checks.is_empty() {
        md.push_str("None declared.\n\n");
    } else {
        md.push_str("| Round | Command | Result | Time |\n|---|---|---|---|\n");
        for c in &d.checks {
            let result = match (&c.error, c.timed_out, c.exit_code) {
                (Some(e), _, _) => format!("could not run: {e}"),
                (_, true, _) => "timed out".into(),
                (_, _, Some(0)) => "passed".into(),
                (_, _, Some(code)) => format!("failed (exit {code})"),
                _ => "killed".into(),
            };
            md.push_str(&format!(
                "| {} | `{}` | {} | {} ms |\n",
                c.round, c.command, result, c.millis
            ));
        }
        md.push_str("\nRun by the host after the agent finished; the model cannot run, skip or edit them.\n\n");
    }
    md.push_str("## Change\n\n");
    if let Some(branch) = &d.branch {
        md.push_str(&format!("Branch `{branch}`"));
        if let Some(commit) = &d.commit {
            md.push_str(&format!(", commit `{commit}`"));
        }
        md.push_str(&format!(", worktree `{}`.\n\n", d.workdir.display()));
    }
    if d.changed_files.is_empty() {
        md.push_str("No files changed inside the write scope.\n\n");
    } else {
        md.push_str(&format!("```\n{}\n```\n\n", d.changed_files.join("\n")));
        if !diff_stat.trim().is_empty() {
            md.push_str(&format!("```\n{}```\n\n", diff_stat));
        }
    }
    if !d.test_files_changed.is_empty() {
        md.push_str(&format!(
            "**Review these: the agent changed files that look like tests**, which can make a check pass by weakening it:\n\n{}\n\n",
            d.test_files_changed.iter().map(|f| format!("- `{f}`")).collect::<Vec<_>>().join("\n")
        ));
    }
    if d.pushed {
        md.push_str("Pushed.\n\n");
    }
    if let Some(pr) = &d.pr {
        md.push_str(&format!("Pull request: {pr}\n\n"));
    }
    md.push_str(&format!(
        "## Agent's report\n\n{}\n\n## Evidence\n\nRounds: {}. Tokens: {} in / {} out. Audit chain: {}. Agent task: `{}`.\n",
        if d.answer.trim().is_empty() { "(none)" } else { d.answer.trim() },
        d.rounds,
        d.tokens.0,
        d.tokens.1,
        if chain_ok { "verified" } else { "NOT VERIFIED" },
        d.agent_task_id,
    ));
    completion::atomic_write(&d.report_dir.join("report.md"), md.as_bytes())?;
    let json_report = json!({
        "id": d.id,
        "outcome": d.outcome.as_str(),
        "rounds": d.rounds,
        "branch": d.branch,
        "baseCommit": base,
        "commit": d.commit,
        "worktree": d.workdir,
        "changedFiles": d.changed_files,
        "testFilesChanged": d.test_files_changed,
        "pushed": d.pushed,
        "pullRequest": d.pr,
        "error": d.error,
        "question": d.question.as_ref().map(|(q, o)| json!({"text": q, "options": o})),
        "agentTask": d.agent_task_id,
        "tokens": {"input": d.tokens.0, "output": d.tokens.1},
        "auditChainVerified": chain_ok,
        "checks": d.checks.iter().map(|c| json!({
            "round": c.round, "command": c.command, "passed": c.passed,
            "exitCode": c.exit_code, "timedOut": c.timed_out, "millis": c.millis,
            "error": c.error,
        })).collect::<Vec<_>>(),
    });
    completion::atomic_write(
        &d.report_dir.join("report.json"),
        &serde_json::to_vec_pretty(&json_report).map_err(|e| e.to_string())?,
    )?;
    Ok(())
}

impl Delivery {
    pub fn summary(&self) -> Value {
        json!({
            "id": self.id,
            "outcome": self.outcome.as_str(),
            "rounds": self.rounds,
            "branch": self.branch,
            "commit": self.commit,
            "worktree": self.workdir,
            "pushed": self.pushed,
            "pullRequest": self.pr,
            "checks": self.checks.iter().map(|c| json!({"round": c.round, "command": c.command, "passed": c.passed})).collect::<Vec<_>>(),
            "testFilesChanged": self.test_files_changed,
            "report": self.report_dir.join("report.md"),
            "error": self.error,
            "question": self.question.as_ref().map(|(q, o)| json!({"text": q, "options": o})),
        })
    }
}

/// `jani deliver`
pub fn command(cfg: &Config, catalog: &Catalog) -> JResult<()> {
    let spec = Spec::from_config(cfg)?;
    let consent: Option<SharedConsent> = if crate::consent::TerminalConsent::available() {
        Some(std::sync::Arc::new(crate::consent::TerminalConsent))
    } else {
        None
    };
    let delivery = deliver(cfg, catalog, &spec, consent)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&delivery.summary()).map_err(|e| e.to_string())?
    );
    match delivery.outcome {
        Outcome::Verified => Ok(()),
        Outcome::Unverified => {
            eprintln!("jani: UNVERIFIED: no --accept checks were declared, so nothing but the agent's word says the task is done");
            Ok(())
        }
        Outcome::Blocked => Err(
            "blocked: the agent asked a question nobody could answer. Run it through `jani work` to answer later, or with a terminal attached to answer now"
                .into(),
        ),
        Outcome::Failed => Err(delivery
            .error
            .clone()
            .unwrap_or_else(|| "acceptance checks did not pass".into())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_split_like_a_person_would_expect_without_a_shell() {
        assert_eq!(
            split_command("python3 -m unittest -q").unwrap(),
            ["python3", "-m", "unittest", "-q"]
        );
        assert_eq!(
            split_command(r#"pytest -k "a and b" 'x y'"#).unwrap(),
            ["pytest", "-k", "a and b", "x y"]
        );
        assert_eq!(
            split_command(r"C:\tools\run.exe --flag").unwrap(),
            [r"C:\tools\run.exe", "--flag"]
        );
        assert_eq!(split_command(r#"echo "" x"#).unwrap(), ["echo", "", "x"]);
        assert!(split_command("echo 'open").is_err());
        assert!(split_command("   ").is_err());
        // No shell: these are just words.
        assert_eq!(split_command("a && b | c > d").unwrap().len(), 7);
    }

    #[test]
    fn test_looking_paths_are_flagged() {
        for p in [
            "tests/test_x.py",
            "src/foo_test.go",
            "spec/a.rb",
            "test_calc.py",
            "src/__tests__/a.js",
        ] {
            assert!(looks_like_test(p), "{p}");
        }
        for p in ["src/calc.py", "README.md", "docs/guide.md"] {
            assert!(!looks_like_test(p), "{p}");
        }
    }

    #[test]
    fn tails_keep_the_end_and_respect_char_boundaries() {
        assert_eq!(tail("abc", 10), "abc");
        let long = "é".repeat(100);
        let t = tail(&long, 11);
        assert!(t.starts_with('…') && t.len() <= 14);
    }
}
