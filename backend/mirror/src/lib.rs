//! Evidence-driven, deterministic reflection. No model, coordinator, network,
//! tool execution or autonomous self-modification is part of this library.
use jani_core::journal::{decision, reason};
use jani_core::types::{sha256_hex, AuditEvent};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

pub const VERSION: &str = "mirror/v1";
pub const MAX_TIMELINE: usize = 160;
pub const MAX_WORKFLOW_RUNS: usize = 50;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Observation {
    pub event_id: String,
    pub at: u64,
    pub action: String,
    pub decision: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Finding {
    pub id: String,
    pub run_id: String,
    pub code: String,
    pub severity: String,
    pub observation: String,
    pub recommendation: String,
    pub evidence: Vec<String>,
    pub confidence: String,
    pub status: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunReport {
    pub version: &'static str,
    pub run_id: String,
    pub outcome: String,
    pub first_event_at: u64,
    pub last_event_at: u64,
    pub elapsed_observed_seconds: u64,
    pub event_count: usize,
    pub timeline_truncated: bool,
    pub actions: BTreeMap<String, u64>,
    pub decisions: BTreeMap<String, u64>,
    pub timeline: Vec<Observation>,
    pub findings: Vec<Finding>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowReport {
    pub version: &'static str,
    pub considered_runs: usize,
    pub completed: usize,
    pub failed: usize,
    pub incomplete: usize,
    pub recurring_findings: BTreeMap<String, usize>,
    pub runs: Vec<RunReport>,
    pub note: &'static str,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Comparison {
    pub version: &'static str,
    pub a: RunReport,
    pub b: RunReport,
    pub event_count_delta_b_minus_a: i64,
    pub denial_count_delta_b_minus_a: i64,
    pub interpretation: &'static str,
}

// Only report stable, non-sensitive code-like identifiers; no prompts, paths,
// arguments, model messages, resource digests or tool results are included.
fn safe(value: &str) -> String {
    if !jani_core::types::valid_id(value, 96) {
        return "redacted_identifier".into();
    }
    value.to_string()
}

fn identify(run: &str, code: &str, evidence: &[String]) -> String {
    let digest = sha256_hex(format!("mirror/v1\0{run}\0{code}\0{}", evidence.join(",")).as_bytes());
    format!("mir-{}", &digest[..24])
}

fn finding(
    run: &str,
    code: &str,
    severity: &str,
    observation: &str,
    recommendation: &str,
    evidence: Vec<String>,
) -> Finding {
    Finding {
        id: identify(run, code, &evidence),
        run_id: run.to_owned(),
        code: code.into(),
        severity: severity.into(),
        observation: observation.into(),
        recommendation: recommendation.into(),
        evidence,
        confidence: "rule_based".into(),
        status: "proposed".into(),
    }
}

fn is_denied(event: &AuditEvent) -> bool {
    matches!(event.decision.as_str(), "deny" | "denied" | "refused")
}

fn terminal<'a>(events: &[&'a AuditEvent]) -> Option<&'a AuditEvent> {
    events.iter().rev().copied().find(|e| {
        e.action == jani_core::tools::action::AGENT_RUN
            && matches!(e.decision.as_str(), decision::COMPLETED | decision::FAILED)
    })
}

pub fn run_report(events: &[AuditEvent], tenant: &str, run_id: &str) -> Result<RunReport, String> {
    if run_id.is_empty() || run_id.len() > 128 {
        return Err("run id must be 1..128 bytes".into());
    }
    let relevant: Vec<&AuditEvent> = events
        .iter()
        .filter(|e| e.tenant_id == tenant && e.run_id == run_id)
        .collect();
    if relevant.is_empty() {
        return Err("run not found in the authorized tenant's audit history".into());
    }
    let start = relevant.iter().map(|e| e.timestamp).min().unwrap_or(0);
    let end = relevant.iter().map(|e| e.timestamp).max().unwrap_or(0);
    let mut actions = BTreeMap::new();
    let mut decisions = BTreeMap::new();
    for e in &relevant {
        *actions.entry(safe(&e.action)).or_insert(0) += 1;
        *decisions.entry(safe(&e.decision)).or_insert(0) += 1;
    }
    let terminal_event = terminal(&relevant);
    let outcome = terminal_event
        .map(|e| e.decision.as_str())
        .unwrap_or("incomplete")
        .to_string();
    let mut findings = Vec::new();
    if outcome == "incomplete" {
        let evidence = relevant
            .last()
            .map(|e| vec![e.event_id.clone()])
            .unwrap_or_default();
        findings.push(finding(run_id, "run_incomplete", "high",
            "No trusted agent.run completion or failure event is recorded.",
            "Inspect worker termination, journal durability and checkpoint ownership; do not claim completion.",
            evidence));
    }
    if let Some(event) = terminal_event {
        // Diagnose the same latest terminal event that determines the outcome;
        // earlier handoffs or non-terminal reasons must not override it.
        let diagnostic = match (event.decision.as_str(), event.reason_code.as_str()) {
            (decision::COMPLETED | decision::FAILED, reason::CHECKPOINT)
            | (decision::FAILED, reason::STEP_LIMIT) => Some((
                "step_budget_exhausted",
                "The run used its whole step allowance and had to be closed with a handoff instead of finishing.",
                "Review task decomposition, repeated tool calls and step budget; compare with a smaller bounded plan.",
            )),
            (decision::COMPLETED, reason::STALLED) => Some((
                "repeated_calls",
                "The run was closed with a handoff after repeated tool calls; task completion is not verified.",
                "Review repeated tool calls and their receipts; revise the plan before resuming and verify task completion independently.",
            )),
            (decision::FAILED, _) => Some((
                "failed_run",
                "Jani recorded a failed run; the audit trail alone does not establish root cause.",
                "Inspect the failure reason and provider/tool receipts before changing the workflow.",
            )),
            _ => None,
        };
        if let Some((code, observation, recommendation)) = diagnostic {
            findings.push(finding(
                run_id,
                code,
                "high",
                observation,
                recommendation,
                vec![event.event_id.clone()],
            ));
        }
    }
    // Ignore operational outcomes on agent.run; only repeated tool denials
    // should trigger a redundant-denial warning.
    let mut denials: BTreeMap<(String, String), Vec<String>> = BTreeMap::new();
    for e in &relevant {
        if e.action != jani_core::tools::action::AGENT_RUN && is_denied(e) {
            denials
                .entry((safe(&e.action), safe(&e.reason_code)))
                .or_default()
                .push(e.event_id.clone());
        }
    }
    let loops: Vec<Vec<String>> = denials.into_values().filter(|ids| ids.len() >= 2).collect();
    if !loops.is_empty() {
        findings.push(finding(run_id, "repeated_denial", "medium",
            "An identical action and reason were denied repeatedly during the run.",
            "Treat an authorization refusal as terminal for that action; request operator approval or revise the plan rather than retrying.",
            loops.into_iter().flat_map(|v| v.into_iter().take(3)).take(12).collect()));
    }
    let wrote = relevant.iter().any(|e| {
        e.action.starts_with(jani_core::tools::action::REPO_WRITE)
            && matches!(
                e.decision.as_str(),
                "permit" | "permitted" | "completed" | "succeeded"
            )
    });
    let tested = relevant.iter().any(|e| {
        e.action == jani_core::tools::action::COMMAND_EXEC
            && matches!(
                e.decision.as_str(),
                decision::EXECUTED | "completed" | "succeeded"
            )
    });
    if wrote && !tested {
        findings.push(finding(run_id, "verification_evidence_gap", "low",
            "A repository write is recorded without a permitted command.exec event in this run.",
            "Check whether tests were executed through another audited channel; collect verification evidence before claiming the change is tested.",
            relevant.iter().filter(|e| e.action.starts_with(jani_core::tools::action::REPO_WRITE))
                .take(2).map(|e| e.event_id.clone()).collect()));
    }
    let timeline_truncated = relevant.len() > MAX_TIMELINE;
    let timeline: Vec<Observation> = relevant
        .iter()
        .rev()
        .take(MAX_TIMELINE)
        .rev()
        .map(|e| Observation {
            event_id: e.event_id.clone(),
            at: e.timestamp,
            action: safe(&e.action),
            decision: safe(&e.decision),
            reason: safe(&e.reason_code),
        })
        .collect();
    Ok(RunReport {
        version: VERSION,
        run_id: safe(run_id),
        outcome,
        first_event_at: start,
        last_event_at: end,
        elapsed_observed_seconds: end.saturating_sub(start),
        event_count: relevant.len(),
        timeline_truncated,
        actions,
        decisions,
        timeline,
        findings,
    })
}

pub fn workflow_report(events: &[AuditEvent], tenant: &str) -> WorkflowReport {
    let mut run_ids: Vec<(u64, String)> = events
        .iter()
        .filter(|e| e.tenant_id == tenant && e.action == "agent.run")
        .map(|e| (e.timestamp, e.run_id.clone()))
        .collect();
    run_ids.sort_by(|a, b| b.cmp(a));
    let mut seen = BTreeSet::new();
    let mut runs = Vec::new();
    for (_, id) in run_ids {
        if !seen.insert(id.clone()) {
            continue;
        }
        if let Ok(report) = run_report(events, tenant, &id) {
            runs.push(report);
        }
        if runs.len() >= MAX_WORKFLOW_RUNS {
            break;
        }
    }
    let completed = runs.iter().filter(|r| r.outcome == "completed").count();
    let failed = runs.iter().filter(|r| r.outcome == "failed").count();
    let incomplete = runs.iter().filter(|r| r.outcome == "incomplete").count();
    let mut recurring_findings = BTreeMap::new();
    for run in &runs {
        for f in &run.findings {
            *recurring_findings.entry(f.code.clone()).or_insert(0) += 1;
        }
    }
    WorkflowReport {
        version: VERSION, considered_runs: runs.len(), completed, failed, incomplete,
        recurring_findings, runs,
        note: "Last 50 distinct audited agent.run executions; findings are heuristics, not proven causes. Counts are not normalized for task difficulty.",
    }
}

pub fn compare(a: RunReport, b: RunReport) -> Comparison {
    let denial_count = |r: &RunReport| -> i64 {
        r.decisions.get("denied").copied().unwrap_or(0) as i64
            + r.decisions.get("deny").copied().unwrap_or(0) as i64
    };
    Comparison {
        version: VERSION,
        event_count_delta_b_minus_a: b.event_count as i64 - a.event_count as i64,
        denial_count_delta_b_minus_a: denial_count(&b) - denial_count(&a),
        interpretation: "Descriptive deltas only. Different tasks cannot establish causal workflow improvement; compare matched scenarios with independent acceptance tests.",
        a, b,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jani_core::tools::action;
    use jani_core::types::Classification;

    fn e(
        run: &str,
        tenant: &str,
        action: &str,
        decision: &str,
        reason: &str,
        i: u64,
    ) -> AuditEvent {
        AuditEvent {
            event_version: "1.0".into(),
            event_id: format!("e-{run}-{i}"),
            tenant_id: tenant.into(),
            task_id: run.into(),
            run_id: run.into(),
            invocation_id: format!("inv-{i}"),
            actor_id: "worker".into(),
            worker_id: "worker".into(),
            action: action.into(),
            resource_digest: "secret-resource-digest".into(),
            decision: decision.into(),
            reason_code: reason.into(),
            policy_revision: "1".into(),
            approval_id: None,
            sandbox_receipt_id: None,
            data_classification: Classification::Internal,
            timestamp: i,
            previous_event_digest: String::new(),
            event_digest: String::new(),
        }
    }

    #[test]
    fn incomplete_and_repeated_denial_report_is_grounded() {
        let events = vec![
            e("a", "t", "agent.run", "started", "run_started", 1),
            e("a", "t", "repo.write", "denied", "permission", 2),
            e("a", "t", "repo.write", "denied", "permission", 3),
            e("z", "other", "agent.run", "failed", "step_limit", 4),
        ];
        let r = run_report(&events, "t", "a").unwrap();
        assert_eq!(r.outcome, "incomplete");
        assert!(r.findings.iter().any(|f| f.code == "run_incomplete"));
        let d = r
            .findings
            .iter()
            .find(|f| f.code == "repeated_denial")
            .unwrap();
        assert_eq!(d.evidence.len(), 2);
        let serialized = serde_json::to_string(&r).unwrap();
        assert!(!serialized.contains("secret-resource-digest"));
        assert!(run_report(&events, "other", "a").is_err());
    }

    #[test]
    fn failed_and_completed_runs_are_distinguished() {
        let events = vec![
            e("a", "t", "agent.run", "started", "run_started", 1),
            e("a", "t", "agent.run", "failed", "step_limit", 2),
            e("b", "t", "agent.run", "started", "run_started", 3),
            e("b", "t", "agent.run", "completed", "final_answer", 4),
        ];
        let a = run_report(&events, "t", "a").unwrap();
        let b = run_report(&events, "t", "b").unwrap();
        assert!(a.findings.iter().any(|f| f.code == "step_budget_exhausted"));
        assert!(b.findings.is_empty());
        let w = workflow_report(&events, "t");
        assert_eq!((w.completed, w.failed, w.incomplete), (1, 1, 0));
        assert_eq!(compare(a, b).event_count_delta_b_minus_a, 0);
    }

    #[test]
    fn completed_checkpoint_preserves_outcome_and_exact_evidence() {
        let events = vec![
            e(
                "a",
                "t",
                action::AGENT_RUN,
                decision::STARTED,
                reason::RUN_STARTED,
                1,
            ),
            e(
                "a",
                "t",
                action::AGENT_RUN,
                decision::COMPLETED,
                reason::CHECKPOINT,
                2,
            ),
            e(
                "a",
                "t",
                action::COMMAND_EXEC,
                decision::COMPLETED,
                reason::OK,
                3,
            ),
        ];
        let report = run_report(&events, "t", "a").unwrap();
        assert_eq!(report.outcome, decision::COMPLETED);
        assert_eq!(report.findings.len(), 1);
        let finding = &report.findings[0];
        assert_eq!(finding.code, "step_budget_exhausted");
        assert_eq!(finding.evidence, vec![events[1].event_id.clone()]);
        assert!(finding.observation.contains("handoff"));
        assert!(finding.recommendation.contains("step budget"));
        let workflow = workflow_report(&events, "t");
        assert_eq!(
            (workflow.completed, workflow.failed, workflow.incomplete),
            (1, 0, 0)
        );
    }

    #[test]
    fn completed_stalled_reports_grounded_repeated_calls() {
        let events = vec![
            e(
                "a",
                "t",
                action::AGENT_RUN,
                decision::COMPLETED,
                reason::CHECKPOINT,
                1,
            ),
            e(
                "a",
                "t",
                action::AGENT_RUN,
                decision::COMPLETED,
                reason::STALLED,
                2,
            ),
            e(
                "a",
                "t",
                action::AGENT_RUN,
                decision::STARTED,
                reason::CHECKPOINT,
                3,
            ),
        ];
        let report = run_report(&events, "t", "a").unwrap();
        assert_eq!(report.outcome, decision::COMPLETED);
        assert_eq!(report.findings.len(), 1);
        let finding = &report.findings[0];
        assert_eq!(finding.code, "repeated_calls");
        assert_eq!(finding.evidence, vec![events[1].event_id.clone()]);
        assert!(finding.observation.contains("repeated tool calls"));
        assert!(finding.observation.contains("not verified"));
        assert!(finding.recommendation.contains("revise the plan"));
        assert!(finding.recommendation.contains("verify task completion"));
    }

    #[test]
    fn last_terminal_final_answer_suppresses_earlier_handoffs() {
        let events = vec![
            e(
                "a",
                "t",
                action::AGENT_RUN,
                decision::COMPLETED,
                reason::CHECKPOINT,
                1,
            ),
            e(
                "a",
                "t",
                action::AGENT_RUN,
                decision::COMPLETED,
                reason::STALLED,
                2,
            ),
            e(
                "a",
                "t",
                action::AGENT_RUN,
                decision::COMPLETED,
                reason::FINAL_ANSWER,
                3,
            ),
            e(
                "a",
                "t",
                action::AGENT_RUN,
                decision::STARTED,
                reason::STEP_LIMIT,
                4,
            ),
            e(
                "a",
                "t",
                action::COMMAND_EXEC,
                decision::COMPLETED,
                reason::CHECKPOINT,
                5,
            ),
        ];
        let report = run_report(&events, "t", "a").unwrap();
        assert_eq!(report.outcome, decision::COMPLETED);
        assert!(report.findings.is_empty());
    }

    #[test]
    fn handoff_diagnostics_and_evidence_are_tenant_and_run_scoped() {
        for (terminal_reason, code) in [
            (reason::CHECKPOINT, "step_budget_exhausted"),
            (reason::STALLED, "repeated_calls"),
        ] {
            let events = vec![
                e(
                    "a",
                    "t",
                    action::AGENT_RUN,
                    decision::STARTED,
                    reason::RUN_STARTED,
                    1,
                ),
                e(
                    "a",
                    "t",
                    action::AGENT_RUN,
                    decision::COMPLETED,
                    terminal_reason,
                    2,
                ),
                e(
                    "a",
                    "other",
                    action::AGENT_RUN,
                    decision::FAILED,
                    reason::STEP_LIMIT,
                    3,
                ),
                e(
                    "b",
                    "t",
                    action::AGENT_RUN,
                    decision::COMPLETED,
                    reason::FINAL_ANSWER,
                    4,
                ),
                e(
                    "a",
                    "other",
                    action::AGENT_RUN,
                    decision::COMPLETED,
                    reason::FINAL_ANSWER,
                    5,
                ),
            ];
            let report = run_report(&events, "t", "a").unwrap();
            assert_eq!(report.outcome, decision::COMPLETED);
            assert_eq!(report.event_count, 2);
            assert_eq!(report.findings.len(), 1);
            assert_eq!(report.findings[0].code, code);
            assert_eq!(report.findings[0].run_id, "a");
            assert_eq!(
                report.findings[0].evidence,
                vec![events[1].event_id.clone()]
            );
            assert_eq!(
                report
                    .timeline
                    .iter()
                    .map(|e| e.event_id.as_str())
                    .collect::<Vec<_>>(),
                vec![events[0].event_id.as_str(), events[1].event_id.as_str()],
            );
            let serialized = serde_json::to_string(&report).unwrap();
            for excluded in &events[2..] {
                assert!(!serialized.contains(&excluded.event_id));
            }
            let other = run_report(&events, "other", "a").unwrap();
            assert_eq!(other.outcome, decision::COMPLETED);
            assert!(other.findings.is_empty());
            assert!(run_report(&events, "unauthorized", "a").is_err());
        }
    }

    #[test]
    fn latest_failure_reason_overrides_earlier_handoffs() {
        for (terminal_reason, code) in [
            (reason::STEP_LIMIT, "step_budget_exhausted"),
            (reason::CHECKPOINT, "step_budget_exhausted"),
            (reason::MODEL_OR_GOVERNANCE_FAILURE, "failed_run"),
        ] {
            let events = vec![
                e(
                    "a",
                    "t",
                    action::AGENT_RUN,
                    decision::COMPLETED,
                    reason::CHECKPOINT,
                    1,
                ),
                e(
                    "a",
                    "t",
                    action::AGENT_RUN,
                    decision::COMPLETED,
                    reason::STALLED,
                    2,
                ),
                e(
                    "a",
                    "t",
                    action::AGENT_RUN,
                    decision::FAILED,
                    terminal_reason,
                    3,
                ),
                e(
                    "a",
                    "t",
                    action::COMMAND_EXEC,
                    decision::COMPLETED,
                    reason::CHECKPOINT,
                    4,
                ),
            ];
            let report = run_report(&events, "t", "a").unwrap();
            assert_eq!(report.outcome, decision::FAILED);
            assert_eq!(report.findings.len(), 1);
            assert_eq!(report.findings[0].code, code);
            assert_eq!(
                report.findings[0].evidence,
                vec![events[2].event_id.clone()]
            );
        }
    }

    #[test]
    fn no_false_success_and_bounded_timeline() {
        let mut events = vec![e("a", "t", "agent.run", "started", "run_started", 1)];
        for i in 2..400 {
            events.push(e("a", "t", "skill.read", "permit", "ok", i));
        }
        let r = run_report(&events, "t", "a").unwrap();
        assert_eq!(r.outcome, "incomplete");
        assert_eq!(r.timeline.len(), MAX_TIMELINE);
        assert!(r.timeline_truncated);
    }
}
