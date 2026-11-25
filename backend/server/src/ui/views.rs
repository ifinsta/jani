//! Read-mostly views for `jani ui`. Each one calls the subsystem the matching
//! CLI command already uses, with the process's own configuration, identity and
//! policy, so the browser sees exactly what `jani <command>` would print and can
//! never do more. Anything that mutates state or runs privileged operations is
//! refused here and stays on the command line.

use crate::config::{Config, JResult};
use crate::types::AuditEvent;
use crate::{evidence, journal, plugins, sandbox, types, update};
use serde_json::{json, Value};

fn valid_run(id: &str) -> JResult<()> {
    if types::valid_id(id, 128) {
        Ok(())
    } else {
        Err("run ID must be 1..128 ASCII letters, digits, '.', '-' or '_'".into())
    }
}

pub(super) fn policy(cfg: &Config) -> JResult<Value> {
    let mut value = crate::policy_explain(cfg)?;
    value["settings"] = json!({
        "path": cfg.settings_path.as_ref().map(|p| p.display().to_string()),
        "profile": cfg.settings_profile,
        "warnings": cfg.settings_warnings,
        "permissions": cfg.permissions.as_ref().map(|p| p.describe()),
        "maxSteps": cfg.max_steps,
        "profiles": cfg.model_choices.iter().map(|c| json!({"label": c.label, "model": c.model})).collect::<Vec<_>>(),
    });
    value["mode"] = json!(cfg.mode.as_str());
    Ok(value)
}

pub(super) fn policy_check(cfg: &Config, body: &Value) -> JResult<Value> {
    let action = body["action"].as_str().unwrap_or("");
    let resource = body["resource"].as_str().unwrap_or("");
    if action.is_empty() || action.len() > 128 || resource.is_empty() || resource.len() > 512 {
        return Err("action (1..128) and resource (1..512) are required".into());
    }
    if action.chars().chain(resource.chars()).any(char::is_control) {
        return Err("action and resource must be printable".into());
    }
    crate::policy_check(cfg, action, resource)
}

pub(super) fn audit(cfg: &Config) -> JResult<Value> {
    if !cfg.state_dir.join(journal::JOURNAL_FILE).is_file() {
        return Ok(json!({"ok": true, "events": 0, "created": false}));
    }
    journal::verify(&cfg.state_dir)
}

pub(super) fn evidence_export(cfg: &Config, id: &str) -> JResult<Value> {
    valid_run(id)?;
    let events = journal::read_verified_events(&cfg.state_dir.join(journal::JOURNAL_FILE))?;
    let receipt = evidence::receipt_with_verifier_keys(
        &cfg.state_dir,
        id,
        &cfg.verifier_keys,
        &cfg.revoked_keys,
    )?;
    let events: Vec<_> = events
        .into_iter()
        .filter(|event| event.run_id == id)
        .collect();
    Ok(json!({
        "schemaVersion": "jani.run-evidence-export/v1",
        "exportedAt": types::now_unix(),
        "receipt": receipt,
        "events": events,
        "note": "Audit-only export. It does not verify the engineering outcome, reconcile side effects or attest the result.",
    }))
}

/// Receipts for the most recent runs, newest first. Runs with no verifiable
/// receipt are listed with the reason instead of being dropped.
pub(super) fn evidence_list(events: &[AuditEvent], run_ids: &[String]) -> Value {
    let rows: Vec<Value> = run_ids
        .iter()
        .take(30)
        .map(
            |id| match evidence::receipt_from_verified_events(events, id) {
                Ok(receipt) => json!({"runId": id, "receipt": receipt}),
                Err(error) => json!({"runId": id, "error": error}),
            },
        )
        .collect();
    json!(rows)
}

pub(super) fn mirror(
    cfg: &Config,
    sub: &str,
    run: Option<&str>,
    other: Option<&str>,
) -> JResult<Value> {
    // `record` persists a recommendation, so it is deliberately not offered.
    let mut positional = Vec::new();
    match sub {
        "workflows" | "history" => {}
        "recommendations" => {
            if let Some(run) = run {
                valid_run(run)?;
                positional.push(run.to_string());
            }
        }
        "inspect" | "critique" => {
            let run = run.ok_or("a run ID is required")?;
            valid_run(run)?;
            positional.push(run.to_string());
        }
        "compare" => {
            let (a, b) = (
                run.ok_or("two run IDs are required")?,
                other.ok_or("two run IDs are required")?,
            );
            valid_run(a)?;
            valid_run(b)?;
            positional.push(a.to_string());
            positional.push(b.to_string());
        }
        _ => return Err("unsupported Mirror view".into()),
    }
    let mut next = cfg.clone();
    next.command = "mirror".into();
    next.sub = Some(sub.to_string());
    next.positional = positional;
    crate::mirror::command(&next)
}

pub(super) fn lab(cfg: &Config, run: Option<&str>) -> JResult<Value> {
    let mut out = json!({
        "version": "jani-lab/v1",
        "labKeysConfigured": cfg.lab_keys.len(),
        "evaluation": "Evaluation compares independently signed trial receipts and is run with `jani lab evaluate FILE --lab-key ID:HEX`. It cannot run trials, sign evidence, approve patches or deploy.",
    });
    if let Some(run) = run {
        valid_run(run)?;
        let mut next = cfg.clone();
        next.command = "lab".into();
        next.sub = Some("propose".into());
        next.positional = vec![run.to_string()];
        out["proposal"] = crate::lab::command(&next)?;
    }
    Ok(out)
}

/// What the plugin registry holds and why it might not. The directory is named, not
/// printed in full, so a hosted person never sees the server's paths.
pub(super) fn plugins(cfg: &Config) -> Value {
    let mut out = json!({
        "configured": cfg.plugin_dir.is_some(),
        "trustRoots": cfg.plugin_keys.len(),
        "directory": cfg
            .plugin_dir
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned()),
    });
    match plugins::PluginRegistry::load(cfg) {
        Ok(registry) => {
            out["plugins"] = registry.list()["plugins"].clone();
            out["verify"] = registry
                .verify(None)
                .unwrap_or_else(|e| json!({"ok": false, "error": e}));
        }
        Err(error) => {
            out["plugins"] = json!([]);
            out["error"] = json!(error);
        }
    }
    out
}

/// One plugin, as `jani plugins inspect` shows it, minus the server's file path.
pub(super) fn plugin_detail(cfg: &Config, id: &str) -> JResult<Value> {
    let registry = plugins::PluginRegistry::load(cfg)?;
    let mut value = registry.inspect(id)?;
    if let Some(name) = value["source"]
        .as_str()
        .and_then(|s| std::path::Path::new(s).file_name())
        .map(|n| n.to_string_lossy().into_owned())
    {
        value["source"] = json!(name);
    }
    let sidecar = value["runtime"].as_str() == Some("sidecar");
    value["invocable"] = json!(sidecar);
    if let Some(map) = value.as_object_mut() {
        map.remove("invocationNote");
    }
    Ok(value)
}

pub(super) fn system(cfg: &Config) -> Value {
    let plugin_view = match plugins::PluginRegistry::load(cfg) {
        Ok(registry) => json!({
            "list": registry.list(),
            "verify": registry.verify(None).unwrap_or_else(|e| json!({"ok": false, "error": e})),
        }),
        Err(error) => json!({"error": error}),
    };
    json!({
        "mode": cfg.mode.as_str(),
        "sandbox": {
            "configured": cfg.sandbox_mode.as_str(),
            "network": cfg.sandbox_net.as_str(),
            "doctor": sandbox::doctor(),
            "check": sandbox::check(cfg),
        },
        "plugins": plugin_view,
        "mcp": {
            "contract": cfg.mcp_contract_path.as_ref().map(|p| p.display().to_string()),
            "delegation": cfg.mcp_delegation_path.as_ref().map(|p| p.display().to_string()),
            "contractKeys": cfg.mcp_contract_keys.len(),
            "delegationKeys": cfg.mcp_delegation_keys.len(),
            "receiptKeys": cfg.mcp_receipt_keys.len(),
            "agent": cfg.mcp_agent,
        },
        "keys": {"policyKeys": cfg.policy_keys.len(), "pluginKeys": cfg.plugin_keys.len(), "labKeys": cfg.lab_keys.len()},
    })
}

type NativeOp = (&'static str, &'static str);
type NativeTool = (&'static str, &'static str, &'static [NativeOp]);

const NATIVE_DOC: &[NativeTool] = &[
    (
        "index",
        "Hash eligible files and extract bounded symbol and import hints.",
        &[("build", ""), ("search", "QUERY")],
    ),
    (
        "context",
        "Pin files and assemble bounded task context.",
        &[("pin", "PATH"), ("assemble", "QUERY"), ("show", "")],
    ),
    (
        "planner",
        "Dependency-ordered plans with evidence per node.",
        &[
            ("create", "PLAN.json"),
            ("inspect", ""),
            ("report", "NODE EVIDENCE"),
            ("verify", "NODE"),
        ],
    ),
    (
        "pruner",
        "Preview or apply removal of stale native state.",
        &[("preview", ""), ("run", "")],
    ),
    (
        "checkpoint",
        "Task-scoped checkpoints for later resume.",
        &[("create", ""), ("inspect", ""), ("resume", "")],
    ),
    (
        "verifier",
        "Select and run verification checks.",
        &[("select", ""), ("run", "")],
    ),
    (
        "impact",
        "Estimate what a path touches.",
        &[("analyze", "PATH")],
    ),
    (
        "scheduler",
        "Choose the next ready plan node.",
        &[("next", "")],
    ),
    (
        "workspace",
        "Snapshot and roll back worktree state.",
        &[("status", ""), ("snapshot", ""), ("rollback", "")],
    ),
    (
        "evidence",
        "Record and check evidence files against plan nodes.",
        &[("record", "ID FILE"), ("check", "ID"), ("list", "")],
    ),
    (
        "optimizer",
        "Review recorded runs for deterministic suggestions.",
        &[("review", "")],
    ),
    (
        "simulator",
        "Deterministic simulation with a fixed default seed.",
        &[("run", "[SEED]")],
    ),
];

pub(super) fn native_catalog() -> Value {
    json!(NATIVE_DOC
        .iter()
        .map(|(tool, summary, ops)| json!({
            "tool": tool,
            "summary": summary,
            "operations": ops.iter().map(|(op, args)| {
                let mutating = jani_native::mutation(tool, op);
                let privileged = jani_native::privileged(tool, op);
                json!({
                    "name": op, "args": args, "mutating": mutating, "privileged": privileged,
                    "runnable": !mutating && !privileged,
                })
            }).collect::<Vec<_>>(),
        }))
        .collect::<Vec<_>>())
}

pub(super) fn native_run(cfg: &Config, body: &Value) -> JResult<Value> {
    let tool = body["tool"].as_str().unwrap_or("");
    let op = body["op"].as_str().unwrap_or("");
    if !jani_native::TOOLS.contains(&tool) {
        return Err("unsupported native tool".into());
    }
    if jani_native::mutation(tool, op) || jani_native::privileged(tool, op) {
        return Err(format!(
            "{tool} {op} changes state or is operator-only; run it with `jani native {tool} {op}`"
        ));
    }
    if !NATIVE_DOC
        .iter()
        .any(|(t, _, ops)| *t == tool && ops.iter().any(|(o, _)| *o == op))
    {
        return Err("unsupported native operation".into());
    }
    let args: Vec<String> = body["args"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    if args.len() > 6
        || args
            .iter()
            .any(|a| a.len() > 512 || a.chars().any(char::is_control))
    {
        return Err("at most 6 printable arguments of up to 512 bytes".into());
    }
    let mut next = cfg.clone();
    next.command = "native".into();
    next.sub = Some(tool.to_string());
    next.positional = std::iter::once(op.to_string()).chain(args).collect();
    crate::native::command(&next)
}

pub(super) fn updates_info(cfg: &Config) -> Value {
    json!({
        "version": env!("CARGO_PKG_VERSION"),
        "source": cfg.update_source,
        "targets": cfg.update_targets.iter().map(|p| p.display().to_string()).collect::<Vec<_>>(),
        "executable": std::env::current_exe().ok().map(|p| p.display().to_string()),
        "note": "Checking contacts the release source named above. Installing is not offered here; use `jani update install`.",
    })
}

pub(super) fn updates_check(cfg: &Config) -> JResult<Value> {
    if cfg.update_source.is_none() {
        return Err("no update source configured; set JANI_UPDATE_SOURCE or --source".into());
    }
    update::check_report(cfg)
}
