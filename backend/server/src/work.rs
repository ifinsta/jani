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
//! (`--resume`), so a crash costs the current turn, not the whole task. A task
//! interrupted three times is failed rather than retried forever.
//!
//! Every state change is checked against the claim's owner token, so a worker
//! that was presumed dead and comes back cannot overwrite the result of the one
//! that took over (fencing).
//!
//! Status: `queued` → `running` → `done` | `failed` | `blocked` (waiting for the
//! operator's answer to a question) | `cancelled`.

use crate::config::{Config, JResult};
use crate::consent::SharedConsent;
use crate::deliver::{self, split_command, Outcome, Spec};
use crate::skills::Catalog;
use crate::types::{new_id, now_unix};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

pub const MAX_ATTEMPTS: u64 = 3;
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
    pub created: u64,
    pub updated: u64,
    pub lease_until: u64,
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

impl Task {
    fn to_json(&self) -> Value {
        json!({
            "id": self.id, "prompt": self.prompt, "accept": self.accept,
            "repairRounds": self.repair_rounds, "status": self.status,
            "attempts": self.attempts, "created": self.created, "updated": self.updated,
            "leaseUntil": self.lease_until, "owner": self.owner, "outcome": self.outcome,
            "report": self.report, "branch": self.branch, "commit": self.commit,
            "error": self.error, "question": self.question, "options": self.options,
            "answers": self.answers.iter().map(|(q, a)| json!({"question": q, "answer": a})).collect::<Vec<_>>(),
            "history": self.history.iter().map(|(t, e)| json!({"at": t, "event": e})).collect::<Vec<_>>(),
        })
    }

    fn from_json(v: &Value) -> Option<Self> {
        let strings = |k: &str| -> Vec<String> {
            v[k].as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default()
        };
        let opt = |k: &str| v[k].as_str().map(str::to_string);
        Some(Self {
            id: opt("id")?,
            prompt: opt("prompt")?,
            accept: strings("accept"),
            repair_rounds: v["repairRounds"].as_u64().map(|n| n as usize),
            status: opt("status")?,
            attempts: v["attempts"].as_u64().unwrap_or(0),
            created: v["created"].as_u64().unwrap_or(0),
            updated: v["updated"].as_u64().unwrap_or(0),
            lease_until: v["leaseUntil"].as_u64().unwrap_or(0),
            owner: opt("owner").unwrap_or_default(),
            outcome: opt("outcome"),
            report: opt("report"),
            branch: opt("branch"),
            commit: opt("commit"),
            error: opt("error"),
            question: opt("question"),
            options: strings("options"),
            answers: v["answers"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|x| {
                            Some((
                                x["question"].as_str()?.to_string(),
                                x["answer"].as_str()?.to_string(),
                            ))
                        })
                        .collect()
                })
                .unwrap_or_default(),
            history: v["history"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|x| Some((x["at"].as_u64()?, x["event"].as_str()?.to_string())))
                        .collect()
                })
                .unwrap_or_default(),
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
}

fn dir(state: &Path) -> PathBuf {
    state.join("work").join("tasks")
}

fn with_lock<T>(state: &Path, f: impl FnOnce() -> JResult<T>) -> JResult<T> {
    let root = state.join("work");
    std::fs::create_dir_all(dir(state)).map_err(|e| format!("{}: {e}", root.display()))?;
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(root.join(".lock"))
        .map_err(|e| e.to_string())?;
    lock.lock().map_err(|e| format!("work lock: {e}"))?;
    let out = f();
    let _ = lock.unlock();
    out
}

fn read_all(state: &Path) -> Vec<Task> {
    let mut tasks: Vec<Task> = std::fs::read_dir(dir(state))
        .map(|entries| {
            entries
                .flatten()
                .filter_map(|e| std::fs::read_to_string(e.path()).ok())
                .filter_map(|t| serde_json::from_str::<Value>(&t).ok())
                .filter_map(|v| Task::from_json(&v))
                .collect()
        })
        .unwrap_or_default();
    tasks.sort_by(|a, b| (a.created, &a.id).cmp(&(b.created, &b.id)));
    tasks
}

fn write(state: &Path, task: &Task) -> JResult<()> {
    let path = dir(state).join(format!("{}.json", task.id));
    let tmp = path.with_extension("json.tmp");
    std::fs::write(
        &tmp,
        serde_json::to_vec_pretty(&task.to_json()).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &path).map_err(|e| e.to_string())
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

pub fn list(state: &Path) -> Vec<Task> {
    read_all(state)
}

pub fn get(state: &Path, id: &str) -> JResult<Task> {
    if !valid_id(id) {
        return Err(format!("'{id}' is not a task id"));
    }
    read_all(state)
        .into_iter()
        .find(|t| t.id == id)
        .ok_or_else(|| format!("no task {id}"))
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
    for line in accept {
        split_command(line)?;
    }
    with_lock(state, || {
        let now = now_unix();
        // Milliseconds, so tasks added in the same second still keep their order.
        let created_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(now * 1000);
        let mut task = Task {
            id: new_id("wrk"),
            prompt: prompt.to_string(),
            accept: accept.to_vec(),
            repair_rounds,
            status: "queued".into(),
            attempts: 0,
            created: created_ms,
            updated: now,
            lease_until: 0,
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

/// Take the oldest queued task for `owner`. First, any running task whose lease
/// has expired goes back in the queue (or fails, after `MAX_ATTEMPTS`).
pub fn claim(state: &Path, owner: &str, lease: u64) -> JResult<Option<Task>> {
    with_lock(state, || {
        let now = now_unix();
        for mut task in read_all(state) {
            if task.status == "running" && task.lease_until <= now {
                if task.attempts >= MAX_ATTEMPTS {
                    task.status = "failed".into();
                    task.error = Some(format!("interrupted {} times; giving up", task.attempts));
                    task.note("failed: too many interrupted attempts");
                } else {
                    task.status = "queued".into();
                    task.note(format!(
                        "lease of {} expired; back in the queue",
                        task.owner
                    ));
                }
                task.owner.clear();
                write(state, &task)?;
            }
        }
        let Some(mut task) = read_all(state).into_iter().find(|t| t.status == "queued") else {
            return Ok(None);
        };
        task.status = "running".into();
        task.attempts += 1;
        task.owner = owner.to_string();
        task.lease_until = now + lease;
        let attempt = task.attempts;
        task.note(format!("claimed by {owner} (attempt {attempt})"));
        write(state, &task)?;
        Ok(Some(task))
    })
}

/// Renew a lease. False means the claim is gone (cancelled, or taken over).
pub fn heartbeat(state: &Path, id: &str, owner: &str, lease: u64) -> bool {
    with_lock(state, || {
        let mut task = get(state, id)?;
        if task.status != "running" || task.owner != owner {
            return Ok(false);
        }
        task.lease_until = now_unix() + lease;
        write(state, &task)?;
        Ok(true)
    })
    .unwrap_or(false)
}

pub struct Finish {
    pub status: &'static str,
    pub outcome: Option<String>,
    pub report: Option<String>,
    pub branch: Option<String>,
    pub commit: Option<String>,
    pub error: Option<String>,
    pub question: Option<(String, Vec<String>)>,
}

/// Record how a claimed task ended. Refused unless `owner` still holds the claim.
pub fn finish(state: &Path, id: &str, owner: &str, done: Finish) -> JResult<Task> {
    with_lock(state, || {
        let mut task = get(state, id)?;
        if task.status != "running" || task.owner != owner {
            return Err(format!(
                "task {id} is no longer yours (status {}, owner {}); not overwriting",
                task.status,
                if task.owner.is_empty() {
                    "none"
                } else {
                    &task.owner
                }
            ));
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
        task.owner.clear();
        task.lease_until = 0;
        task.note(done.status);
        write(state, &task)?;
        Ok(task)
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
                task.owner.clear();
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
        task.status = "queued".into();
        task.error = None;
        task.attempts = 0;
        task.note("retried by the operator");
        write(state, &task)?;
        Ok(task)
    })
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

/// Run one claimed task through delivery and record how it ended.
pub fn process(
    cfg: &Config,
    catalog: &Catalog,
    task: &Task,
    lease: u64,
    consent: Option<SharedConsent>,
) -> JResult<Task> {
    let state = cfg.state_dir.clone();
    let cancel = Arc::new(AtomicBool::new(false));
    let stop = AtomicBool::new(false);
    let spec = Spec {
        prompt: task.prompt.clone(),
        accept: task
            .accept
            .iter()
            .map(|a| split_command(a))
            .collect::<JResult<Vec<_>>>()?,
        repair_rounds: task.repair_rounds.unwrap_or(cfg.repair_rounds),
        branch: None,
        use_worktree: true,
        commit: true,
        push_remote: None,
        open_pr: false,
        id: Some(format!("dlv-{}", task.id.trim_start_matches("wrk-"))),
        resume: task.attempts > 1 || !task.answers.is_empty(),
        context: answers_context(task),
        cancel: Some(cancel.clone()),
    };
    let result = std::thread::scope(|scope| {
        // Renew the lease while the agent works; stop it if the claim is lost.
        scope.spawn(|| {
            let every = Duration::from_millis((lease * 1000 / 3).max(200));
            let mut waited = Duration::ZERO;
            while !stop.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(100));
                waited += Duration::from_millis(100);
                if waited >= every {
                    waited = Duration::ZERO;
                    if !heartbeat(&state, &task.id, &task.owner, lease) {
                        cancel.store(true, Ordering::SeqCst);
                        break;
                    }
                }
            }
        });
        let result = deliver::deliver(cfg, catalog, &spec, consent);
        stop.store(true, Ordering::SeqCst);
        result
    });
    let done = match result {
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
        Err(e) => Finish {
            status: "failed",
            outcome: None,
            report: None,
            branch: None,
            commit: None,
            error: Some(e),
            question: None,
        },
    };
    // If the operator cancelled while it ran, the claim is gone and that stands.
    finish(&state, &task.id, &task.owner, done).or_else(|_| get(&state, &task.id))
}

/// Drain the queue (or keep polling with `watch`). Returns the tasks processed.
pub fn run_worker(cfg: &Config, catalog: &Catalog) -> JResult<Vec<Task>> {
    let owner = format!("{}-{}", std::process::id(), new_id("w"));
    let lease = lease_secs();
    let mut processed = Vec::new();
    loop {
        match claim(&cfg.state_dir, &owner, lease)? {
            Some(task) => {
                eprintln!("jani work: running {} (attempt {})", task.id, task.attempts);
                let finished = process(cfg, catalog, &task, lease, None)?;
                println!(
                    "{}",
                    serde_json::to_string(&summary(&finished)).map_err(|e| e.to_string())?
                );
                processed.push(finished);
                if cfg.once {
                    break;
                }
            }
            None if cfg.watch => std::thread::sleep(Duration::from_secs(2)),
            None => break,
        }
    }
    Ok(processed)
}

pub fn summary(t: &Task) -> Value {
    json!({
        "id": t.id, "status": t.status, "outcome": t.outcome, "attempts": t.attempts,
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
            let repair = (cfg.repair_rounds != crate::config::defaults::REPAIR_ROUNDS)
                .then_some(cfg.repair_rounds);
            show(&add(state, prompt, &cfg.accept, repair)?);
            Ok(())
        }
        Some("list") | None => {
            let tasks = list(state);
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
        Some("run") => {
            let done = run_worker(cfg, catalog)?;
            eprintln!("jani work: {} task(s) processed", done.len());
            Ok(())
        }
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
    fn tasks_are_claimed_oldest_first_and_only_once() {
        let s = state("order");
        let a = add(&s, "first", &[], None).unwrap();
        std::thread::sleep(Duration::from_millis(1100));
        let b = add(&s, "second", &[], None).unwrap();
        let one = claim(&s, "w1", 60).unwrap().unwrap();
        let two = claim(&s, "w2", 60).unwrap().unwrap();
        assert_eq!(
            (one.id.as_str(), two.id.as_str()),
            (a.id.as_str(), b.id.as_str())
        );
        assert!(
            claim(&s, "w3", 60).unwrap().is_none(),
            "nothing left to claim"
        );
        let _ = std::fs::remove_dir_all(s);
    }

    #[test]
    fn an_expired_lease_requeues_the_task_and_the_old_owner_is_fenced_out() {
        let s = state("fence");
        let t = add(&s, "job", &[], None).unwrap();
        let first = claim(&s, "slow-worker", 1).unwrap().unwrap();
        assert_eq!(first.attempts, 1);
        std::thread::sleep(Duration::from_millis(2200));
        // A second worker takes over once the lease has lapsed.
        let second = claim(&s, "new-worker", 60).unwrap().unwrap();
        assert_eq!((second.id.as_str(), second.attempts), (t.id.as_str(), 2));
        // The presumed-dead worker comes back: neither its heartbeat nor its result counts.
        assert!(!heartbeat(&s, &t.id, "slow-worker", 60));
        assert!(finish(&s, &t.id, "slow-worker", done())
            .unwrap_err()
            .contains("no longer yours"));
        assert_eq!(get(&s, &t.id).unwrap().status, "running");
        assert_eq!(
            finish(&s, &t.id, "new-worker", done()).unwrap().status,
            "done"
        );
        let _ = std::fs::remove_dir_all(s);
    }

    #[test]
    fn a_task_interrupted_too_often_fails_instead_of_looping_forever() {
        let s = state("giveup");
        let t = add(&s, "job", &[], None).unwrap();
        for _ in 0..MAX_ATTEMPTS {
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
    fn a_heartbeat_keeps_a_long_task_alive() {
        let s = state("beat");
        let t = add(&s, "job", &[], None).unwrap();
        claim(&s, "w", 2).unwrap().unwrap();
        for _ in 0..4 {
            std::thread::sleep(Duration::from_millis(700));
            assert!(heartbeat(&s, &t.id, "w", 2));
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
            &c.id,
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
        let _ = std::fs::remove_dir_all(s);
    }
}
