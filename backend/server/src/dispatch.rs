//! Event-driven webhook/schedule intake dispatcher (`jani dispatch`).
use crate::agent::Agent;
use crate::config::{Config, JResult};
use crate::data::redact_text;
use crate::identity;
use crate::policy::PolicyEngine;
use crate::skills::Catalog;
use crate::types::{sha256_hex, valid_id, WorkContract};
use jani_core::kill;
use jani_store::tasks::{EffectGate, TaskStore};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::fs;
use std::path::Path;

const MAX_EVENT_BYTES: u64 = 256 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IntakeEvent {
    pub source: String,
    pub event: String,
    pub reference: String,
    pub delivery_id: String,
    #[serde(default)]
    pub prompt: Option<String>,
    #[serde(default)]
    pub severity: Option<String>,
}

pub enum DispatchPlan {
    Deduplicated {
        task_id: String,
        idempotency_key: String,
        cached: Option<Value>,
    },
    Ready {
        tenant: String,
        task_id: String,
        idempotency_key: String,
        prompt: String,
        event: IntakeEvent,
    },
}

pub fn load_event(path: &Path) -> JResult<IntakeEvent> {
    let meta = fs::symlink_metadata(path)
        .map_err(|e| format!("cannot stat event {}: {e}", path.display()))?;
    if meta.file_type().is_symlink() || !meta.is_file() {
        return Err(format!(
            "event {} must be a regular non-symlink file",
            path.display()
        ));
    }
    if meta.len() > MAX_EVENT_BYTES {
        return Err(format!(
            "event {} exceeds {MAX_EVENT_BYTES} bytes",
            path.display()
        ));
    }
    let text = fs::read_to_string(path)
        .map_err(|e| format!("cannot read event {}: {e}", path.display()))?;
    let mut ev: IntakeEvent =
        serde_json::from_str(&text).map_err(|e| format!("invalid intake event JSON: {e}"))?;
    if !valid_id(&ev.source, 64) || !valid_id(&ev.delivery_id, 96) {
        return Err("event source and deliveryId must use safe identifier characters".into());
    }
    if ev.event.trim().is_empty() || ev.reference.trim().is_empty() || ev.reference.len() > 256 {
        return Err("event type and bounded reference are required".into());
    }
    if let Some(ref mut prompt) = ev.prompt {
        let (clean, _) = redact_text(prompt);
        *prompt = clean;
    }
    Ok(ev)
}

pub fn prepare(cfg: &Config, event_path: &Path, contract_path: &Path) -> JResult<DispatchPlan> {
    let ev = load_event(event_path)?;
    let _policy = PolicyEngine::load(cfg)?;
    let identity = identity::load(cfg)?;
    let contract = WorkContract::load(contract_path)?;
    if contract.metadata.tenant != identity.tenant
        || contract.metadata.principal != identity.principal
    {
        return Err("work contract tenant/principal does not match identity".into());
    }
    if let Some(rec) = kill::is_killed(&cfg.state_dir, &identity.tenant)? {
        return Err(format!(
            "KILL_SWITCH_ACTIVE: intake refused ({})",
            rec.reason
        ));
    }

    let task_id = contract.metadata.task_id.clone();
    let idempotency_key = format!("intake:{}:{}:{}", ev.source, ev.reference, ev.delivery_id);
    let payload_digest = sha256_hex(
        serde_json::to_string(&ev)
            .map_err(|e| e.to_string())?
            .as_bytes(),
    );

    let mut tasks = TaskStore::open(&cfg.state_dir)?;
    tasks.checkpoint(
        &identity.tenant,
        &task_id,
        1,
        "queued",
        None,
        None,
        &identity.principal,
        &contract.spec.purpose,
        &cfg.root.display().to_string(),
        &[],
    )?;
    match tasks.begin_effect(
        &identity.tenant,
        &task_id,
        1,
        &idempotency_key,
        "intake.dispatch",
        &ev.reference,
        &payload_digest,
    )? {
        EffectGate::AlreadyApplied(cached) => Ok(DispatchPlan::Deduplicated {
            task_id,
            idempotency_key,
            cached,
        }),
        EffectGate::InFlight => Err("duplicate webhook delivery is already in flight".into()),
        EffectGate::Proceed => {
            let (base_prompt, _) = redact_text(
                ev.prompt
                    .as_deref()
                    .filter(|s| !s.trim().is_empty())
                    .unwrap_or(
                        "Investigate and address the assigned event within work-contract bounds.",
                    ),
            );
            let bounded_prompt = format!(
                "Assigned event: source={} event={} reference={} delivery={}\n\nTask: {}",
                ev.source, ev.event, ev.reference, ev.delivery_id, base_prompt
            );
            Ok(DispatchPlan::Ready {
                tenant: identity.tenant,
                task_id,
                idempotency_key,
                prompt: bounded_prompt,
                event: ev,
            })
        }
    }
}

pub fn run(cfg: &Config, catalog: &Catalog) -> JResult<()> {
    let event_path = cfg
        .event_path
        .as_ref()
        .ok_or("dispatch requires --event FILE")?;
    let contract_path = cfg
        .contract_path
        .as_ref()
        .ok_or("dispatch requires --contract FILE")?;
    let plan = prepare(cfg, event_path, contract_path)?;
    match plan {
        DispatchPlan::Deduplicated {
            task_id,
            idempotency_key,
            cached,
        } => {
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "ok": true,
                    "deduplicated": true,
                    "taskId": task_id,
                    "idempotencyKey": idempotency_key,
                    "cached": cached,
                }))
                .map_err(|e| e.to_string())?
            );
            Ok(())
        }
        DispatchPlan::Ready {
            tenant,
            task_id,
            idempotency_key,
            prompt,
            event,
        } => {
            if cfg.sub.as_deref() == Some("dry-run") {
                let mut tasks = TaskStore::open(&cfg.state_dir)?;
                tasks.finish_effect(
                    &tenant,
                    &task_id,
                    &idempotency_key,
                    true,
                    Some(&json!({"queued": true, "event": event.event})),
                )?;
                println!(
                    "{}",
                    serde_json::to_string_pretty(&json!({
                        "ok": true,
                        "queued": true,
                        "taskId": task_id,
                        "idempotencyKey": idempotency_key,
                        "promptPreview": prompt,
                    }))
                    .map_err(|e| e.to_string())?
                );
                return Ok(());
            }
            let mut agent = Agent::new(cfg, catalog)?;
            let outcome = agent.run(&prompt);
            let mut tasks = TaskStore::open(&cfg.state_dir)?;
            match outcome {
                Ok(answer) => {
                    let _ = tasks.finish_effect(
                        &tenant,
                        &task_id,
                        &idempotency_key,
                        true,
                        Some(&json!({"completed": true})),
                    );
                    println!("{answer}");
                    Ok(())
                }
                Err(err) => {
                    let _ = tasks.finish_effect(
                        &tenant,
                        &task_id,
                        &idempotency_key,
                        false,
                        Some(&json!({"error": &err})),
                    );
                    Err(err)
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{new_id, now_unix, ContractBudget, ContractMetadata, ContractSpec};

    #[test]
    fn webhook_delivery_is_deduplicated_scrubbed_and_blocked_by_kill_switch() {
        let dir = std::env::temp_dir().join(format!("jani-dispatch-{}", new_id("t")));
        fs::create_dir_all(&dir).unwrap();
        let mut cfg = Config::baseline();
        cfg.root = dir.clone();
        cfg.state_dir = dir.join("state");
        cfg.tenant = "acme".into();
        cfg.principal = "bot-1".into();

        let contract = WorkContract {
            api_version: "jani.dev/v1alpha1".into(),
            kind: "WorkContract".into(),
            metadata: ContractMetadata {
                task_id: "task-intake-1".into(),
                tenant: "acme".into(),
                principal: "bot-1".into(),
                workspace: String::new(),
            },
            spec: ContractSpec {
                purpose: "coding".into(),
                capabilities: vec!["repo.read".into()],
                write_paths: vec!["src".into()],
                sandbox_profile: None,
                data_classification: None,
                budget: ContractBudget::default(),
                acceptance: vec![],
                acceptance_checks: vec![],
            },
        };
        let contract_path = dir.join("contract.json");
        fs::write(&contract_path, serde_json::to_vec(&contract).unwrap()).unwrap();

        let event_path = dir.join("event.json");
        let fake_secret = format!("{}AbcDef1234567890AbcDef1234567890AbCd", "ghp_");
        fs::write(
            &event_path,
            serde_json::to_vec(&json!({
                "source": "github",
                "event": "pull_request.opened",
                "reference": "acme/repo#42",
                "deliveryId": "deliv-100",
                "prompt": format!("Fix PR with token {fake_secret}"),
                "severity": "normal"
            }))
            .unwrap(),
        )
        .unwrap();

        let first = prepare(&cfg, &event_path, &contract_path).unwrap();
        let (tenant, task_id, key) = match first {
            DispatchPlan::Ready {
                tenant,
                task_id,
                idempotency_key,
                prompt,
                ..
            } => {
                assert!(!prompt.contains(&fake_secret));
                assert!(prompt.contains("[REDACTED"), "{prompt}");
                (tenant, task_id, idempotency_key)
            }
            _ => panic!("expected Ready"),
        };
        TaskStore::open(&cfg.state_dir)
            .unwrap()
            .finish_effect(&tenant, &task_id, &key, true, Some(&json!({"done": true})))
            .unwrap();

        // Duplicate delivery must be deduplicated cleanly.
        let second = prepare(&cfg, &event_path, &contract_path).unwrap();
        assert!(matches!(second, DispatchPlan::Deduplicated { .. }));

        // Activating kill switch must refuse new dispatches.
        kill::activate(&cfg.state_dir, Some("acme"), "sec-admin", "incident-9").unwrap();
        let ev2 = dir.join("event2.json");
        fs::write(
            &ev2,
            serde_json::to_vec(&json!({
                "source": "github",
                "event": "issue.opened",
                "reference": "acme/repo#43",
                "deliveryId": "deliv-101"
            }))
            .unwrap(),
        )
        .unwrap();
        let err = match prepare(&cfg, &ev2, &contract_path) {
            Ok(_) => panic!("kill switch should refuse prepare"),
            Err(err) => err,
        };
        assert!(err.contains("KILL_SWITCH_ACTIVE"), "{err}");
        let _ = now_unix();
        let _ = fs::remove_dir_all(&dir);
    }
}
