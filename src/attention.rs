//! Standalone, durable local lifecycle and alert protocol.
//! No coordinator, external bus, plugin executor or background LLM process.
//! The state is scoped to a local state directory and ONE trusted machine.

use crate::config::{Config, JResult};
use crate::identity;
use crate::policy::PolicyEngine;
use crate::types::{new_id, now_unix, sha256_hex, ActionRequest, Decision, Effect, RiskTier};
use jani_contract::{AgentPhase as Phase, AlertPhase};
use jani_store::{Store, TransitionEvent};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
#[cfg(test)]
use std::fs;

const VERSION: u32 = 1;
const MAX_ALERTS: usize = 10_000;
const MAX_STATE_BYTES: u64 = 8_000_000;
const LEASE_SECS: u64 = 60;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AgentRecord {
    tenant: String,
    agent_id: String,
    phase: Phase,
    generation: u64,
    wake_requested: bool,
    updated_at: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AlertRecord {
    id: String,
    tenant: String,
    recipient: String,
    sender: String,
    source: String,
    event: String,
    reference: String,
    severity: String,
    dedupe_key: String,
    phase: AlertPhase,
    created_at: u64,
    updated_at: u64,
    attempts: u32,
    next_attempt_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    lease: Option<Lease>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    evidence: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Lease {
    consumer: String,
    token: String,
    expires_at: u64,
}

#[derive(Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Snapshot {
    #[serde(default = "version")]
    version: u32,
    #[serde(default)]
    agents: BTreeMap<String, AgentRecord>,
    #[serde(default)]
    alerts: BTreeMap<String, AlertRecord>,
}

fn version() -> u32 {
    VERSION
}

fn key(tenant: &str, id: &str) -> String {
    format!("{tenant}/{id}")
}

fn identifier(name: &str, value: &str) -> JResult<()> {
    if value.is_empty()
        || value.len() > 96
        || !value.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
    {
        return Err(format!("{name} must contain 1..96 ASCII letters, digits, '.', '-' or '_'"));
    }
    Ok(())
}

fn argument<'a>(cfg: &'a Config, index: usize, name: &str) -> JResult<&'a str> {
    let value = cfg.positional.get(index).map(String::as_str)
        .ok_or_else(|| format!("missing {name} at argument {}", index + 1))?;
    identifier(name, value)?;
    Ok(value)
}

fn actor(cfg: &Config, action: &str, recipient: &str, risk: RiskTier, effect: Effect, own: bool) -> JResult<identity::VerifiedIdentity> {
    let id = identity::load(cfg)?;
    if cfg.is_enterprise() && (!id.verified || (own && id.principal != recipient)) {
        return Err("enterprise agent inbox operations require the recipient's verified identity".into());
    }
    let engine = PolicyEngine::load(cfg)?;
    let request = ActionRequest {
        task_id: cfg.task_id.clone().unwrap_or_else(|| "attention".into()),
        invocation_id: new_id("attention"),
        tenant_id: id.tenant.clone(),
        principal_id: id.principal.clone(),
        action: action.into(),
        resource: format!("agent:{recipient}"),
        effect,
        risk,
        arguments_digest: sha256_hex(format!("{action}:{recipient}").as_bytes()),
        idempotency_key: None,
    };
    let decision = engine.evaluate(cfg, &request);
    if decision.decision != Decision::Permit {
        return Err(format!("attention policy denied {action}: {}", decision.reason_code));
    }
    Ok(id)
}

// A single SQLite transaction commits the snapshot and transition event together.
// Legacy JSON snapshots are imported once on first access, without truncation.
fn transaction<F>(cfg: &Config, actor: &str, action: &str, f: F) -> JResult<Value>
where
    F: FnOnce(&mut Snapshot) -> JResult<(Value, bool)>,
{
    let mut store = Store::open(&cfg.state_dir)?;
    let legacy = cfg.state_dir.join("attention").join("state.json");
    let event = TransitionEvent {
        tenant: cfg.tenant.clone(),
        actor: actor.to_string(),
        action: action.to_string(),
        payload_digest: sha256_hex(format!("{action}:{actor}").as_bytes()),
    };
    store.transact(
        &legacy,
        Snapshot {
            version: VERSION,
            ..Default::default()
        },
        event,
        |snapshot| {
            if snapshot.version != VERSION {
                return Err("unsupported attention snapshot version".into());
            }
            f(snapshot)
        },
    )
}

fn lookup_agent<'a>(s: &'a mut Snapshot, tenant: &str, id: &str) -> JResult<&'a mut AgentRecord> {
    s.agents.get_mut(&key(tenant, id)).ok_or_else(|| format!("agent '{id}' is not registered"))
}

fn lookup_alert<'a>(s: &'a mut Snapshot, tenant: &str, agent: &str, alert: &str) -> JResult<&'a mut AlertRecord> {
    let value = s.alerts.get_mut(alert).ok_or("alert not found")?;
    if value.tenant != tenant || value.recipient != agent {
        return Err("alert not found in recipient inbox".into());
    }
    Ok(value)
}

pub fn command(cfg: &Config) -> JResult<Value> {
    let verb = cfg.sub.as_deref().ok_or("attention command needs a subcommand")?;
    if cfg.command == "wake" && verb == "watch" {
        // The watcher is only a lightweight poller; it never runs the model or
        // accepts a message as permission. Recheck identity and policy each tick.
        let mut next = cfg.clone();
        next.sub = Some("once".into());
        loop {
            let result = command(&next)?;
            if result.get("woke").and_then(Value::as_bool) == Some(true) {
                return Ok(result);
            }
            std::thread::sleep(std::time::Duration::from_secs(2));
        }
    }
    let agent = argument(cfg, 0, "agent id")?;
    let action = format!("attention.{}.{}", cfg.command, verb);
    let own = !matches!((cfg.command.as_str(), verb), ("agent", "register") | ("alerts", "send"));
    let effect = if matches!((cfg.command.as_str(), verb), ("agent", "status") | ("alerts", "list") | ("wake", "poll")) {
        Effect::Read
    } else {
        Effect::Control
    };
    let risk = if matches!((cfg.command.as_str(), verb), ("alerts", "send")) { RiskTier::R2 } else { RiskTier::R1 };
    let identity = actor(cfg, &action, agent, risk, effect, own)?;
    let tenant = identity.tenant;
    let principal = identity.principal;
    let now = now_unix();
    match (cfg.command.as_str(), verb) {
        ("agent", "register") => transaction(cfg, &principal, &action, |s| {
            let k = key(&tenant, agent);
            if s.agents.contains_key(&k) {
                return Err("agent already registered".into());
            }
            let record = AgentRecord {
                tenant: tenant.clone(),
                agent_id: agent.into(),
                phase: Phase::Ready,
                generation: 1,
                wake_requested: false,
                updated_at: now,
            };
            s.agents.insert(k, record.clone());
            Ok((json!(record), true))
        }),
        ("agent", "status") => transaction(cfg, &principal, &action, |s| {
            Ok((json!(lookup_agent(s, &tenant, agent)?), false))
        }),
        ("agent", "sleep") => transaction(cfg, &principal, &action, |s| {
            let a = lookup_agent(s, &tenant, agent)?;
            if a.phase != Phase::Ready {
                return Err("sleep requires ready state; active work must checkpoint separately".into());
            }
            if !a.phase.can_transition(Phase::Sleeping) {
                return Err("lifecycle transition is not allowed by contract".into());
            }
            a.phase = Phase::Sleeping;
            a.updated_at = now;
            a.generation += 1;
            Ok((json!(a), true))
        }),
        ("agent", "wake") => transaction(cfg, &principal, &action, |s| {
            let a = lookup_agent(s, &tenant, agent)?;
            if a.phase != Phase::Sleeping {
                return Err("wake requires sleeping state; paused/quarantined agents cannot wake".into());
            }
            if !a.phase.can_transition(Phase::Ready) {
                return Err("lifecycle transition is not allowed by contract".into());
            }
            a.phase = Phase::Ready;
            a.wake_requested = false;
            a.updated_at = now;
            a.generation += 1;
            Ok((json!(a), true))
        }),
        ("wake", "poll") => transaction(cfg, &principal, &action, |s| {
            let a = lookup_agent(s, &tenant, agent)?.clone();
            let pending = s.alerts.values().filter(|e| {
                e.tenant == tenant && e.recipient == agent && e.phase == AlertPhase::Pending
            }).count();
            Ok((json!({"phase": a.phase, "wakeRequested": a.wake_requested, "pending": pending, "generation": a.generation}), false))
        }),
        ("wake", "once") => transaction(cfg, &principal, &action, |s| {
            let a = lookup_agent(s, &tenant, agent)?;
            if a.phase != Phase::Sleeping || !a.wake_requested {
                return Ok((json!({"woke":false, "phase":a.phase, "generation":a.generation}), false));
            }
            a.phase = Phase::Ready;
            a.wake_requested = false;
            a.updated_at = now;
            a.generation += 1;
            Ok((json!({"woke":true,"phase":a.phase,"generation":a.generation}),true))
        }),
        ("alerts", "send") => {
            let event = argument(cfg, 1, "event")?;
            let source = argument(cfg, 2, "source")?;
            let reference = argument(cfg, 3, "reference")?;
            let severity = argument(cfg, 4, "severity")?;
            if !matches!(severity, "info" | "normal" | "high" | "critical") {
                return Err("severity must be info, normal, high or critical".into());
            }
            let dedupe = argument(cfg, 5, "idempotency key")?;
            transaction(cfg, &principal, &action, |s| {
                let _ = lookup_agent(s, &tenant, agent)?;
                if let Some(a) = s.alerts.values().find(|a| a.tenant == tenant && a.sender == principal && a.dedupe_key == dedupe) {
                    if a.source != source || a.event != event || a.reference != reference || a.recipient != agent || a.severity != severity {
                        return Err("idempotency key reused with different alert contents".into());
                    }
                    return Ok((json!({"alert":a,"replayed":true}), false));
                }
                if s.alerts.len() >= MAX_ALERTS {
                    return Err("attention store full; archive resolved alerts first".into());
                }
                let a = AlertRecord {
                    id: new_id("alert"),
                    tenant: tenant.clone(),
                    recipient: agent.into(),
                    sender: principal.clone(),
                    source: source.into(),
                    event: event.into(),
                    reference: reference.into(),
                    severity: severity.into(),
                    dedupe_key: dedupe.into(),
                    phase: AlertPhase::Pending,
                    created_at: now,
                    updated_at: now,
                    attempts: 0,
                    next_attempt_at: now,
                    lease: None,
                    evidence: None,
                };
                if matches!(severity, "high" | "critical") {
                    let agent_record = lookup_agent(s, &tenant, agent)?;
                    if agent_record.phase == Phase::Sleeping {
                        agent_record.wake_requested = true;
                    }
                }
                s.alerts.insert(a.id.clone(), a.clone());
                Ok((json!({"alert":a,"replayed":false}), true))
            })
        }
        ("alerts", "list") => transaction(cfg, &principal, &action, |s| {
            let _ = lookup_agent(s, &tenant, agent)?;
            let alerts = s.alerts.values().filter(|a| a.tenant == tenant && a.recipient == agent)
                .map(|a| json!({
                    "id":a.id,"event":a.event,"source":a.source,"reference":a.reference,
                    "severity":a.severity,"phase":a.phase,"attempts":a.attempts,
                    "createdAt":a.created_at,"evidence":a.evidence
                })).collect::<Vec<_>>();
            Ok((json!(alerts), false))
        }),
        ("alerts", "claim") => transaction(cfg, &principal, &action, |s| {
            let state = lookup_agent(s, &tenant, agent)?;
            if !matches!(state.phase, Phase::Ready | Phase::Running) {
                return Err("agent must be ready before claiming an alert".into());
            }
            let candidate = s.alerts.values_mut().filter(|a|
                a.tenant == tenant && a.recipient == agent
                && (a.phase == AlertPhase::Pending || (a.phase == AlertPhase::Leased && a.lease.as_ref().is_some_and(|l| l.expires_at <= now)))
                && a.next_attempt_at <= now
            ).min_by_key(|a| (a.created_at, a.id.clone()));
            if let Some(a) = candidate {
                a.attempts = a.attempts.saturating_add(1);
                if !a.phase.can_transition(AlertPhase::Leased) {
                    return Err("alert transition is not allowed by contract".into());
                }
                a.phase = AlertPhase::Leased;
                a.updated_at = now;
                a.lease = Some(Lease { consumer: principal.clone(), token: new_id("receipt"), expires_at: now + LEASE_SECS });
                return Ok((json!({"alert":a}), true));
            }
            Ok((json!({"alert":null}), false))
        }),
        ("alerts", "accept") | ("alerts", "retry") => {
            let alert = argument(cfg, 1, "alert id")?;
            let receipt = argument(cfg, 2, "receipt token")?;
            transaction(cfg, &principal, &action, |s| {
                let a = lookup_alert(s, &tenant, agent, alert)?;
                if a.phase != AlertPhase::Leased {
                    return Err("alert is not leased".into());
                }
                let lease = a.lease.as_ref().ok_or("alert lease missing")?;
                if lease.consumer != principal || lease.token != receipt || lease.expires_at <= now {
                    return Err("stale or incorrect receipt; refusing action".into());
                }
                if verb == "accept" {
                    if !a.phase.can_transition(AlertPhase::Accepted) {
                        return Err("alert transition is not allowed by contract".into());
                    }
                    a.phase = AlertPhase::Accepted;
                    a.lease = None;
                } else {
                    if !a.phase.can_transition(AlertPhase::Pending) {
                        return Err("alert retry transition is not allowed by contract".into());
                    }
                    a.phase = AlertPhase::Pending;
                    a.lease = None;
                    let backoff = 1u64 << a.attempts.min(8);
                    a.next_attempt_at = now.saturating_add(backoff.min(300));
                }
                a.updated_at = now;
                Ok((json!(a), true))
            })
        }
        ("alerts", "resolve") => {
            let alert = argument(cfg, 1, "alert id")?;
            let evidence = argument(cfg, 2, "evidence reference")?;
            transaction(cfg, &principal, &action, |s| {
                let a = lookup_alert(s, &tenant, agent, alert)?;
                if a.phase != AlertPhase::Accepted {
                    return Err("resolve requires an accepted alert; verification is external".into());
                }
                if !a.phase.can_transition(AlertPhase::Resolved) {
                    return Err("alert resolution transition is not allowed by contract".into());
                }
                a.phase = AlertPhase::Resolved;
                a.evidence = Some(evidence.into());
                a.updated_at = now;
                Ok((json!(a), true))
            })
        }
        _ => Err(format!("unsupported {} subcommand '{}'", cfg.command, verb)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::RuntimeMode;

    fn fixture() -> Config {
        let mut cfg = Config::baseline();
        let dir = std::env::temp_dir().join(format!("jani-attention-{}", new_id("test")));
        fs::create_dir_all(&dir).unwrap();
        cfg.root = dir.clone();
        cfg.state_dir = dir.join(".jani");
        cfg.mode = RuntimeMode::LocalUnmanaged;
        cfg.tenant = "sample".into();
        cfg.principal = "operator".into();
        cfg
    }

    fn call(cfg: &mut Config, command_name: &str, sub: &str, args: &[&str]) -> JResult<Value> {
        cfg.command = command_name.into();
        cfg.sub = Some(sub.into());
        cfg.positional = args.iter().map(|x| (*x).into()).collect();
        command(cfg)
    }

    #[test]
    fn sleep_publish_wake_and_resolve() {
        let mut cfg = fixture();
        call(&mut cfg, "agent", "register", &["worker"]).unwrap();
        call(&mut cfg, "agent", "sleep", &["worker"]).unwrap();
        let first = call(&mut cfg, "alerts", "send", &["worker","payment.failed","bank","case-42","high","key-1"]).unwrap();
        assert_eq!(first["replayed"], false);
        assert_eq!(call(&mut cfg, "wake", "poll", &["worker"]).unwrap()["wakeRequested"], true);
        assert_eq!(call(&mut cfg, "wake", "once", &["worker"]).unwrap()["woke"], true);
        let claimed = call(&mut cfg, "alerts", "claim", &["worker"]).unwrap();
        let id = claimed["alert"]["id"].as_str().unwrap().to_string();
        let receipt = claimed["alert"]["lease"]["token"].as_str().unwrap().to_string();
        call(&mut cfg, "alerts", "accept", &["worker",&id,&receipt]).unwrap();
        assert!(call(&mut cfg, "alerts", "accept", &["worker",&id,&receipt]).is_err());
        let done = call(&mut cfg, "alerts", "resolve", &["worker",&id,"evidence-123"]).unwrap();
        assert_eq!(done["phase"], "resolved");
        let _ = fs::remove_dir_all(&cfg.root);
    }

    #[test]
    fn dedupe_is_scoped_and_conflicts_fail() {
        let mut cfg = fixture();
        call(&mut cfg, "agent", "register", &["a"]).unwrap();
        let args = ["a", "build.failed", "ci", "job-10", "normal", "dedupe-1"];
        let one = call(&mut cfg, "alerts", "send", &args).unwrap();
        let two = call(&mut cfg, "alerts", "send", &args).unwrap();
        assert_eq!(one["alert"]["id"], two["alert"]["id"]);
        assert_eq!(two["replayed"], true);
        assert!(call(&mut cfg, "alerts", "send", &["a","build.failed","ci","job-11","normal","dedupe-1"]).is_err());
        let _ = fs::remove_dir_all(&cfg.root);
    }

    #[test]
    fn stale_receipts_cannot_ack_after_reclaim() {
        let mut cfg = fixture();
        call(&mut cfg, "agent", "register", &["a"]).unwrap();
        call(&mut cfg, "alerts", "send", &["a","job.failed","ci","job-10","high","k"]).unwrap();
        let first = call(&mut cfg, "alerts", "claim", &["a"]).unwrap();
        let id = first["alert"]["id"].as_str().unwrap().to_string();
        let stale = first["alert"]["lease"]["token"].as_str().unwrap().to_string();
        transaction(&cfg,"test","test", |s| {
            let a = s.alerts.get_mut(&id).unwrap();
            a.lease.as_mut().unwrap().expires_at = 0;
            Ok((json!({"forced":"expiry"}), true))
        }).unwrap();
        let second = call(&mut cfg, "alerts", "claim", &["a"]).unwrap();
        let fresh = second["alert"]["lease"]["token"].as_str().unwrap();
        assert_ne!(stale, fresh);
        assert!(call(&mut cfg, "alerts", "accept", &["a",&id,&stale]).is_err());
        let _ = fs::remove_dir_all(&cfg.root);
    }

    #[test]
    fn quarantined_agent_cannot_be_woken() {
        let mut cfg = fixture();
        call(&mut cfg, "agent", "register", &["a"]).unwrap();
        transaction(&cfg,"test","test", |s| {
            lookup_agent(s, "sample", "a")?.phase = Phase::Quarantined;
            Ok((json!({"ok":true}), true))
        }).unwrap();
        assert!(call(&mut cfg, "agent", "wake", &["a"]).is_err());
        let _ = fs::remove_dir_all(&cfg.root);
    }

    #[test]
    fn corrupt_state_fails_closed() {
        let mut cfg = fixture();
        call(&mut cfg, "agent", "register", &["a"]).unwrap();
        fs::write(cfg.state_dir.join("attention/state.json"), b"{invalid").unwrap();
        assert!(call(&mut cfg, "agent", "status", &["a"]).is_err());
        let _ = fs::remove_dir_all(&cfg.root);
    }
}
