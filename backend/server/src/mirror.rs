//! Jani Mirror CLI: read-only evidence inspection plus explicit proposal recording.
//! Nothing in this module executes a tool, modifies a policy or invokes a model.
use crate::config::{Config, JResult};
use crate::identity;
use crate::journal;
use crate::policy::PolicyEngine;
use crate::types::{new_id, sha256_hex, ActionRequest, Decision, Effect, RiskTier};
use jani_mirror::{compare, run_report, workflow_report};
use jani_store::mirror::MirrorRegister;
use serde_json::{json, to_value, Value};
use std::fs;

const MAX_JOURNAL_BYTES: u64 = 32 * 1024 * 1024;

fn run_arg<'a>(cfg: &'a Config, index: usize) -> JResult<&'a str> {
    let id = cfg.positional.get(index).map(String::as_str)
        .or_else(|| if index == 0 { cfg.run_id.as_deref() } else { None })
        .ok_or("Mirror operation requires a run ID")?;
    if id.len() > 128 || id.is_empty() ||
        !id.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_'|'-'|'.')) {
        return Err("run ID contains unsupported characters".into());
    }
    Ok(id)
}

fn read_journal(cfg: &Config) -> JResult<Vec<crate::types::AuditEvent>> {
    let path = cfg.state_dir.join(journal::JOURNAL_FILE);
    match fs::symlink_metadata(&path) {
        Ok(meta) => {
            if meta.file_type().is_symlink() || !meta.is_file() {
                return Err("Mirror refuses symlinked or non-regular audit files".into());
            }
            if meta.len() > MAX_JOURNAL_BYTES {
                return Err("journal exceeds the 32 MiB Mirror review window; export a bounded trusted slice for a future reader".into());
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
        Err(e) => return Err(e.to_string()),
    }
    let result = journal::verify(&cfg.state_dir)?;
    if result.get("ok").and_then(Value::as_bool) != Some(true) {
        return Err("Mirror refused unverified/tampered audit journal".into());
    }
    journal::read_events(&path)
}

/// Reflection is a separate, explicitly authorized action.
/// It inherits enterprise signed policy and principal verification; there is
/// no special Mirror privilege, and audit records do not grant it one.
pub fn command(cfg: &Config) -> JResult<Value> {
    let sub = cfg.sub.as_deref().unwrap_or("workflows");
    if !matches!(sub, "inspect" | "critique" | "workflows" |
          "compare" | "recommendations" | "record" | "history") {
        return Err(format!("unknown Mirror subcommand '{sub}'"));
    }
    let identity = identity::load(cfg)?;
    let effect = if sub == "record" { Effect::Control } else { Effect::Read };
    let resource = if matches!(sub, "inspect" | "critique" | "record" | "compare") ||
        (sub == "recommendations" && !cfg.positional.is_empty()) {
        format!("mirror:run:{}", run_arg(cfg, 0)?)
    } else {
        format!("mirror:tenant:{}", identity.tenant)
    };
    let policy = PolicyEngine::load(cfg)?;
    let action = format!("mirror.{sub}");
    let verdict = policy.evaluate(cfg, &ActionRequest {
        task_id: cfg.task_id.clone().unwrap_or_else(|| "mirror-review".into()),
        invocation_id: new_id("mirror"),
        tenant_id: identity.tenant.clone(),
        principal_id: identity.principal.clone(),
        action,
        resource,
        effect,
        risk: if sub == "record" { RiskTier::R1 } else { RiskTier::R0 },
        arguments_digest: sha256_hex(b"mirror-metadata-only"),
        idempotency_key: None,
    });
    if verdict.decision != Decision::Permit {
        return Err(format!("Mirror authorization denied: {}", verdict.reason_code));
    }
    if sub == "compare" {
        // The first run's policy grant never authorizes access to the second.
        let second = run_arg(cfg, 1)?;
        let decision = policy.evaluate(cfg, &ActionRequest {
            task_id: cfg.task_id.clone().unwrap_or_else(|| "mirror-review".into()),
            invocation_id: new_id("mirror"),
            tenant_id: identity.tenant.clone(),
            principal_id: identity.principal.clone(),
            action: "mirror.compare".into(),
            resource: format!("mirror:run:{second}"),
            effect: Effect::Read,
            risk: RiskTier::R0,
            arguments_digest: sha256_hex(b"mirror-compare-second-run"),
            idempotency_key: None,
        });
        if decision.decision != Decision::Permit {
            return Err(format!("Mirror comparison denied for second run: {}", decision.reason_code));
        }
    }
    if sub == "history" {
        if !cfg.state_dir.join("mirror").join("improvements.sqlite3").exists() {
            return Ok(json!({"status":"proposed_only","recommendations":[]}));
        }
        return MirrorRegister::open(&cfg.state_dir)?.list(&identity.tenant, 100);
    }

    let events = read_journal(cfg)?;
    match sub {
        "inspect" => to_value(run_report(&events, &identity.tenant, run_arg(cfg,0)?)?)
            .map_err(|e| e.to_string()),
        "critique" => {
            let mut report = run_report(&events, &identity.tenant, run_arg(cfg,0)?)?;
            // Critical summaries are short: detailed event history is available
            // through mirror inspect, and is not sent to a model.
            report.timeline.clear();
            to_value(report).map_err(|e| e.to_string())
        }
        "workflows" => {
            let mut report = workflow_report(&events, &identity.tenant);
            // Avoid printing thousands of historical events across 50 runs.
            for run in &mut report.runs {
                run.timeline.clear();
            }
            to_value(report).map_err(|e| e.to_string())
        }
        "recommendations" => {
            let findings: Vec<jani_mirror::Finding> = if !cfg.positional.is_empty() {
                run_report(&events, &identity.tenant, run_arg(cfg,0)?)?.findings
            } else {
                workflow_report(&events,&identity.tenant).runs.into_iter()
                    .flat_map(|r| r.findings).collect()
            };
            Ok(json!({
                "mode":"deterministic",
                "status":"proposed_only",
                "recommendations":findings,
                "note":"Rule-based observations; no LLM review, automated execution or proven causal improvement."
            }))
        }
        "compare" => {
            let a = run_report(&events, &identity.tenant, run_arg(cfg,0)?)?;
            let b = run_report(&events, &identity.tenant, run_arg(cfg,1)?)?;
            to_value(compare(a,b)).map_err(|e| e.to_string())
        }
        "record" => {
            let report = run_report(&events, &identity.tenant, run_arg(cfg,0)?)?;
            let items = report.findings.iter().map(to_value)
                .collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())?;
            // Writing improvements is explicit and never updates Jani's code,
            // policies, workflows, runtime state or tool permissions.
            let inserted = MirrorRegister::open(&cfg.state_dir)?
                .record(&identity.tenant, &items)?;
            Ok(json!({
                "runId":report.run_id,
                "recorded": inserted,
                "candidates": items.len(),
                "status":"proposed_only",
                "autoApplied":false
            }))
        }
        _ => unreachable!("validated Mirror subcommand"),
    }
}
