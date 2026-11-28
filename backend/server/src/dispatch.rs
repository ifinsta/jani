//! Event-driven webhook/schedule intake dispatcher (`jani dispatch`).
use crate::agent::{Agent, SessionAuthority};
use crate::config::{Config, JResult};
use crate::data::redact_text;
use crate::skills::Catalog;
use crate::types::{sha256_hex, valid_id};
use jani_store::tasks::{DispatchClaim, DispatchRequest, TaskStore};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::fs::OpenOptions;
use std::io::Read;
use std::path::Path;

const MAX_EVENT_BYTES: u64 = 256 * 1024;

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct IntakeEvent {
    source: String,
    event: String,
    reference: String,
    delivery_id: String,
    #[serde(default)]
    prompt: Option<String>,
    #[serde(default)]
    severity: Option<String>,
}

impl IntakeEvent {
    fn load(path: &Path) -> JResult<Self> {
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
            options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
        }
        let file = options
            .open(path)
            .map_err(|e| format!("cannot open event {}: {e}", path.display()))?;
        let metadata = file
            .metadata()
            .map_err(|e| format!("cannot stat event: {e}"))?;
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;
            if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
                return Err("event must be a regular non-link file".into());
            }
        }
        if !metadata.is_file() {
            return Err("event must be a regular non-link file".into());
        }
        let mut bytes = Vec::new();
        file.take(MAX_EVENT_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| format!("cannot read event: {e}"))?;
        if bytes.len() as u64 > MAX_EVENT_BYTES {
            return Err(format!("event exceeds {MAX_EVENT_BYTES} bytes"));
        }
        let event: Self = serde_json::from_slice(&bytes)
            .map_err(|e| format!("invalid intake event JSON: {e}"))?;
        if !valid_id(&event.source, 64) || !valid_id(&event.delivery_id, 96) {
            return Err("event source and deliveryId must use safe identifier characters".into());
        }
        if event.event.trim().is_empty()
            || event.reference.trim().is_empty()
            || event.reference.len() > 256
        {
            return Err("event type and bounded reference are required".into());
        }
        Ok(event)
    }

    fn prompt(&self) -> String {
        let requested = self
            .prompt
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or("Investigate and address the assigned event within work-contract bounds.");
        format!(
            "Assigned event: source={:?} event={:?} reference={:?} delivery={:?}\n\nTask: {}",
            redact_text(&self.source).0,
            redact_text(&self.event).0,
            redact_text(&self.reference).0,
            redact_text(&self.delivery_id).0,
            redact_text(requested).0,
        )
    }
}

pub fn run(cfg: &Config) -> JResult<()> {
    let dry_run = match cfg.sub.as_deref() {
        None => false,
        Some("dry-run") => true,
        Some(_) => return Err("dispatch accepts only the 'dry-run' subcommand".into()),
    };
    if !cfg.positional.is_empty() {
        return Err(
            "unexpected dispatch arguments; place 'dry-run' immediately after 'dispatch'".into(),
        );
    }

    let event_path = cfg
        .event_path
        .as_ref()
        .ok_or("dispatch requires --event FILE")?;
    if cfg.contract_path.is_none() {
        return Err("dispatch requires --contract FILE".into());
    }
    let event = IntakeEvent::load(event_path)?;
    let authority = SessionAuthority::load(cfg)?;
    let contract = authority
        .contract
        .as_ref()
        .ok_or("dispatch requires --contract FILE")?;
    let task_id = contract.metadata.task_id.clone();
    if !valid_id(&task_id, 96) {
        return Err("dispatch contract taskId must be a bounded identifier".into());
    }
    let identity = &authority.identity;
    let tenant = identity.tenant.clone();
    let principal = identity.principal.clone();
    let delivery_key = format!(
        "intake:v2:{}",
        digest(&(&event.source, &event.delivery_id))?
    );
    let prompt = event.prompt();
    if dry_run {
        return crate::print_json(&json!({
            "ok": true, "dryRun": true, "queued": false, "taskId": task_id,
            "idempotencyKey": delivery_key, "promptPreview": prompt,
        }));
    }

    let workspace = cfg.root.display().to_string();
    let request_digest = digest(&(&event, contract, &workspace))?;
    let request = DispatchRequest {
        tenant: &tenant,
        task_id: &task_id,
        principal: &principal,
        purpose: &cfg.purpose,
        workspace: &workspace,
        delivery_key: &delivery_key,
        request_digest: &request_digest,
        source: &event.source,
        delivery_id: &event.delivery_id,
    };
    let catalog = Catalog::load(cfg)?;
    let mut tasks = TaskStore::open(&cfg.state_dir)?;
    match tasks
        .claim_dispatch(&request)
        .map_err(|e| format!("dispatch {delivery_key} for task {task_id}: {e}"))?
    {
        DispatchClaim::Completed { turn_status } => {
            return crate::print_json(&json!({
                "ok": true, "deduplicated": true, "taskId": task_id,
                "idempotencyKey": delivery_key, "turnStatus": turn_status,
                "cached": {"turnStatus": turn_status},
            }));
        }
        DispatchClaim::Acquired => {}
    }
    let mut agent = match Agent::new_for_dispatch(cfg, &catalog, authority, &delivery_key) {
        Ok(agent) => agent,
        Err(error) => {
            return Err(match tasks.release_unstarted_dispatch(&request) {
                Ok(()) => format!(
                    "dispatch startup failed; delivery was released: {}",
                    redact_text(&error).0
                ),
                Err(release_error) => unfinished(
                    &task_id,
                    &delivery_key,
                    &format!("{error}; claim release failed: {release_error}"),
                ),
            })
        }
    };
    let answer = agent
        .run(&prompt)
        .map_err(|e| unfinished(&task_id, &delivery_key, &e))?;
    let run_id = agent
        .last_run_id()
        .ok_or("delivery turn did not start a run")?;
    let turn_status = tasks
        .complete_dispatch(&request, run_id)
        .map_err(|e| unfinished(&task_id, &delivery_key, &e))?;
    crate::print_json(&json!({
        "ok": true, "deduplicated": false, "taskId": task_id,
        "idempotencyKey": delivery_key, "turnStatus": turn_status, "answer": answer,
    }))
}

fn digest(value: &impl Serialize) -> JResult<String> {
    let bytes = serde_json::to_vec(value).map_err(|e| e.to_string())?;
    Ok(sha256_hex(&bytes))
}

fn unfinished(task_id: &str, delivery_key: &str, error: &str) -> String {
    format!(
        "delivery {delivery_key} for task {task_id} requires reconciliation: {}",
        redact_text(error).0
    )
}
