pub use jani_core::{
    approvals, budget, config, crypto, data, env, evidence, identity, journal, net, permissions,
    policy, process, recovery, schema, tools, types,
};
mod agent;
mod attention;
mod broker;
mod consent;
mod deliver;
mod eval;
mod exec;
mod execpolicy;
mod gateway;
mod http;
mod lab;
mod mcp;
mod memory;
mod mirror;
mod model;
mod native;
mod plugins;
mod repl;
mod sandbox;
mod skills;
mod tui;
mod ui;
mod update;
mod work;
mod workspace;

use config::{Config, JResult};
use policy::{action_profile, PolicyEngine};
use serde_json::json;
use skills::Catalog;
use std::path::Path;
use types::{new_id, ActionRequest};

fn main() {
    if let Err(err) = run() {
        eprintln!("jani: {err}");
        std::process::exit(1);
    }
}

fn print_json(value: &serde_json::Value) -> JResult<()> {
    println!(
        "{}",
        serde_json::to_string_pretty(value).map_err(|e| e.to_string())?
    );
    Ok(())
}

fn run() -> JResult<()> {
    let cfg = Config::parse()?;
    if cfg.command == "version" {
        println!("jani {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    if cfg.command == "help" {
        println!("{}", config::help());
        return Ok(());
    }
    // Mirror works on recorded evidence without model credentials or skill discovery.
    if cfg.command == "mirror" {
        return print_json(&mirror::command(&cfg)?);
    }
    if cfg.command == "lab" {
        return print_json(&lab::command(&cfg)?);
    }
    if cfg.command == "mcp" {
        return print_json(&mcp::command(&cfg)?);
    }
    if cfg.command == "native" {
        return print_json(&native::command(&cfg)?);
    }
    // Crash triage must not depend on available skills, model credentials or providers.
    if cfg.command == "recovery" {
        return match cfg.sub.as_deref() {
            None | Some("status") => print_json(&recovery::status(&cfg.state_dir)?),
            Some(other) => Err(format!("unknown recovery subcommand '{other}'")),
        };
    }
    if cfg.command == "evidence" {
        let run_id = cfg
            .run_id
            .as_ref()
            .or_else(|| cfg.positional.first())
            .ok_or("evidence requires a run ID")?;
        return print_json(&evidence::receipt(&cfg.state_dir, run_id)?);
    }
    // Delivery operates on local binaries and a release origin, not on a worktree
    // agent session, so it must not require model credentials or skill discovery.
    if cfg.command == "update" {
        return update::command(&cfg);
    }
    let catalog = Catalog::load(&cfg)?;
    match cfg.command.as_str() {
        "skills" => {
            println!("{}", catalog.listing());
            Ok(())
        }
        "doctor" => doctor(&cfg, &catalog),
        "run" => {
            let prompt = cfg.prompt.as_ref().ok_or("no prompt supplied")?;
            let mut agent = agent::Agent::new(&cfg, &catalog)?;
            // With a person at the terminal, an unclassified command is put to them
            // (on stderr) instead of being refused; with none, it is refused.
            if consent::TerminalConsent::available() {
                agent.set_consent(std::sync::Arc::new(consent::TerminalConsent));
            }
            println!("{}", agent.run(prompt)?);
            Ok(())
        }
        "repl" => repl::run(&cfg, &catalog),
        "ui" => ui::command(&cfg, &catalog),
        "sandbox" => match cfg.sub.as_deref() {
            Some("doctor") | None => print_json(&sandbox::doctor()),
            Some("check") => print_json(&sandbox::check(&cfg)),
            Some(other) => Err(format!("unknown sandbox subcommand '{other}'")),
        },
        "plugins" => plugins_command(&cfg),
        "policy" => policy_command(&cfg),
        "exec" => exec_command(&cfg),
        "deliver" => deliver::command(&cfg, &catalog),
        "eval" => eval::command(&cfg, &catalog),
        "memory" => memory_command(&cfg),
        "work" => work::command(&cfg, &catalog),
        "audit" => audit_command(&cfg),
        "inspect" => inspect_command(&cfg),
        "approvals" => approvals_command(&cfg),
        "budget" => budget_command(&cfg),
        "identity" => identity_command(&cfg),
        "contract" => contract_command(&cfg),
        "agent" | "alerts" | "wake" => print_json(&attention::command(&cfg)?),
        _ => Err("unrecognized command".into()),
    }
}

fn doctor(cfg: &Config, catalog: &Catalog) -> JResult<()> {
    let identity = identity::load(cfg);
    let policy = PolicyEngine::load(cfg);
    let plugins = plugins::PluginRegistry::load(cfg);
    println!("Jani v{}", env!("CARGO_PKG_VERSION"));
    println!("Worktree: {}", cfg.root.display());
    println!(
        "Mode: {} (tenant {}, principal {})",
        cfg.mode.as_str(),
        cfg.tenant,
        cfg.principal
    );
    println!(
        "Identity: {}",
        match &identity {
            Ok(identity) => format!(
                "{} verified={} issuer={}",
                identity.principal, identity.verified, identity.issuer
            ),
            Err(error) => format!("ERROR: {error}"),
        }
    );
    println!(
        "Policy: {}",
        match &policy {
            Ok(policy) => policy.describe(),
            Err(error) => format!("ERROR: {error}"),
        }
    );
    println!("Discovered {} skills", catalog.skills.len());
    println!(
        "Plugins: {}",
        match &plugins {
            Ok(registry) => {
                if registry.plugins.is_empty() {
                    "none configured".to_string()
                } else {
                    format!(
                        "{} verified ({})",
                        registry.plugins.len(),
                        registry.capability_summary()
                    )
                }
            }
            Err(error) => format!("ERROR: {error}"),
        }
    );
    println!(
        "Sandbox mode: {}; network: {}",
        cfg.sandbox_mode.as_str(),
        cfg.sandbox_net.as_str()
    );
    let backends = sandbox::doctor();
    if let Some(items) = backends
        .get("backends")
        .and_then(serde_json::Value::as_array)
    {
        let available = items
            .iter()
            .filter(|item| item.get("available").and_then(serde_json::Value::as_bool) == Some(true))
            .filter_map(|item| item.get("name").and_then(serde_json::Value::as_str))
            .collect::<Vec<_>>()
            .join(", ");
        println!("Sandbox backends: {available}");
    }
    println!(
        "Settings: {}",
        match (&cfg.settings_path, &cfg.settings_profile) {
            (Some(path), Some(profile)) => format!("{} (profile {profile})", path.display()),
            (Some(path), None) => path.display().to_string(),
            (None, _) => "none".to_string(),
        }
    );
    for warning in &cfg.settings_warnings {
        println!("Settings warning: {warning}");
    }
    println!("Model: {}", cfg.model);
    println!(
        "Model API: {} {} (output ceiling {} tokens)",
        cfg.protocol.as_str(),
        cfg.api_base,
        cfg.max_output_tokens
    );
    println!(
        "Model key: {} ({})",
        if cfg.api_key.is_some() {
            "SET"
        } else {
            "MISSING"
        },
        cfg.api_key_source
    );
    println!(
        "Permissions: {}",
        cfg.permissions
            .as_ref()
            .map(permissions::Policy::describe)
            .unwrap_or_else(|| "none (CLI grants and policy only)".into())
    );
    println!("Writes: {}; commands: {}", cfg.allow_write, cfg.allow_exec);
    println!(
        "Write scope: {}",
        if cfg.write_paths.is_empty() {
            "none".to_string()
        } else {
            cfg.write_paths
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        }
    );
    let journal_path = cfg.state_dir.join(journal::JOURNAL_FILE);
    if journal_path.is_file() {
        let verification = journal::verify(&cfg.state_dir)?;
        println!(
            "Audit journal: {} events, chain_ok={}",
            verification
                .get("events")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0),
            verification
                .get("ok")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false)
        );
    } else {
        println!("Audit journal: not created yet");
    }
    Ok(())
}

fn plugins_command(cfg: &Config) -> JResult<()> {
    let registry = plugins::PluginRegistry::load(cfg)?;
    match cfg.sub.as_deref() {
        Some("list") | None => print_json(&registry.list()),
        Some("inspect") => {
            let id = cfg
                .positional
                .first()
                .ok_or("plugins inspect requires a plugin id")?;
            print_json(&registry.inspect(id)?)
        }
        Some("verify") => print_json(&registry.verify(cfg.positional.first().map(String::as_str))?),
        Some(other) => Err(format!("unknown plugins subcommand '{other}'")),
    }
}

pub(crate) fn policy_explain(cfg: &Config) -> JResult<serde_json::Value> {
    let engine = PolicyEngine::load(cfg)?;
    let bundle = engine.effective.bundle.as_ref();
    Ok(json!({
        "summary": engine.describe(),
        "revision": engine.effective.revision,
        "tenant": engine.effective.tenant,
        "defaultSandboxProfile": engine.default_sandbox_profile().as_str(),
        "defaultNetwork": engine.default_network().as_str(),
        "data": engine.data_policy(),
        "budgets": engine.budget_limits(),
        "rules": bundle.map(|bundle| bundle.spec.rules.clone()).unwrap_or_default(),
        "deniedActions": bundle.map(|bundle| bundle.spec.denied_actions.clone()).unwrap_or_default(),
    }))
}

pub(crate) fn policy_check(
    cfg: &Config,
    action: &str,
    resource: &str,
) -> JResult<serde_json::Value> {
    let engine = PolicyEngine::load(cfg)?;
    let identity = identity::load(cfg)?;
    let (effect, risk) = action_profile(action);
    let request = ActionRequest {
        task_id: cfg.task_id.clone().unwrap_or_else(|| new_id("task")),
        invocation_id: new_id("inv"),
        tenant_id: identity.tenant.clone(),
        principal_id: identity.principal.clone(),
        action: action.to_string(),
        resource: resource.to_string(),
        effect,
        risk,
        arguments_digest: "policy-check".into(),
        idempotency_key: None,
    };
    serde_json::to_value(engine.evaluate(cfg, &request)).map_err(|e| e.to_string())
}

fn policy_command(cfg: &Config) -> JResult<()> {
    match cfg.sub.as_deref() {
        Some("explain") | None => print_json(&policy_explain(cfg)?),
        Some("check") => {
            let action = cfg
                .action
                .as_ref()
                .ok_or("policy check requires --action")?;
            let resource = cfg
                .resource
                .as_ref()
                .ok_or("policy check requires --resource")?;
            print_json(&policy_check(cfg, action, resource)?)
        }
        Some(other) => Err(format!("unknown policy subcommand '{other}'")),
    }
}

/// `jani exec rules | allow -- CMD | deny -- CMD | forget N | check -- CMD`
fn exec_command(cfg: &Config) -> JResult<()> {
    let argv = || -> JResult<Vec<String>> {
        if cfg.positional.is_empty() {
            return Err(
                "give the command after `--`, e.g. `jani exec allow -- cargo build`".into(),
            );
        }
        Ok(cfg.positional.clone())
    };
    let show = |rule: &execpolicy::Rule| {
        serde_json::json!({
            "effect": if rule.allow { "allow" } else { "deny" },
            "command": rule.argv.join(" "),
            "argv": rule.argv,
            "addedBy": rule.by,
            "added": rule.added,
        })
    };
    match cfg.sub.as_deref() {
        Some("rules") | None => {
            let rules = execpolicy::load_rules(&cfg.state_dir)?;
            print_json(&serde_json::json!({
                "mode": cfg.exec_mode.as_str(),
                "file": cfg.state_dir.join("exec-rules.json"),
                "rules": rules.iter().enumerate().map(|(i, r)| {
                    let mut v = show(r);
                    v["number"] = (i + 1).into();
                    v
                }).collect::<Vec<_>>(),
            }))
        }
        Some(which @ ("allow" | "deny")) => {
            let rule =
                execpolicy::add_rule(&cfg.state_dir, which == "allow", argv()?, &cfg.principal)?;
            print_json(&show(&rule))
        }
        Some("forget") => {
            let number: usize = cfg
                .positional
                .first()
                .and_then(|n| n.parse().ok())
                .ok_or("forget needs a rule number from `jani exec rules`")?;
            print_json(&show(&execpolicy::remove_rule(&cfg.state_dir, number)?))
        }
        Some("check") => {
            let argv = argv()?;
            let rules = execpolicy::load_rules(&cfg.state_dir)?;
            let profile = execpolicy::default_profile(cfg);
            let (confined, network_cut) = execpolicy::confinement(cfg, profile, cfg.sandbox_net);
            let verdict = execpolicy::classify(
                &argv,
                &execpolicy::Context {
                    cfg,
                    confined,
                    network_cut,
                    rules: &rules,
                    session: &[],
                },
            );
            let (outcome, why, suggest) = match verdict {
                execpolicy::Verdict::Allow(basis) => ("runs", basis.code().to_string(), Vec::new()),
                execpolicy::Verdict::Ask { reason, suggest } => {
                    ("asks the operator", reason, suggest)
                }
                execpolicy::Verdict::Deny { reason } => ("refused", reason, Vec::new()),
            };
            print_json(&serde_json::json!({
                "command": argv.join(" "),
                "outcome": outcome,
                "why": why,
                "wouldRemember": suggest.join(" "),
                "mode": cfg.exec_mode.as_str(),
                "sandbox": {"profile": profile.as_str(), "confined": confined, "networkCut": network_cut},
            }))
        }
        Some(other) => Err(format!("unknown exec subcommand '{other}'")),
    }
}

/// `jani memory list | add TOPIC NOTE | forget ID`
fn memory_command(cfg: &Config) -> JResult<()> {
    let note_json = |n: &memory::Note| serde_json::json!({"id": n.id, "topic": n.topic, "note": n.note, "by": n.by, "ts": n.ts});
    match cfg.sub.as_deref() {
        Some("list") | None => print_json(&serde_json::json!({
            "file": cfg.state_dir.join("memory.jsonl"),
            "notes": memory::load(&cfg.state_dir).iter().map(note_json).collect::<Vec<_>>(),
        })),
        Some("add") => {
            let topic = cfg
                .positional
                .first()
                .ok_or("usage: jani memory add TOPIC NOTE")?;
            let note = cfg
                .positional
                .get(1)
                .ok_or("usage: jani memory add TOPIC NOTE")?;
            print_json(&note_json(&memory::add(
                &cfg.state_dir,
                topic,
                note,
                "operator",
            )?))
        }
        Some("forget") => {
            let id = cfg
                .positional
                .first()
                .and_then(|n| n.parse().ok())
                .ok_or("usage: jani memory forget ID")?;
            print_json(&note_json(&memory::forget(&cfg.state_dir, id)?))
        }
        Some(other) => Err(format!("unknown memory subcommand '{other}'")),
    }
}

fn audit_command(cfg: &Config) -> JResult<()> {
    match cfg.sub.as_deref() {
        Some("verify") | None => {
            let result = journal::verify(&cfg.state_dir)?;
            print_json(&result)?;
            if result.get("ok").and_then(serde_json::Value::as_bool) != Some(true) {
                return Err("audit journal verification failed".into());
            }
            Ok(())
        }
        Some(other) => Err(format!("unknown audit subcommand '{other}'")),
    }
}

fn inspect_command(cfg: &Config) -> JResult<()> {
    let run_id = cfg
        .run_id
        .clone()
        .or_else(|| cfg.positional.first().cloned())
        .ok_or("inspect requires a run id (--run or positional)")?;
    let integrity = journal::verify(&cfg.state_dir)?;
    if integrity["ok"] != true {
        return Err("audit journal integrity failed: refusing unverified inspection".into());
    }
    let path = cfg.state_dir.join(journal::JOURNAL_FILE);
    let events = journal::read_run(&path, &run_id)?;
    if events.is_empty() {
        return Err(format!("no journal events for run '{run_id}'"));
    }
    print_json(&json!(events))
}

fn approvals_command(cfg: &Config) -> JResult<()> {
    let mut store = approvals::ApprovalStore::open(&cfg.state_dir)?;
    match cfg.sub.as_deref() {
        Some("list") | None => print_json(&store.to_json()),
        Some("decide") => {
            let id = cfg
                .positional
                .first()
                .ok_or("approvals decide requires an approval id")?;
            if cfg.approve == cfg.reject {
                return Err("exactly one of --approve or --reject is required".into());
            }
            let identity = identity::load(cfg)?;
            if !identity.verified {
                return Err("approval decisions require a verified identity assertion".into());
            }
            let role = cfg
                .role
                .as_ref()
                .ok_or("--role is required to record an approval decision")?;
            let decided = store.decide(id, &identity.principal, role, cfg.approve)?;
            print_json(&serde_json::to_value(decided).map_err(|e| e.to_string())?)
        }
        Some(other) => Err(format!("unknown approvals subcommand '{other}'")),
    }
}

fn budget_command(cfg: &Config) -> JResult<()> {
    match cfg.sub.as_deref() {
        Some("show") | None => {
            let task = cfg.task_id.as_ref().ok_or("budget show requires --task")?;
            print_json(&budget::show(&cfg.state_dir, task)?)
        }
        Some(other) => Err(format!("unknown budget subcommand '{other}'")),
    }
}

fn identity_command(cfg: &Config) -> JResult<()> {
    match cfg.sub.as_deref() {
        Some("whoami") | None => {
            let identity = identity::load(cfg)?;
            print_json(&json!({
                "principal": identity.principal,
                "tenant": identity.tenant,
                "subjectType": identity.subject_type,
                "issuer": identity.issuer,
                "verified": identity.verified,
                "expiresAt": identity.expires_at,
            }))
        }
        Some(other) => Err(format!("unknown identity subcommand '{other}'")),
    }
}

fn contract_command(cfg: &Config) -> JResult<()> {
    match cfg.sub.as_deref() {
        Some("validate") | None => {
            let path = cfg
                .positional
                .first()
                .ok_or("contract validate requires a JSON file path")?;
            let contract = types::WorkContract::load(Path::new(path))?;
            for write_path in &contract.spec.write_paths {
                config::validate_relative(Path::new(write_path))?;
            }
            print_json(&json!({
                "ok": true,
                "contract": contract,
            }))
        }
        Some(other) => Err(format!("unknown contract subcommand '{other}'")),
    }
}
