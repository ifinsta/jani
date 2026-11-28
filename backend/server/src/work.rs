//! A work queue that survives restarts: the difference between an agent that
//! answers when asked and one that has a job.
//!
//! `jani work add "TASK" --accept CMD` puts a task in `<state-dir>/work/`. A
//! worker (`jani work run`, or `--watch` to keep polling) claims one, runs it
//! through the delivery loop (branch, host-run acceptance, repair, report) and
//! records the outcome. Several workers may share a state directory.
//!
//! Claims are **leases**. A worker renews its lease while it works; if it dies,
//! the lease expires and the task goes back in the queue. The next worker
//! reuses the dead one's worktree and **resumes its saved conversation**
//! (the task store's checkpoint, via `Agent::resume`), so a crash costs the current turn, not the whole task. A task
//! interrupted three times is failed rather than retried forever.
//!
//! Every state change is checked against the claim's owner token, so a worker
//! that was presumed dead and comes back cannot overwrite the result of the one
//! that took over (fencing).
//!
//! Status: `queued` → `running` → `done` | `failed` | `blocked` (waiting for the
//! operator's answer to a question) | `cancelled`.

mod records;

use crate::config::{Config, JResult};
use crate::deliver::{self, split_command, Outcome, Spec};
use crate::skills::Catalog;
use crate::types::{new_id, now_unix};
use records::{get, write};
use serde::{de::DeserializeOwned, Deserialize};
use serde_json::{json, Value};
use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs::File;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::Duration;

const MAX_INTERRUPTED_ATTEMPTS: u64 = 3;
const DEFAULT_LEASE_SECS: u64 = 120;

pub fn lease_secs() -> u64 {
    std::env::var("JANI_WORK_LEASE_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|n| *n >= 1)
        .unwrap_or(DEFAULT_LEASE_SECS)
}

#[derive(Clone, Debug)]
pub struct Task {
    pub id: String,
    pub prompt: String,
    pub accept: Vec<String>,
    pub repair_rounds: Option<usize>,
    pub status: String,
    pub attempts: u64,
    interruptions: u64,
    resume_on_claim: bool,
    delivery_generation: u64,
    pub created: u64,
    pub updated: u64,
    lease_until_ms: u64,
    pub owner: String,
    pub outcome: Option<String>,
    pub report: Option<String>,
    pub branch: Option<String>,
    pub commit: Option<String>,
    pub error: Option<String>,
    /// What the agent asked and is waiting on, when blocked.
    pub question: Option<String>,
    pub options: Vec<String>,
    /// Question and answer pairs, oldest first.
    pub answers: Vec<(String, String)>,
    pub history: Vec<(u64, String)>,
}

#[derive(Deserialize)]
struct SavedAnswer {
    question: String,
    answer: String,
}

#[derive(Deserialize)]
struct SavedEvent {
    at: u64,
    event: String,
}

/// Missing legacy fields can use defaults. A present field must match its type.
fn field<T: DeserializeOwned>(value: &Value, name: &str) -> Option<Option<T>> {
    match value.get(name) {
        Some(value) => T::deserialize(value).ok().map(Some),
        None => Some(None),
    }
}

impl Task {
    fn to_json(&self) -> Value {
        json!({
            "id": self.id, "prompt": self.prompt, "accept": self.accept,
            "repairRounds": self.repair_rounds, "status": self.status,
            "attempts": self.attempts, "interruptions": self.interruptions,
            "resumeOnClaim": self.resume_on_claim,
            "deliveryGeneration": self.delivery_generation,
            "created": self.created, "updated": self.updated,
            "leaseUntil": self.lease_until_ms.div_ceil(1000),
            "leaseUntilMillis": self.lease_until_ms, "owner": self.owner, "outcome": self.outcome,
            "report": self.report, "branch": self.branch, "commit": self.commit,
            "error": self.error, "question": self.question, "options": self.options,
            "answers": self.answers.iter().map(|(q, a)| json!({"question": q, "answer": a})).collect::<Vec<_>>(),
            "history": self.history.iter().map(|(t, e)| json!({"at": t, "event": e})).collect::<Vec<_>>(),
        })
    }

    fn from_json(v: &Value) -> Option<Self> {
        let opt = |k: &str| field::<Option<String>>(v, k).map(Option::flatten);
        let status: String = field(v, "status")??;
        if !matches!(
            status.as_str(),
            "queued" | "running" | "done" | "failed" | "blocked" | "cancelled"
        ) {
            return None;
        }
        let prompt: String = field(v, "prompt")??;
        if prompt.trim().is_empty() {
            return None;
        }
        let history: Vec<_> = field::<Vec<SavedEvent>>(v, "history")?
            .unwrap_or_default()
            .into_iter()
            .map(|entry| (entry.at, entry.event))
            .collect();
        let retried = |event: &str| event == "retried by the operator";
        let seconds = field::<u64>(v, "leaseUntil")?;
        let millis = field(v, "leaseUntilMillis")?;
        let owner: String = field(v, "owner")?.unwrap_or_default();
        if status == "running" && (owner.is_empty() || (seconds.is_none() && millis.is_none())) {
            return None;
        }
        let lease_until_ms =
            millis.unwrap_or_else(|| seconds.unwrap_or_default().saturating_mul(1000));
        Some(Self {
            id: field(v, "id")??,
            prompt,
            accept: field(v, "accept")??,
            repair_rounds: field(v, "repairRounds")?.flatten(),
            status,
            attempts: field(v, "attempts")??,
            interruptions: field(v, "interruptions")?.unwrap_or_else(|| {
                history
                    .iter()
                    .rev()
                    .take_while(|(_, event)| !retried(event))
                    .filter(|(_, event)| {
                        event.starts_with("lease of ")
                            && event.ends_with(" expired; back in the queue")
                    })
                    .count() as u64
            }),
            resume_on_claim: field(v, "resumeOnClaim")?
                .unwrap_or_else(|| history.iter().any(|(_, event)| retried(event))),
            delivery_generation: field(v, "deliveryGeneration")?.unwrap_or_default(),
            created: field(v, "created")??,
            updated: field(v, "updated")?.unwrap_or_default(),
            lease_until_ms,
            owner,
            outcome: opt("outcome")?,
            report: opt("report")?,
            branch: opt("branch")?,
            commit: opt("commit")?,
            error: opt("error")?,
            question: opt("question")?,
            options: field(v, "options")?.unwrap_or_default(),
            answers: field::<Vec<SavedAnswer>>(v, "answers")?
                .unwrap_or_default()
                .into_iter()
                .map(|entry| (entry.question, entry.answer))
                .collect(),
            history,
        })
    }

    fn note(&mut self, event: impl Into<String>) {
        let now = now_unix();
        self.updated = now;
        self.history.push((now, event.into()));
        if self.history.len() > 50 {
            self.history.remove(0);
        }
    }

    fn holds_lease(&self, owner: &str, now_ms: u64) -> bool {
        self.status == "running" && self.owner == owner && self.lease_until_ms > now_ms
    }

    fn clear_lease(&mut self) {
        self.owner.clear();
        self.lease_until_ms = 0;
    }
}

fn with_lock<T>(state: &Path, f: impl FnOnce() -> JResult<T>) -> JResult<T> {
    let _lock = records::lock_queue(state)?;
    f()
}

fn read_all(state: &Path) -> JResult<Vec<Task>> {
    let mut tasks = records::entries(state)?
        .map(|entry| {
            let (id, path) = entry?;
            records::read(&path, &id)
        })
        .collect::<JResult<Vec<_>>>()?;
    tasks.sort_by(|a, b| (a.created, &a.id).cmp(&(b.created, &b.id)));
    Ok(tasks)
}

fn acceptance(checks: &[String]) -> JResult<Vec<Vec<String>>> {
    checks
        .iter()
        .enumerate()
        .map(|(index, line)| {
            split_command(line)
                .ok()
                .filter(|argv| !argv[0].is_empty() && !line.contains('\0'))
                .ok_or_else(|| format!("invalid acceptance check {}", index + 1))
        })
        .collect()
}

fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or_default()
}

fn lease_deadline(now_ms: u64, lease_secs: u64) -> u64 {
    now_ms.saturating_add(lease_secs.saturating_mul(1000))
}

pub fn add(
    state: &Path,
    prompt: &str,
    accept: &[String],
    repair_rounds: Option<usize>,
) -> JResult<Task> {
    if prompt.trim().is_empty() {
        return Err("a task needs a prompt".into());
    }
    acceptance(accept)?;
    with_lock(state, || {
        let now = now_unix();
        // Milliseconds, so tasks added in the same second still keep their order.
        let created_ms = now_millis();
        let mut task = Task {
            id: new_id("wrk"),
            prompt: prompt.to_string(),
            accept: accept.to_vec(),
            repair_rounds,
            status: "queued".into(),
            attempts: 0,
            interruptions: 0,
            resume_on_claim: false,
            delivery_generation: 0,
            created: created_ms,
            updated: now,
            lease_until_ms: 0,
            owner: String::new(),
            outcome: None,
            report: None,
            branch: None,
            commit: None,
            error: None,
            question: None,
            options: Vec::new(),
            answers: Vec::new(),
            history: Vec::new(),
        };
        task.note("queued");
        write(state, &task)?;
        Ok(task)
    })
}

struct Claim {
    task: Task,
    accept: Vec<Vec<String>>,
    _execution_lock: File,
}

#[derive(Default)]
struct ClaimScan {
    next: Option<Claim>,
    issues: BTreeSet<String>,
}

impl ClaimScan {
    fn observe<T>(&mut self, result: JResult<T>) -> Option<T> {
        match result {
            Ok(value) => Some(value),
            Err(error) => {
                self.issues.insert(error);
                None
            }
        }
    }
}

fn expire(state: &Path, task: &mut Task) -> JResult<()> {
    task.interruptions = task.interruptions.saturating_add(1);
    if task.interruptions >= MAX_INTERRUPTED_ATTEMPTS && !has_finalization(state, task)? {
        task.status = "failed".into();
        task.error = Some(format!(
            "interrupted {} times; giving up",
            task.interruptions
        ));
        task.note("failed: too many interrupted attempts");
    } else {
        task.status = "queued".into();
        task.note(format!(
            "lease of {} expired; back in the queue",
            task.owner
        ));
    }
    task.clear_lease();
    write(state, task)
}

/// Requeue expired tasks and claim the oldest queued task without a live local worker.
fn claim(state: &Path, owner: &str, lease: u64, deferred: &HashSet<String>) -> JResult<ClaimScan> {
    with_lock(state, || {
        let locks = records::claims_dir(state)?;
        let now = now_millis();
        let mut scan = ClaimScan::default();
        let mut ready = Vec::new();
        for entry in records::entries(state)? {
            let Some((id, path)) = scan.observe(entry) else {
                continue;
            };
            if deferred.contains(&id) {
                continue;
            }
            let Some(mut task) = scan.observe(records::read(&path, &id)) else {
                continue;
            };
            if task.status == "running" && task.lease_until_ms <= now {
                let Some(Some(_execution_lock)) = scan.observe(records::try_lock_task(&locks, &id))
                else {
                    continue;
                };
                if scan.observe(expire(state, &mut task)).is_none() {
                    continue;
                }
            }
            if task.status == "queued" {
                ready.push((task.created, id));
            }
        }
        ready.sort_unstable();
        for (created, id) in ready {
            let Some(Some(execution_lock)) = scan.observe(records::try_lock_task(&locks, &id))
            else {
                continue;
            };
            let Some(mut task) = scan.observe(get(state, &id)) else {
                continue;
            };
            if task.status != "queued" || task.created != created {
                scan.issues
                    .insert(format!("task {id} changed during queue scan"));
                continue;
            }
            let accept = match acceptance(&task.accept) {
                Ok(accept) => accept,
                Err(error) => {
                    scan.issues.insert(format!("task {id}: {error}"));
                    task.status = "failed".into();
                    task.error = Some(error);
                    task.clear_lease();
                    task.note("failed: invalid acceptance check");
                    scan.observe(write(state, &task));
                    continue;
                }
            };
            task.status = "running".into();
            task.attempts = task.attempts.saturating_add(1);
            task.owner = owner.to_string();
            task.lease_until_ms = lease_deadline(now_millis(), lease);
            let attempt = task.attempts;
            task.note(format!("claimed by {owner} (attempt {attempt})"));
            if scan.observe(write(state, &task)).is_some() {
                scan.next = Some(Claim {
                    task,
                    accept,
                    _execution_lock: execution_lock,
                });
                break;
            }
        }
        Ok(scan)
    })
}

#[derive(Debug)]
enum StateError {
    Queue(String),
    Task(String),
}

impl StateError {
    fn message(&self) -> &str {
        match self {
            Self::Queue(error) | Self::Task(error) => error,
        }
    }
}

fn with_record_lock<T>(state: &Path, update: impl FnOnce() -> JResult<T>) -> Result<T, StateError> {
    let _lock = records::lock_queue(state).map_err(StateError::Queue)?;
    update().map_err(StateError::Task)
}

/// False means the claim is gone. Queue and task I/O errors remain distinguishable.
fn heartbeat(state: &Path, id: &str, owner: &str, lease: u64) -> Result<bool, StateError> {
    with_record_lock(state, || {
        let mut task = get(state, id)?;
        let now = now_millis();
        if !task.holds_lease(owner, now) {
            return Ok(false);
        }
        task.lease_until_ms = lease_deadline(now, lease);
        write(state, &task)?;
        Ok(true)
    })
}

struct Finish {
    status: &'static str,
    outcome: Option<String>,
    report: Option<String>,
    branch: Option<String>,
    commit: Option<String>,
    error: Option<String>,
    question: Option<(String, Vec<String>)>,
}

impl Finish {
    fn failed(error: String) -> Self {
        Self {
            status: "failed",
            outcome: None,
            report: None,
            branch: None,
            commit: None,
            error: Some(error),
            question: None,
        }
    }
}

/// The queue lock and the record write have different failure scopes.
fn finish(state: &Path, id: &str, owner: &str, done: Finish) -> Result<Option<Task>, StateError> {
    with_record_lock(state, || {
        let mut task = get(state, id)?;
        if !task.holds_lease(owner, now_millis()) {
            return Ok(None);
        }
        task.status = done.status.into();
        task.outcome = done.outcome;
        task.report = done.report;
        task.branch = done.branch;
        task.commit = done.commit;
        task.error = done.error;
        if let Some((question, options)) = done.question {
            task.question = Some(question);
            task.options = options;
        }
        task.clear_lease();
        task.note(done.status);
        write(state, &task)?;
        Ok(Some(task))
    })
}

/// The operator's answer to a blocked task's question; the task goes back in the queue.
pub fn answer(state: &Path, id: &str, text: &str) -> JResult<Task> {
    if text.trim().is_empty() {
        return Err("an answer cannot be empty".into());
    }
    with_lock(state, || {
        let mut task = get(state, id)?;
        let Some(question) = task.question.take().filter(|_| task.status == "blocked") else {
            return Err(format!(
                "task {id} is not waiting for an answer (status {})",
                task.status
            ));
        };
        task.answers.push((question, text.trim().to_string()));
        next_delivery_generation(&mut task)?;
        task.options.clear();
        task.status = "queued".into();
        task.note("answered; back in the queue");
        write(state, &task)?;
        Ok(task)
    })
}

pub fn cancel(state: &Path, id: &str) -> JResult<Task> {
    with_lock(state, || {
        let mut task = get(state, id)?;
        match task.status.as_str() {
            "queued" | "blocked" | "running" => {
                // A running worker sees this on its next heartbeat and stops at a safe point.
                task.status = "cancelled".into();
                task.clear_lease();
                task.note("cancelled by the operator");
                write(state, &task)?;
                Ok(task)
            }
            other => Err(format!(
                "task {id} is {other}; only queued, blocked or running tasks can be cancelled"
            )),
        }
    })
}

pub fn retry(state: &Path, id: &str) -> JResult<Task> {
    with_lock(state, || {
        let mut task = get(state, id)?;
        if !matches!(task.status.as_str(), "failed" | "cancelled") {
            return Err(format!(
                "task {id} is {}; only failed or cancelled tasks can be retried",
                task.status
            ));
        }
        let unfinished = task.status == "cancelled" || task.outcome.is_none();
        let resume_finalization = unfinished && has_finalization(state, &task)?;
        task.status = "queued".into();
        if !resume_finalization {
            next_delivery_generation(&mut task)?;
        }
        task.resume_on_claim |= task.attempts > 0;
        task.error = None;
        task.question = None;
        task.options.clear();
        task.attempts = 0;
        task.interruptions = 0;
        task.note("retried by the operator");
        write(state, &task)?;
        Ok(task)
    })
}

fn delivery_id(task_id: &str) -> String {
    let suffix = task_id
        .strip_prefix("wrk-")
        .expect("validated work task id");
    format!("dlv-{suffix}")
}

fn has_finalization(state: &Path, task: &Task) -> JResult<bool> {
    let id = delivery_id(&task.id);
    Ok(
        deliver::saved_queue_finalization(state, &id, task.delivery_generation)?
            || deliver::queue_finalization_intent(state, &id, task.delivery_generation)?,
    )
}

fn next_delivery_generation(task: &mut Task) -> JResult<()> {
    task.delivery_generation = task
        .delivery_generation
        .checked_add(1)
        .ok_or("the task has exhausted its delivery generations")?;
    Ok(())
}

fn answers_context(task: &Task) -> Option<String> {
    if task.answers.is_empty() {
        return None;
    }
    let lines: Vec<String> = task
        .answers
        .iter()
        .map(|(q, a)| format!("Q: {q}\nA (from the operator): {a}"))
        .collect();
    Some(format!(
        "You asked the operator questions earlier. Their answers:\n{}",
        lines.join("\n")
    ))
}

/// Run one claimed task through delivery. Persisting its result is a separate step.
fn process(
    cfg: &Config,
    catalog: &Catalog,
    task: &Task,
    lease: u64,
    accept: Vec<Vec<String>>,
) -> Result<Finish, StateError> {
    let state = cfg.state_dir.clone();
    let cancel = Arc::new(AtomicBool::new(false));
    let spec = Spec {
        prompt: task.prompt.clone(),
        accept,
        repair_rounds: task.repair_rounds.unwrap_or(cfg.repair_rounds),
        branch: None,
        use_worktree: true,
        commit: true,
        push_remote: None,
        open_pr: false,
        id: Some(delivery_id(&task.id)),
        queue_generation: Some(task.delivery_generation),
        resume: task.resume_on_claim || task.attempts > 1 || !task.answers.is_empty(),
        context: answers_context(task),
        cancel: Some(cancel.clone()),
    };
    let result = std::thread::scope(|scope| {
        let (stop, stopped) = mpsc::channel::<()>();
        // Renew the lease while the agent works; stop it if the claim is lost.
        let cancel_on_loss = &cancel;
        let state_dir = &state;
        let renewer = scope.spawn(move || {
            let every = (Duration::from_secs(lease) / 3).max(Duration::from_millis(200));
            while let Err(mpsc::RecvTimeoutError::Timeout) = stopped.recv_timeout(every) {
                match heartbeat(state_dir, &task.id, &task.owner, lease) {
                    Ok(true) => continue,
                    lost_or_failed => {
                        cancel_on_loss.store(true, Ordering::SeqCst);
                        return lost_or_failed.map(|_| ());
                    }
                }
            }
            Ok(())
        });
        let guard_fault = RefCell::new(None);
        let check_claim = || {
            let claim = with_record_lock(&state, || {
                let current = get(&state, &task.id)?;
                Ok(current.delivery_generation == task.delivery_generation
                    && current.holds_lease(&task.owner, now_millis()))
            });
            match claim {
                Ok(true) => Ok(()),
                Ok(false) => Err("the worker no longer holds the delivery claim".into()),
                Err(error) => {
                    let message = error.message().to_owned();
                    guard_fault.replace(Some(error));
                    Err(message)
                }
            }
        };
        let result = deliver::deliver_with_guard(cfg, catalog, &spec, None, &check_claim);
        drop(stop);
        let renewal = renewer
            .join()
            .map_err(|_| StateError::Queue("lease worker panicked".into()))?;
        let guard_fault = guard_fault.into_inner();
        if let Some(error @ StateError::Queue(_)) = guard_fault {
            return Err(error);
        }
        renewal?;
        if let Some(error) = guard_fault {
            return Err(error);
        }
        Ok(result)
    })?;
    Ok(match result {
        Ok(d) => {
            let (status, error) = match d.outcome {
                Outcome::Verified | Outcome::Unverified => ("done", None),
                Outcome::Blocked => ("blocked", None),
                Outcome::Failed => (
                    "failed",
                    d.error
                        .clone()
                        .or_else(|| Some("acceptance checks did not pass".into())),
                ),
            };
            Finish {
                status,
                outcome: Some(d.outcome.as_str().into()),
                report: Some(d.report_dir.join("report.md").display().to_string()),
                branch: d.branch.clone(),
                commit: d.commit.clone(),
                error,
                question: d.question.clone(),
            }
        }
        Err(error) => Finish::failed(error),
    })
}

/// Drain the queue, or keep polling with `watch`.
fn run_worker(cfg: &Config, catalog: &Catalog) -> JResult<()> {
    let lease = lease_secs();
    let mut processed = 0;
    let mut partial_failure = false;
    let mut previous_issues = BTreeSet::new();
    let mut finish_issues = HashMap::new();
    let mut deferred = HashSet::new();
    loop {
        let owner = format!("{}-{}", std::process::id(), new_id("w"));
        let scan = claim(&cfg.state_dir, &owner, lease, &deferred)?;
        for issue in scan.issues.difference(&previous_issues) {
            eprintln!("jani work: {issue}");
        }
        partial_failure |= !scan.issues.is_empty();
        previous_issues = scan.issues;
        match scan.next {
            Some(claim) => {
                let task = &claim.task;
                eprintln!("jani work: running {} (attempt {})", task.id, task.attempts);
                let result = process(cfg, catalog, task, lease, claim.accept)
                    .and_then(|done| finish(&cfg.state_dir, &task.id, &task.owner, done));
                match result {
                    Ok(Some(finished)) => {
                        finish_issues.remove(&task.id);
                        println!(
                            "{}",
                            serde_json::to_string(&summary(&finished)).map_err(|e| e.to_string())?
                        );
                        processed += 1;
                    }
                    Ok(None) => eprintln!("jani work: lost the claim for {}", task.id),
                    Err(StateError::Queue(error)) => return Err(error),
                    Err(StateError::Task(error)) => {
                        if finish_issues.get(&task.id) != Some(&error) {
                            eprintln!("jani work: queue state for {}: {error}", task.id);
                            finish_issues.insert(task.id.clone(), error);
                        }
                        partial_failure = true;
                        if !cfg.watch {
                            deferred.insert(task.id.clone());
                        }
                    }
                }
                if cfg.once {
                    break;
                }
            }
            None if cfg.watch => std::thread::sleep(Duration::from_secs(2)),
            None => break,
        }
    }
    eprintln!("jani work: {processed} task(s) processed");
    if partial_failure {
        return Err(
            "one or more task records could not be processed; see queue diagnostics".into(),
        );
    }
    Ok(())
}

pub fn summary(t: &Task) -> Value {
    json!({
        "id": t.id, "status": t.status, "outcome": t.outcome, "attempts": t.attempts,
        "interruptions": t.interruptions,
        "branch": t.branch, "commit": t.commit, "report": t.report, "error": t.error,
        "question": t.question, "options": t.options,
        "prompt": t.prompt.lines().next().unwrap_or(""),
    })
}

/// `jani work ...`
pub fn command(cfg: &Config, catalog: &Catalog) -> JResult<()> {
    let state = &cfg.state_dir;
    let id = || {
        cfg.positional
            .first()
            .cloned()
            .ok_or_else(|| "give the task id".to_string())
    };
    let show = |t: &Task| {
        println!(
            "{}",
            serde_json::to_string_pretty(&t.to_json()).unwrap_or_default()
        )
    };
    match cfg.sub.as_deref() {
        Some("add") => {
            let prompt = cfg
                .positional
                .first()
                .ok_or("usage: jani work add \"TASK\" [--accept CMD]...")?;
            let repair = cfg.repair_rounds_explicit.then_some(cfg.repair_rounds);
            show(&add(state, prompt, &cfg.accept, repair)?);
            Ok(())
        }
        Some("list") | None => {
            let tasks = read_all(state)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&tasks.iter().map(summary).collect::<Vec<_>>())
                    .map_err(|e| e.to_string())?
            );
            Ok(())
        }
        Some("show") => {
            show(&get(state, &id()?)?);
            Ok(())
        }
        Some("run") => run_worker(cfg, catalog),
        Some("answer") => {
            let text = cfg
                .positional
                .get(1)
                .ok_or("usage: jani work answer ID \"TEXT\"")?;
            show(&answer(state, &id()?, text)?);
            Ok(())
        }
        Some("cancel") => {
            show(&cancel(state, &id()?)?);
            Ok(())
        }
        Some("retry") => {
            show(&retry(state, &id()?)?);
            Ok(())
        }
        Some(other) => Err(format!("unknown work subcommand '{other}'")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use records::dir;
    use std::path::PathBuf;

    fn claim(state: &Path, owner: &str, lease: u64) -> JResult<Option<Claim>> {
        super::claim(state, owner, lease, &HashSet::new()).map(|scan| scan.next)
    }

    fn state(name: &str) -> PathBuf {
        let d =
            std::env::temp_dir().join(format!("jani-work-{name}-{}", crate::process::unique_id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    fn done() -> Finish {
        Finish {
            status: "done",
            outcome: Some("verified".into()),
            report: None,
            branch: None,
            commit: None,
            error: None,
            question: None,
        }
    }

    #[test]
    fn interrupted_writes_are_never_listed_or_claimed() {
        let s = state("uncommitted");
        let t = add(&s, "job", &[], None).unwrap();
        let path = dir(&s).join(format!("{}.json", t.id));
        std::fs::rename(&path, path.with_extension("json.tmp")).unwrap();
        assert!(get(&s, &t.id).is_err());
        assert!(claim(&s, "w", 60).unwrap().is_none());
        let _ = std::fs::remove_dir_all(s);
    }

    #[test]
    fn an_expired_owner_cannot_renew_or_finish_before_any_takeover() {
        let s = state("expired-owner");
        let t = add(&s, "job", &[], None).unwrap();
        claim(&s, "w", 1).unwrap().unwrap();
        std::thread::sleep(Duration::from_millis(1100));
        assert!(finish(&s, &t.id, "w", done()).unwrap().is_none());
        assert!(!heartbeat(&s, &t.id, "w", 60).unwrap());
        assert_eq!(claim(&s, "other", 60).unwrap().unwrap().task.attempts, 2);
        let _ = std::fs::remove_dir_all(s);
    }

    #[test]
    fn a_corrupt_committed_task_is_reported_when_the_queue_is_read() {
        let s = state("corrupt");
        let t = add(&s, "job", &[], None).unwrap();
        let path = dir(&s).join(format!("{}.json", t.id));
        let other = add(&s, "other job", &[], None).unwrap();
        std::fs::write(path, "{invalid json").unwrap();
        assert!(read_all(&s).is_err());
        assert!(get(&s, &t.id).is_err());
        let scan = super::claim(&s, "w", 60, &HashSet::new()).unwrap();
        assert_eq!(scan.next.as_ref().unwrap().task.id, other.id);
        assert_eq!(scan.issues.len(), 1);
        assert!(scan.issues.first().unwrap().contains(&t.id));
        drop(scan);
        let _ = std::fs::remove_dir_all(s);
    }

    #[test]
    fn an_expired_claim_with_a_held_execution_lock_cannot_be_recovered_yet() {
        let s = state("fence-before-recovery");
        let old = add(&s, "old", &[], None).unwrap();
        let held = claim(&s, "old-worker", 60).unwrap().unwrap();
        let mut expired = get(&s, &old.id).unwrap();
        expired.lease_until_ms = 0;
        expired.interruptions = MAX_INTERRUPTED_ATTEMPTS - 1;
        write(&s, &expired).unwrap();
        let next = add(&s, "next", &[], None).unwrap();

        let selected = claim(&s, "next-worker", 60).unwrap().unwrap();
        assert_eq!(selected.task.id, next.id);
        let untouched = get(&s, &old.id).unwrap();
        assert_eq!(untouched.status, "running");
        assert_eq!(untouched.interruptions, expired.interruptions);
        drop(held);
        assert!(claim(&s, "replacement", 60).unwrap().is_none());
        assert_eq!(get(&s, &old.id).unwrap().status, "failed");
        drop(selected);
        let _ = std::fs::remove_dir_all(s);
    }

    #[test]
    fn a_broken_lock_or_claim_write_only_blocks_its_own_task() {
        let s = state("isolated-claim");
        let order = |prompt: &str, created| {
            let mut task = add(&s, prompt, &[], None).unwrap();
            task.created = created;
            write(&s, &task).unwrap();
            task
        };
        let bad_lock = order("bad lock", 1);
        let bad_write = order("bad staging path", 2);
        let healthy = order("healthy", 3);
        let blocked_lock = records::claims_dir(&s).unwrap().join(&bad_lock.id);
        let blocked_write = dir(&s).join(format!("{}.json.tmp", bad_write.id));
        std::fs::create_dir(&blocked_lock).unwrap();
        std::fs::create_dir(&blocked_write).unwrap();

        let scan = super::claim(&s, "worker", 60, &HashSet::new()).unwrap();
        assert_eq!(scan.next.as_ref().unwrap().task.id, healthy.id);
        assert_eq!(scan.issues.len(), 2);
        for task in [&bad_lock, &bad_write] {
            let saved = get(&s, &task.id).unwrap();
            assert_eq!(saved.status, "queued");
            assert_eq!(saved.attempts, 0);
        }
        assert!(blocked_lock.is_dir() && blocked_write.is_dir());
        drop(scan);
        std::fs::remove_dir(blocked_lock).unwrap();
        std::fs::remove_dir(blocked_write).unwrap();
        let first = claim(&s, "worker-2", 60).unwrap().unwrap();
        let second = claim(&s, "worker-3", 60).unwrap().unwrap();
        assert_eq!(first.task.id, bad_lock.id);
        assert_eq!(second.task.id, bad_write.id);
        drop((first, second));
        let _ = std::fs::remove_dir_all(s);
    }

    #[test]
    fn damaged_scheduling_fields_cannot_become_legacy_defaults() {
        let s = state("record-schema");
        let valid = add(&s, "job", &[], None).unwrap().to_json();
        for (field, malformed) in [
            ("created", json!("earlier")),
            ("attempts", Value::Null),
            ("prompt", json!("   ")),
            ("leaseUntilMillis", json!("now")),
            ("answers", json!([{"question": "Where?", "answer": false}])),
            (
                "history",
                json!([{"at": "last week", "event": "retried by the operator"}]),
            ),
        ] {
            let mut record = valid.clone();
            record[field] = malformed;
            assert!(
                Task::from_json(&record).is_none(),
                "accepted invalid {field}"
            );
        }
        for field in ["attempts", "created"] {
            let mut record = valid.clone();
            record.as_object_mut().unwrap().remove(field);
            assert!(
                Task::from_json(&record).is_none(),
                "accepted absent {field}"
            );
        }
        let _ = std::fs::remove_dir_all(s);
    }

    #[test]
    fn old_task_records_preserve_their_lease_and_recovery_history() {
        let s = state("old-format");
        let t = add(&s, "job", &[], None).unwrap();
        let mut value = t.to_json();
        for field in ["leaseUntilMillis", "interruptions", "resumeOnClaim"] {
            value.as_object_mut().unwrap().remove(field);
        }
        value["leaseUntil"] = json!(100);
        value["history"] = json!([
            {"at": 90, "event": "retried by the operator"},
            {"at": 96, "event": "lease of old-worker expired; back in the queue"}
        ]);
        let record = Task::from_json(&value).unwrap();
        assert_eq!(record.lease_until_ms, 100_000);
        assert_eq!(record.interruptions, 1);
        assert!(record.resume_on_claim);
        value["accept"] = json!([false]);
        assert!(Task::from_json(&value).is_none());
        let _ = std::fs::remove_dir_all(s);
    }

    #[test]
    fn a_lost_claim_and_a_completion_write_error_have_distinct_results() {
        let s = state("finish-io");
        let t = add(&s, "job", &[], None).unwrap();
        let held = claim(&s, "w", 60).unwrap().unwrap();
        let tmp = dir(&s).join(format!("{}.json.tmp", t.id));
        std::fs::create_dir(&tmp).unwrap();
        assert!(finish(&s, &t.id, "other", done()).unwrap().is_none());
        assert!(matches!(
            finish(&s, &t.id, "w", done()),
            Err(StateError::Task(_))
        ));
        assert!(matches!(
            heartbeat(&s, &t.id, "w", 60),
            Err(StateError::Task(_))
        ));
        assert_eq!(get(&s, &t.id).unwrap().status, "running");
        let shared_lock = s.join("work/.lock");
        std::fs::remove_file(&shared_lock).unwrap();
        std::fs::create_dir(&shared_lock).unwrap();
        assert!(matches!(
            finish(&s, &t.id, "w", done()),
            Err(StateError::Queue(_))
        ));
        assert!(matches!(
            heartbeat(&s, &t.id, "w", 60),
            Err(StateError::Queue(_))
        ));
        drop(held);
        let _ = std::fs::remove_dir_all(s);
    }

    #[test]
    fn tasks_are_claimed_oldest_first_and_only_once() {
        let s = state("order");
        let a = add(&s, "first", &[], None).unwrap();
        std::thread::sleep(Duration::from_millis(1100));
        let b = add(&s, "second", &[], None).unwrap();
        let one = claim(&s, "w1", 60).unwrap().unwrap();
        let two = claim(&s, "w2", 60).unwrap().unwrap();
        assert_eq!(
            (one.task.id.as_str(), two.task.id.as_str()),
            (a.id.as_str(), b.id.as_str())
        );
        assert!(
            claim(&s, "w3", 60).unwrap().is_none(),
            "nothing left to claim"
        );
        drop((one, two));
        let _ = std::fs::remove_dir_all(s);
    }

    #[test]
    fn a_retry_waits_for_its_old_worker_without_blocking_other_tasks() {
        let s = state("live-delivery");
        let first = add(&s, "first", &[], None).unwrap();
        let old = claim(&s, "old", 60).unwrap().unwrap();
        cancel(&s, &first.id).unwrap();
        retry(&s, &first.id).unwrap();
        let second = add(&s, "second", &[], None).unwrap();

        let other = claim(&s, "other", 60).unwrap().unwrap();
        assert_eq!(other.task.id, second.id);
        assert!(claim(&s, "replacement", 60).unwrap().is_none());
        drop(old);
        let replacement = claim(&s, "replacement", 60).unwrap().unwrap();
        assert_eq!(replacement.task.id, first.id);
        drop((other, replacement));
        let _ = std::fs::remove_dir_all(s);
    }

    #[test]
    fn an_expired_lease_requeues_the_task_and_the_old_owner_is_fenced_out() {
        let s = state("fence");
        let t = add(&s, "job", &[], None).unwrap();
        let first = claim(&s, "slow-worker", 1).unwrap().unwrap();
        assert_eq!(first.task.attempts, 1);
        std::thread::sleep(Duration::from_millis(2200));
        // A second worker takes over once the lease has lapsed.
        drop(first);
        let second = claim(&s, "new-worker", 60).unwrap().unwrap();
        assert_eq!(
            (second.task.id.as_str(), second.task.attempts),
            (t.id.as_str(), 2)
        );
        // The presumed-dead worker comes back: neither its heartbeat nor its result counts.
        assert!(!heartbeat(&s, &t.id, "slow-worker", 60).unwrap());
        assert!(finish(&s, &t.id, "slow-worker", done()).unwrap().is_none());
        assert_eq!(get(&s, &t.id).unwrap().status, "running");
        assert_eq!(
            finish(&s, &t.id, "new-worker", done())
                .unwrap()
                .unwrap()
                .status,
            "done"
        );
        drop(second);
        let _ = std::fs::remove_dir_all(s);
    }

    #[test]
    fn a_task_interrupted_too_often_fails_instead_of_looping_forever() {
        let s = state("giveup");
        let t = add(&s, "job", &[], None).unwrap();
        for _ in 0..MAX_INTERRUPTED_ATTEMPTS {
            claim(&s, "w", 1).unwrap().unwrap();
            std::thread::sleep(Duration::from_millis(1100));
        }
        assert!(claim(&s, "w", 1).unwrap().is_none());
        let task = get(&s, &t.id).unwrap();
        assert_eq!(task.status, "failed");
        assert!(task.error.unwrap().contains("giving up"));
        // The operator can still retry it.
        assert_eq!(retry(&s, &t.id).unwrap().status, "queued");
        let _ = std::fs::remove_dir_all(s);
    }

    #[test]
    fn answered_questions_do_not_spend_the_interruption_budget() {
        let s = state("question-budget");
        let t = add(&s, "job", &[], None).unwrap();
        for _ in 0..MAX_INTERRUPTED_ATTEMPTS {
            claim(&s, "w", 60).unwrap().unwrap();
            let mut blocked = done();
            blocked.status = "blocked";
            blocked.question = Some(("Continue?".into(), Vec::new()));
            finish(&s, &t.id, "w", blocked).unwrap().unwrap();
            answer(&s, &t.id, "yes").unwrap();
        }
        let mut interrupted = claim(&s, "w", 60).unwrap().unwrap().task;
        interrupted.lease_until_ms = 0;
        write(&s, &interrupted).unwrap();
        let resumed = claim(&s, "replacement", 60).unwrap().unwrap().task;
        assert_eq!(resumed.interruptions, 1);
        assert_eq!(resumed.attempts, MAX_INTERRUPTED_ATTEMPTS + 2);
        let _ = std::fs::remove_dir_all(s);
    }

    #[test]
    fn a_heartbeat_keeps_a_long_task_alive() {
        let s = state("beat");
        let t = add(&s, "job", &[], None).unwrap();
        claim(&s, "w", 2).unwrap().unwrap();
        for _ in 0..4 {
            std::thread::sleep(Duration::from_millis(700));
            assert!(heartbeat(&s, &t.id, "w", 2).unwrap());
        }
        assert!(
            claim(&s, "other", 60).unwrap().is_none(),
            "a renewed lease is not stolen"
        );
        let _ = std::fs::remove_dir_all(s);
    }

    #[test]
    fn answers_requeue_a_blocked_task_and_only_a_blocked_one() {
        let s = state("answer");
        let t = add(&s, "job", &[], None).unwrap();
        assert!(answer(&s, &t.id, "yes").is_err(), "not blocked");
        let c = claim(&s, "w", 60).unwrap().unwrap();
        finish(
            &s,
            &c.task.id,
            "w",
            Finish {
                status: "blocked",
                outcome: None,
                report: None,
                branch: None,
                commit: None,
                error: None,
                question: Some((
                    "Which database?".into(),
                    vec!["postgres".into(), "sqlite".into()],
                )),
            },
        )
        .unwrap();
        drop(c);
        let blocked = get(&s, &t.id).unwrap();
        assert_eq!(
            (blocked.status.as_str(), blocked.options.len()),
            ("blocked", 2)
        );
        assert!(
            claim(&s, "w2", 60).unwrap().is_none(),
            "a blocked task is not claimable"
        );
        assert!(answer(&s, &t.id, "   ").is_err());
        let requeued = answer(&s, &t.id, "sqlite").unwrap();
        assert_eq!(requeued.status, "queued");
        assert_eq!(
            requeued.answers,
            vec![("Which database?".to_string(), "sqlite".to_string())]
        );
        assert!(answers_context(&requeued)
            .unwrap()
            .contains("A (from the operator): sqlite"));
        let _ = std::fs::remove_dir_all(s);
    }

    #[test]
    fn cancel_and_retry_only_apply_where_they_make_sense_and_ids_are_validated() {
        let s = state("cancel");
        let t = add(&s, "job", &[], None).unwrap();
        assert!(retry(&s, &t.id).is_err());
        assert_eq!(cancel(&s, &t.id).unwrap().status, "cancelled");
        assert!(cancel(&s, &t.id).is_err());
        assert_eq!(retry(&s, &t.id).unwrap().status, "queued");
        assert!(get(&s, "../etc/passwd").is_err());
        assert!(add(&s, "  ", &[], None).is_err());
        assert!(
            add(&s, "job", &["python 'oops".to_string()], None).is_err(),
            "a malformed check is refused up front"
        );
        assert_eq!(acceptance(&["' '".into()]).unwrap(), vec![vec![" "]]);
        let _ = std::fs::remove_dir_all(s);
    }
}
