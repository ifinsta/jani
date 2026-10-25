//! Mandatory policy / identity / journal bridge for all native commands.
use crate::config::{Config, JResult};
use crate::{identity, journal, policy, types};
use serde_json::Value;
use types::{ActionRequest, Classification, Decision, Effect, RiskTier};

fn record(
    cfg: &Config,
    tenant: &str,
    principal: &str,
    action: &str,
    op: &str,
    phase: &str,
    args_hash: &str,
) -> JResult<()> {
    let mut journal = journal::Journal::open(&cfg.state_dir)?;
    journal.record(journal::EventInput {
        tenant_id: tenant,
        task_id: cfg.task_id.as_deref().unwrap_or("default"),
        run_id: "native-tool",
        invocation_id: "native-tool",
        actor_id: principal,
        worker_id: "jani-native",
        action,
        resource_digest: args_hash,
        decision: phase,
        reason_code: op,
        policy_revision: "native-tool-v1",
        approval_id: None,
        sandbox_receipt_id: None,
        data_classification: Classification::Internal,
    })?;
    Ok(())
}
pub fn command(cfg: &Config) -> JResult<Value> {
    let tool = cfg.sub.as_deref().ok_or("native tool name required")?;
    if !jani_native::TOOLS.contains(&tool) {
        return Err("unsupported native tool".into());
    }
    let op = cfg
        .positional
        .first()
        .map(String::as_str)
        .ok_or("native operation required")?;
    if cfg.positional.len() > 7 {
        return Err("too many native arguments".into());
    }
    let mutating = jani_native::mutation(tool, op);
    let identity = identity::load(cfg)?;
    let policy = policy::PolicyEngine::load(cfg)?;
    let task = cfg.task_id.as_deref().unwrap_or("default");
    let action = format!("native.{tool}.{op}");
    let digest = types::sha256_hex(
        serde_json::to_string(&cfg.positional)
            .map_err(|e| e.to_string())?
            .as_bytes(),
    );
    let request = ActionRequest {
        task_id: task.into(),
        invocation_id: types::new_id("native"),
        tenant_id: identity.tenant.clone(),
        principal_id: identity.principal.clone(),
        action: action.clone(),
        resource: format!("native:{}:{}", identity.tenant, task),
        effect: if mutating {
            Effect::Control
        } else {
            Effect::Read
        },
        risk: if mutating { RiskTier::R1 } else { RiskTier::R0 },
        arguments_digest: digest.clone(),
        idempotency_key: None,
    };
    let result = policy.evaluate(cfg, &request);
    if result.decision != Decision::Permit {
        return Err(format!(
            "native operation not authorized: {}",
            result.reason_code
        ));
    }
    let human = cfg.approve && identity.verified && identity.subject_type == "human";
    if tool == "planner" && op == "verify" && !human {
        return Err(
            "native planner verification requires signed human identity and --approve".into(),
        );
    }
    // If intent cannot be durably audited, nothing is executed.
    record(
        cfg,
        &identity.tenant,
        &identity.principal,
        &action,
        op,
        "started",
        &digest,
    )?;
    let value = jani_native::run(
        &cfg.root,
        &cfg.state_dir,
        &identity.tenant,
        task,
        tool,
        &cfg.positional,
        human,
    )?;
    record(
        cfg,
        &identity.tenant,
        &identity.principal,
        &action,
        op,
        "completed",
        &digest,
    )?;
    Ok(value)
}
