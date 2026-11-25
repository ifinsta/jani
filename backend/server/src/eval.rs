//! `jani eval`: how often does the agent actually finish the job?
//!
//! Nothing here asks the model whether it succeeded. Each case is a small repo
//! with a task, **visible** checks the agent may run, **hidden** checks it never
//! sees, and a **reference** solution. A run is `solved` only when the hidden
//! checks pass on what the agent delivered, run by the harness afterwards, so
//! passing the visible test by special-casing it does not count.
//!
//! ```text
//! suite/
//!   suite.json                 {"name": "...", "description": "..."}
//!   <case>/case.json           {"prompt", "accept":[...], "hidden":[...], "writePaths":[...], "tags":[...]}
//!   <case>/repo/               the starting files
//!   <case>/hidden/             files copied in after delivery, for the hidden checks
//!   <case>/reference/          files that solve the task (used only by `validate`)
//! ```
//!
//! `jani eval validate SUITE` needs no model: it proves each case is sound
//! (visible check fails before, passes with the reference; hidden check fails
//! before, passes with the reference). `jani eval run SUITE` measures a model.
//! `jani eval compare A B` diffs two results files.

use crate::config::{Config, ExecMode, JResult};
use crate::deliver::{self, run_check, split_command, Outcome, Spec};
use crate::skills::Catalog;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

pub struct Case {
    pub id: String,
    pub dir: PathBuf,
    pub prompt: String,
    pub accept: Vec<Vec<String>>,
    pub hidden: Vec<Vec<String>>,
    pub write_paths: Vec<PathBuf>,
    pub repair_rounds: Option<usize>,
    pub tags: Vec<String>,
}

pub struct Suite {
    pub name: String,
    pub description: String,
    pub cases: Vec<Case>,
}

fn commands(value: &Value, what: &str) -> JResult<Vec<Vec<String>>> {
    value
        .as_array()
        .map(|a| a.as_slice())
        .unwrap_or(&[])
        .iter()
        .map(|v| {
            v.as_str()
                .ok_or_else(|| format!("{what}: every command must be a string"))
                .and_then(split_command)
        })
        .collect()
}

pub fn load_suite(dir: &Path) -> JResult<Suite> {
    let meta: Value = std::fs::read_to_string(dir.join("suite.json"))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or(Value::Null);
    let mut cases = Vec::new();
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .map_err(|e| format!("cannot read suite {}: {e}", dir.display()))?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.join("case.json").is_file())
        .collect();
    entries.sort();
    for path in entries {
        let id = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("case")
            .to_string();
        let text =
            std::fs::read_to_string(path.join("case.json")).map_err(|e| format!("{id}: {e}"))?;
        let v: Value = serde_json::from_str(&text).map_err(|e| format!("{id}/case.json: {e}"))?;
        let prompt = v["prompt"]
            .as_str()
            .filter(|p| !p.trim().is_empty())
            .ok_or(format!("{id}: prompt is required"))?;
        if !path.join("repo").is_dir() {
            return Err(format!("{id}: a case needs a repo/ directory"));
        }
        let accept = commands(&v["accept"], &id)?;
        let hidden = commands(&v["hidden"], &id)?;
        if accept.is_empty() && hidden.is_empty() {
            return Err(format!(
                "{id}: a case needs at least one accept or hidden check, or nothing can be solved"
            ));
        }
        cases.push(Case {
            id,
            dir: path.clone(),
            prompt: prompt.to_string(),
            accept,
            hidden,
            write_paths: v["writePaths"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|p| p.as_str().map(PathBuf::from))
                        .collect()
                })
                .unwrap_or_default(),
            repair_rounds: v["repairRounds"].as_u64().map(|n| n as usize),
            tags: v["tags"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|t| t.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default(),
        });
    }
    if cases.is_empty() {
        return Err(format!("no cases in {}", dir.display()));
    }
    Ok(Suite {
        name: meta["name"].as_str().unwrap_or("suite").to_string(),
        description: meta["description"].as_str().unwrap_or("").to_string(),
        cases,
    })
}

fn copy_dir(from: &Path, to: &Path) -> JResult<()> {
    std::fs::create_dir_all(to).map_err(|e| format!("{}: {e}", to.display()))?;
    for entry in std::fs::read_dir(from)
        .map_err(|e| format!("{}: {e}", from.display()))?
        .flatten()
    {
        let (src, dst) = (entry.path(), to.join(entry.file_name()));
        let kind = entry.file_type().map_err(|e| e.to_string())?;
        if kind.is_symlink() {
            return Err(format!(
                "{}: symlinks are not allowed in a suite",
                src.display()
            ));
        }
        if kind.is_dir() {
            copy_dir(&src, &dst)?;
        } else {
            std::fs::copy(&src, &dst).map_err(|e| format!("{}: {e}", src.display()))?;
        }
    }
    Ok(())
}

fn git(dir: &Path, args: &[&str]) -> JResult<()> {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .map_err(|e| format!("git: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

/// A fresh git repository holding the case's starting files.
fn fresh_repo(case: &Case, dest: &Path) -> JResult<()> {
    copy_dir(&case.dir.join("repo"), dest)?;
    git(dest, &["init", "-q", "-b", "main"])?;
    git(dest, &["add", "-A"])?;
    git(
        dest,
        &[
            "-c",
            "user.name=eval",
            "-c",
            "user.email=eval@local",
            "commit",
            "-qm",
            "start",
        ],
    )
}

fn checks_pass(cfg: &Config, dir: &Path, commands: &[Vec<String>]) -> Vec<bool> {
    let mut run_cfg = cfg.clone();
    run_cfg.root = dir.to_path_buf();
    commands
        .iter()
        .map(|argv| run_check(&run_cfg, 0, argv).passed)
        .collect()
}

/// A case is sound when failing is possible and solving is possible.
pub fn validate(cfg: &Config, suite: &Suite, scratch: &Path) -> Vec<Value> {
    suite
        .cases
        .iter()
        .map(|case| {
            let dir = scratch.join(&case.id);
            let mut problems: Vec<String> = Vec::new();
            let result = (|| -> JResult<()> {
                copy_dir(&case.dir.join("repo"), &dir)?;
                let before_visible = checks_pass(cfg, &dir, &case.accept);
                if !case.accept.is_empty() && before_visible.iter().all(|p| *p) {
                    problems.push(
                        "the visible checks already pass before any work: nothing to fix".into(),
                    );
                }
                if case.dir.join("hidden").is_dir() {
                    copy_dir(&case.dir.join("hidden"), &dir)?;
                }
                let before_hidden = checks_pass(cfg, &dir, &case.hidden);
                if !case.hidden.is_empty() && before_hidden.iter().all(|p| *p) {
                    problems.push(
                        "the hidden checks already pass before any work: they test nothing".into(),
                    );
                }
                if case.dir.join("reference").is_dir() {
                    copy_dir(&case.dir.join("reference"), &dir)?;
                } else {
                    problems.push("no reference/ solution, so solvability is unproven".into());
                }
                if !checks_pass(cfg, &dir, &case.accept).iter().all(|p| *p) {
                    problems.push("the reference solution fails the visible checks".into());
                }
                if !checks_pass(cfg, &dir, &case.hidden).iter().all(|p| *p) {
                    problems.push("the reference solution fails the hidden checks".into());
                }
                Ok(())
            })();
            if let Err(e) = result {
                problems.push(e);
            }
            json!({"case": case.id, "sound": problems.is_empty(), "problems": problems})
        })
        .collect()
}

#[derive(Clone, Debug)]
pub struct RunResult {
    pub case: String,
    pub rep: usize,
    /// verified | unverified | failed | error
    pub outcome: String,
    pub visible_passed: bool,
    pub hidden_passed: Option<bool>,
    pub solved: bool,
    pub tests_touched: bool,
    pub tags: Vec<String>,
    pub rounds: usize,
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub tool_calls: u64,
    pub denials: u64,
    pub refused_for_lack_of_consent: u64,
    pub seconds: f64,
    pub error: Option<String>,
    pub report: Option<PathBuf>,
}

impl RunResult {
    pub fn solved_clean(&self) -> bool {
        self.solved && !self.tests_touched
    }

    /// Why a run did not solve the case, in words a person can sort by.
    pub fn failure(&self) -> Option<&'static str> {
        if self.solved {
            return None;
        }
        Some(match (self.outcome.as_str(), self.hidden_passed) {
            ("error", _) => "harness_or_model_error",
            (_, Some(false)) if self.visible_passed => "passed_visible_failed_hidden",
            _ => "visible_checks_failed",
        })
    }

    fn to_json(&self) -> Value {
        json!({
            "case": self.case, "rep": self.rep, "outcome": self.outcome,
            "solved": self.solved, "solvedClean": self.solved_clean(),
            "visiblePassed": self.visible_passed, "hiddenPassed": self.hidden_passed,
            "testsTouched": self.tests_touched, "tags": self.tags, "rounds": self.rounds,
            "tokensIn": self.tokens_in, "tokensOut": self.tokens_out,
            "toolCalls": self.tool_calls, "denials": self.denials,
            "refusedForLackOfConsent": self.refused_for_lack_of_consent,
            "seconds": (self.seconds * 10.0).round() / 10.0,
            "failure": self.failure(), "error": self.error, "report": self.report,
        })
    }
}

fn journal_counts(state_dir: &Path) -> (u64, u64, u64) {
    let text = std::fs::read_to_string(state_dir.join("journal.jsonl")).unwrap_or_default();
    let (mut tools, mut denials, mut consent) = (0, 0, 0);
    for line in text.lines() {
        let Ok(e) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let action = e["action"].as_str().unwrap_or("");
        let decision = e["decision"].as_str().unwrap_or("");
        if matches!(
            action,
            "repo.read"
                | "repo.write"
                | "repo.list"
                | "command.exec"
                | "skill.read"
                | "native.read"
                | "native.control"
        ) && decision == "executed"
        {
            tools += 1;
        }
        if decision == "deny" {
            denials += 1;
            if e["reason_code"] == "exec_consent_required" {
                consent += 1;
            }
        }
    }
    (tools, denials, consent)
}

pub fn run_case(
    base: &Config,
    catalog: &Catalog,
    case: &Case,
    rep: usize,
    out: &Path,
) -> RunResult {
    let run_dir = out.join("runs").join(format!("{}-{rep}", case.id));
    let started = Instant::now();
    let mut result = RunResult {
        case: case.id.clone(),
        rep,
        outcome: "error".into(),
        visible_passed: false,
        hidden_passed: None,
        solved: false,
        tests_touched: false,
        tags: case.tags.clone(),
        rounds: 0,
        tokens_in: 0,
        tokens_out: 0,
        tool_calls: 0,
        denials: 0,
        refused_for_lack_of_consent: 0,
        seconds: 0.0,
        error: None,
        report: None,
    };
    let attempt = (|| -> JResult<()> {
        let repo = run_dir.join("repo");
        let state = run_dir.join("state");
        fresh_repo(case, &repo)?;
        let mut cfg = base.clone();
        cfg.root = repo;
        cfg.state_dir = state.clone();
        cfg.allow_write = true;
        cfg.write_paths = case.write_paths.clone();
        cfg.task_id = None;
        if cfg.exec_mode == ExecMode::Off {
            cfg.exec_mode = ExecMode::Smart;
        }
        cfg.allow_exec = true;
        let spec = Spec {
            prompt: case.prompt.clone(),
            accept: case.accept.clone(),
            repair_rounds: case.repair_rounds.unwrap_or(base.repair_rounds),
            branch: None,
            use_worktree: true,
            commit: true,
            push_remote: None,
            open_pr: false,
            id: None,
            resume: false,
            context: None,
            cancel: None,
        };
        let delivery = deliver::deliver(&cfg, catalog, &spec, None)?;
        result.outcome = delivery.outcome.as_str().into();
        result.visible_passed = delivery.outcome == Outcome::Verified;
        result.rounds = delivery.rounds;
        result.tokens_in = delivery.tokens.0;
        result.tokens_out = delivery.tokens.1;
        result.tests_touched = !delivery.test_files_changed.is_empty();
        result.error = delivery.error.clone();
        result.report = Some(delivery.report_dir.join("report.md"));
        if !case.hidden.is_empty() && delivery.error.is_none() {
            if case.dir.join("hidden").is_dir() {
                copy_dir(&case.dir.join("hidden"), &delivery.workdir)?;
            }
            let passes = checks_pass(&cfg, &delivery.workdir, &case.hidden);
            result.hidden_passed = Some(passes.iter().all(|p| *p));
        }
        result.solved = match result.hidden_passed {
            Some(hidden) => hidden,
            None => delivery.outcome == Outcome::Verified,
        };
        let (tools, denials, consent) = journal_counts(&state);
        result.tool_calls = tools;
        result.denials = denials;
        result.refused_for_lack_of_consent = consent;
        Ok(())
    })();
    if let Err(e) = attempt {
        result.outcome = "error".into();
        result.error = Some(e);
    }
    result.seconds = started.elapsed().as_secs_f64();
    result
}

fn mean(values: impl Iterator<Item = f64>) -> f64 {
    let v: Vec<f64> = values.collect();
    if v.is_empty() {
        0.0
    } else {
        v.iter().sum::<f64>() / v.len() as f64
    }
}

pub fn summarize(runs: &[RunResult]) -> Value {
    let n = runs.len().max(1) as f64;
    let solved = runs.iter().filter(|r| r.solved).count() as f64;
    let clean = runs.iter().filter(|r| r.solved_clean()).count() as f64;
    let mut cases: Vec<&str> = runs.iter().map(|r| r.case.as_str()).collect();
    cases.sort();
    cases.dedup();
    let per_case: Vec<Value> = cases
        .iter()
        .map(|id| {
            let rs: Vec<&RunResult> = runs.iter().filter(|r| r.case == *id).collect();
            let ok = rs.iter().filter(|r| r.solved).count();
            json!({
                "case": id, "runs": rs.len(), "solved": ok,
                "passAt1": ok as f64 / rs.len() as f64,
                "passAtN": ok > 0,
                "tokens": mean(rs.iter().map(|r| (r.tokens_in + r.tokens_out) as f64)).round(),
                "rounds": mean(rs.iter().map(|r| r.rounds as f64)),
                "failures": rs.iter().filter_map(|r| r.failure()).collect::<Vec<_>>(),
            })
        })
        .collect();
    let any = per_case.iter().filter(|c| c["passAtN"] == true).count() as f64;
    let mut failures = serde_json::Map::new();
    for r in runs {
        if let Some(f) = r.failure() {
            let c = failures.get(f).and_then(Value::as_u64).unwrap_or(0) + 1;
            failures.insert(f.to_string(), json!(c));
        }
    }
    json!({
        "runs": runs.len(),
        "cases": cases.len(),
        "solvedRate": solved / n,
        "solvedCleanRate": clean / n,
        "passAtN": any / cases.len().max(1) as f64,
        "meanRounds": mean(runs.iter().map(|r| r.rounds as f64)),
        "meanTokens": mean(runs.iter().map(|r| (r.tokens_in + r.tokens_out) as f64)).round(),
        "meanSeconds": (mean(runs.iter().map(|r| r.seconds)) * 10.0).round() / 10.0,
        "meanToolCalls": mean(runs.iter().map(|r| r.tool_calls as f64)),
        "testsTouched": runs.iter().filter(|r| r.tests_touched).count(),
        "refusedForLackOfConsent": runs.iter().map(|r| r.refused_for_lack_of_consent).sum::<u64>(),
        "failures": failures,
        "perCase": per_case,
    })
}

fn pct(x: f64) -> String {
    format!("{:.0}%", x * 100.0)
}

pub fn markdown(suite: &Suite, meta: &Value, runs: &[RunResult], summary: &Value) -> String {
    let mut md = format!("# Eval: {}\n\n", suite.name);
    if !suite.description.is_empty() {
        md.push_str(&format!("{}\n\n", suite.description));
    }
    md.push_str(&format!(
        "Model `{}` · exec `{}` · sandbox `{}` · {} run(s) per case\n\n",
        meta["model"].as_str().unwrap_or("?"),
        meta["exec"].as_str().unwrap_or("?"),
        meta["sandbox"].as_str().unwrap_or("?"),
        meta["repeat"],
    ));
    md.push_str(&format!(
        "**Solved {} of {} runs ({})** by the hidden checks. Clean (tests untouched): {}. At least one success per case: {}.\n\n",
        runs.iter().filter(|r| r.solved).count(),
        runs.len(),
        pct(summary["solvedRate"].as_f64().unwrap_or(0.0)),
        pct(summary["solvedCleanRate"].as_f64().unwrap_or(0.0)),
        pct(summary["passAtN"].as_f64().unwrap_or(0.0)),
    ));
    md.push_str("| Case | Solved | Rounds | Tokens | Why not |\n|---|---|---|---|---|\n");
    for c in summary["perCase"].as_array().unwrap_or(&Vec::new()) {
        let why: Vec<String> = c["failures"]
            .as_array()
            .unwrap_or(&Vec::new())
            .iter()
            .filter_map(|f| f.as_str().map(str::to_string))
            .collect();
        md.push_str(&format!(
            "| {} | {}/{} | {:.1} | {} | {} |\n",
            c["case"].as_str().unwrap_or(""),
            c["solved"],
            c["runs"],
            c["rounds"].as_f64().unwrap_or(0.0),
            c["tokens"],
            if why.is_empty() {
                "-".into()
            } else {
                why.join(", ")
            },
        ));
    }
    md.push_str(&format!(
        "\nMean {:.1} tool calls, {:.0} tokens, {:.1}s per run. Test files touched in {} run(s). Commands refused for lack of an operator to ask: {}.\n",
        summary["meanToolCalls"].as_f64().unwrap_or(0.0),
        summary["meanTokens"].as_f64().unwrap_or(0.0),
        summary["meanSeconds"].as_f64().unwrap_or(0.0),
        summary["testsTouched"],
        summary["refusedForLackOfConsent"],
    ));
    if summary["refusedForLackOfConsent"].as_u64().unwrap_or(0) > 0 && meta["sandbox"] == "off" {
        md.push_str("\nCommands were refused because no sandbox confines builds and tests. Re-run with `--sandbox auto` to let them run, or the score understates the model.\n");
    }
    md.push_str("\nSolved means the hidden checks passed on the delivered branch, run by the harness after the agent finished. A case the agent solves by weakening a test shows as solved but not clean.\n");
    md
}

pub fn compare(a: &Value, b: &Value) -> String {
    let get = |v: &Value, k: &str| v["summary"][k].as_f64().unwrap_or(0.0);
    let row = |name: &str, key: &str, percent: bool| {
        let (x, y) = (get(a, key), get(b, key));
        let fmt = |v: f64| if percent { pct(v) } else { format!("{v:.1}") };
        let delta = y - x;
        let d = if percent {
            format!("{:+.0} pts", delta * 100.0)
        } else {
            format!("{delta:+.1}")
        };
        format!("| {name} | {} | {} | {d} |\n", fmt(x), fmt(y))
    };
    let mut out = format!(
        "# Eval comparison\n\nA: `{}` ({})\nB: `{}` ({})\n\n| | A | B | change |\n|---|---|---|---|\n",
        a["meta"]["model"].as_str().unwrap_or("?"), a["meta"]["exec"].as_str().unwrap_or("?"),
        b["meta"]["model"].as_str().unwrap_or("?"), b["meta"]["exec"].as_str().unwrap_or("?"),
    );
    out.push_str(&row("solved", "solvedRate", true));
    out.push_str(&row("solved cleanly", "solvedCleanRate", true));
    out.push_str(&row("any success per case", "passAtN", true));
    out.push_str(&row("mean rounds", "meanRounds", false));
    out.push_str(&row("mean tokens", "meanTokens", false));
    out.push_str(&row("mean seconds", "meanSeconds", false));
    out.push_str("\n| Case | A | B |\n|---|---|---|\n");
    let cases = |v: &Value| {
        v["summary"]["perCase"]
            .as_array()
            .cloned()
            .unwrap_or_default()
    };
    let (ca, cb) = (cases(a), cases(b));
    for case in &ca {
        let id = case["case"].as_str().unwrap_or("");
        let other = cb.iter().find(|c| c["case"] == case["case"]);
        let show = |c: &Value| format!("{}/{}", c["solved"], c["runs"]);
        let mark = match other {
            Some(o) if o["solved"].as_u64() != case["solved"].as_u64() => " **←**",
            _ => "",
        };
        out.push_str(&format!(
            "| {id} | {} | {}{mark} |\n",
            show(case),
            other.map(show).unwrap_or_else(|| "-".into())
        ));
    }
    out
}

pub fn command(cfg: &Config, catalog: &Catalog) -> JResult<()> {
    match cfg.sub.as_deref() {
        Some("compare") => {
            let read = |i: usize| -> JResult<Value> {
                let path = cfg
                    .positional
                    .get(i)
                    .ok_or("compare needs two results.json files")?;
                serde_json::from_str(
                    &std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?,
                )
                .map_err(|e| format!("{path}: {e}"))
            };
            println!("{}", compare(&read(0)?, &read(1)?));
            Ok(())
        }
        Some(which @ ("run" | "validate")) => {
            let dir = cfg
                .positional
                .first()
                .ok_or("give the suite directory, e.g. jani eval run eval/starter")?;
            let mut suite = load_suite(Path::new(dir))?;
            if let Some(only) = cfg.only.as_deref() {
                suite.cases.retain(|c| c.id.contains(only));
                if suite.cases.is_empty() {
                    return Err(format!("no case matches --only {only}"));
                }
            }
            // Bytecode cached by mtime and size hides a same-second rewrite from the next run.
            // SAFETY: set before any other thread exists.
            unsafe { std::env::set_var("PYTHONDONTWRITEBYTECODE", "1") };
            if which == "validate" {
                let scratch = std::env::temp_dir().join(format!(
                    "jani-eval-validate-{}",
                    crate::process::unique_id()
                ));
                let report = validate(cfg, &suite, &scratch);
                let _ = std::fs::remove_dir_all(&scratch);
                let unsound = report.iter().filter(|c| c["sound"] != true).count();
                println!(
                    "{}",
                    serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?
                );
                return if unsound == 0 {
                    eprintln!("jani: all {} cases are sound", report.len());
                    Ok(())
                } else {
                    Err(format!("{unsound} case(s) are not sound"))
                };
            }
            let out = cfg.out_dir.clone().unwrap_or_else(|| {
                cfg.state_dir
                    .join("evals")
                    .join(crate::types::new_id("eval"))
            });
            if out.join("results.json").exists() {
                return Err(format!(
                    "{} already holds results; pick a new --out",
                    out.display()
                ));
            }
            std::fs::create_dir_all(&out).map_err(|e| format!("{}: {e}", out.display()))?;
            let mut base = cfg.clone();
            if base.exec_mode == ExecMode::Off {
                base.exec_mode = ExecMode::Smart;
            }
            let total = suite.cases.len() * cfg.repeat;
            let mut runs = Vec::new();
            for rep in 1..=cfg.repeat {
                for case in &suite.cases {
                    eprintln!(
                        "jani eval: [{}/{total}] {} (run {rep})",
                        runs.len() + 1,
                        case.id
                    );
                    let r = run_case(&base, catalog, case, rep, &out);
                    eprintln!(
                        "           {} in {:.0}s, {} tokens",
                        if r.solved {
                            "solved".to_string()
                        } else {
                            format!("not solved ({})", r.failure().unwrap_or("?"))
                        },
                        r.seconds,
                        r.tokens_in + r.tokens_out
                    );
                    runs.push(r);
                }
            }
            let meta = json!({
                "suite": suite.name, "model": cfg.model, "exec": base.exec_mode.as_str(),
                "sandbox": cfg.sandbox_mode.as_str(), "repeat": cfg.repeat,
                "repairRounds": cfg.repair_rounds, "maxSteps": cfg.max_steps,
                "startedAt": crate::types::now_unix(),
            });
            let summary = summarize(&runs);
            let results = json!({"meta": meta, "summary": summary, "runs": runs.iter().map(RunResult::to_json).collect::<Vec<_>>()});
            std::fs::write(
                out.join("results.json"),
                serde_json::to_vec_pretty(&results).map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string())?;
            let md = markdown(&suite, &meta, &runs, &summary);
            std::fs::write(out.join("report.md"), &md).map_err(|e| e.to_string())?;
            println!("{md}");
            eprintln!("jani eval: results in {}", out.display());
            Ok(())
        }
        Some(other) => Err(format!("unknown eval subcommand '{other}'")),
        None => {
            Err("usage: jani eval run|validate SUITE_DIR | jani eval compare A.json B.json".into())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(
        case: &str,
        solved: bool,
        touched: bool,
        hidden: Option<bool>,
        outcome: &str,
    ) -> RunResult {
        RunResult {
            case: case.into(),
            rep: 1,
            outcome: outcome.into(),
            visible_passed: outcome == "verified",
            hidden_passed: hidden,
            solved,
            tests_touched: touched,
            tags: vec![],
            rounds: 2,
            tokens_in: 100,
            tokens_out: 50,
            tool_calls: 5,
            denials: 0,
            refused_for_lack_of_consent: 0,
            seconds: 3.0,
            error: None,
            report: None,
        }
    }

    #[test]
    fn failures_are_sorted_into_causes_a_person_can_act_on() {
        assert_eq!(
            run("a", true, false, Some(true), "verified").failure(),
            None
        );
        assert_eq!(
            run("a", false, false, Some(false), "verified").failure(),
            Some("passed_visible_failed_hidden")
        );
        assert_eq!(
            run("a", false, false, None, "failed").failure(),
            Some("visible_checks_failed")
        );
        assert_eq!(
            run("a", false, false, None, "error").failure(),
            Some("harness_or_model_error")
        );
    }

    #[test]
    fn a_solve_that_touched_the_tests_counts_as_solved_but_not_clean() {
        let runs = vec![
            run("a", true, true, Some(true), "verified"),
            run("b", true, false, Some(true), "verified"),
        ];
        let s = summarize(&runs);
        assert_eq!(s["solvedRate"], 1.0);
        assert_eq!(s["solvedCleanRate"], 0.5);
        assert_eq!(s["testsTouched"], 1);
    }

    #[test]
    fn pass_at_one_and_pass_at_n_differ_when_runs_vary() {
        let runs = vec![
            run("a", true, false, Some(true), "verified"),
            run("a", false, false, Some(false), "verified"),
            run("b", false, false, None, "failed"),
            run("b", false, false, None, "failed"),
        ];
        let s = summarize(&runs);
        assert_eq!(s["solvedRate"], 0.25);
        assert_eq!(
            s["passAtN"], 0.5,
            "one of two cases was solved at least once"
        );
        let a = &s["perCase"][0];
        assert_eq!(
            (
                a["case"].as_str(),
                a["passAt1"].as_f64(),
                a["passAtN"].as_bool()
            ),
            (Some("a"), Some(0.5), Some(true))
        );
        assert_eq!(s["failures"]["visible_checks_failed"], 2);
    }

    #[test]
    fn compare_shows_the_direction_and_marks_cases_that_changed() {
        let a = json!({"meta": {"model": "m1", "exec": "smart"}, "summary": {"solvedRate": 0.25, "solvedCleanRate": 0.25, "passAtN": 0.5, "meanRounds": 2.0, "meanTokens": 900.0, "meanSeconds": 10.0,
            "perCase": [{"case": "x", "solved": 0, "runs": 2}]}});
        let b = json!({"meta": {"model": "m2", "exec": "smart"}, "summary": {"solvedRate": 0.75, "solvedCleanRate": 0.5, "passAtN": 1.0, "meanRounds": 1.5, "meanTokens": 700.0, "meanSeconds": 8.0,
            "perCase": [{"case": "x", "solved": 2, "runs": 2}]}});
        let text = compare(&a, &b);
        assert!(text.contains("| solved | 25% | 75% | +50 pts |"), "{text}");
        assert!(text.contains("| x | 0/2 | 2/2 **←** |"), "{text}");
    }
}
