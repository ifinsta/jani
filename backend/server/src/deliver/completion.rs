use super::{git_ok, queued_commit, worktree, write_report, Delivery, Outcome, Spec};
use crate::config::{Config, JResult};
use crate::identity::{self, VerifiedIdentity};
use crate::journal::{self, decision, EventInput, Journal};
use crate::policy::{contract_matches_identity, PolicyEngine};
use crate::tools::action;
use crate::types::{new_id, sha256_hex, Classification, WorkContract};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::fs::{self, File, OpenOptions};
use std::io::{ErrorKind, Read, Write};
use std::path::{Path, PathBuf};

const SCHEMA: &str = "jani.delivery-finalization/v1";
const MAX_BYTES: u64 = 2 * 1024 * 1024;
const PREPARED: &str = "delivery.finalization";

#[derive(Deserialize, Serialize)]
struct Record {
    schema: String,
    generation: u64,
    binding: String,
    base: Option<String>,
    observed_head: Option<String>,
    commit: Option<queued_commit::Commit>,
    delivery: Delivery,
}

pub(super) struct Saved {
    record: Record,
    digest: String,
}

impl Saved {
    pub fn linked(&self) -> bool {
        self.record.delivery.branch.is_some()
    }
}

pub(super) fn path(dir: &Path, generation: u64) -> PathBuf {
    dir.join(format!("completion-{generation}.json"))
}

fn invocation(generation: u64) -> String {
    format!("work-{generation}")
}

pub(super) fn intent_exists(state: &Path, id: &str, generation: u64) -> JResult<bool> {
    let path = state.join(journal::JOURNAL_FILE);
    if !path
        .try_exists()
        .map_err(|error| format!("cannot inspect the audit journal: {error}"))?
    {
        return Ok(false);
    }
    let invocation = invocation(generation);
    Ok(journal::read_verified_run(&path, id)?.iter().any(|event| {
        event.action == PREPARED && event.run_id == id && event.invocation_id == invocation
    }))
}

pub(super) fn atomic_write(path: &Path, bytes: &[u8]) -> JResult<()> {
    let dir = path.parent().ok_or("delivery file has no parent")?;
    let temporary = dir.join(new_id("delivery-write"));
    let result = (|| -> std::io::Result<()> {
        let mut options = OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary, path)?;
        #[cfg(unix)]
        File::open(dir)?.sync_all()?;
        Ok(())
    })();
    if let Err(error) = result {
        let _ = fs::remove_file(temporary);
        return Err(format!("cannot publish {}: {error}", path.display()));
    }
    Ok(())
}

pub(super) fn load(
    cfg: &Config,
    dir: &Path,
    id: &str,
    generation: u64,
    resume: bool,
) -> JResult<Option<Saved>> {
    let path = path(dir, generation);
    let file = match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.is_file() => {
            File::open(&path).map_err(|error| format!("cannot open {}: {error}", path.display()))?
        }
        Ok(_) => {
            return Err(format!(
                "invalid delivery finalization file at {}",
                path.display()
            ))
        }
        Err(error) if error.kind() == ErrorKind::NotFound => {
            if resume && intent_exists(&cfg.state_dir, id, generation)? {
                return Err("the saved delivery finalization file is missing; reconcile the delivery before retrying".into());
            }
            return Ok(None);
        }
        Err(error) => return Err(format!("cannot inspect {}: {error}", path.display())),
    };
    let mut bytes = Vec::new();
    file.take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err("saved delivery finalization exceeds its size limit".into());
    }
    let record: Record = serde_json::from_slice(&bytes)
        .map_err(|error| format!("invalid saved delivery finalization: {error}"))?;
    if record.schema != SCHEMA || record.generation != generation || record.delivery.id != id {
        return Err("saved delivery finalization belongs to a different delivery".into());
    }
    Ok(Some(Saved {
        record,
        digest: sha256_hex(&bytes),
    }))
}

pub(super) fn worker_binding(cfg: &Config, spec: &Spec) -> JResult<(VerifiedIdentity, String)> {
    let worker = identity::load(cfg)?;
    if let Some(kill) = jani_core::kill::is_killed(&cfg.state_dir, &worker.tenant)? {
        return Err(format!(
            "KILL_SWITCH_ACTIVE: halted by {} ({})",
            kill.actor, kill.reason
        ));
    }
    let policy = PolicyEngine::load(cfg)?;
    let contract = cfg
        .contract_path
        .as_deref()
        .map(WorkContract::load)
        .transpose()?;
    if let Some(contract) = contract.as_ref() {
        contract_matches_identity(contract, &worker)?;
    }
    let permissions = serde_json::to_vec(&json!({
        "root": cfg.root,
        "tenant": worker.tenant,
        "principal": worker.principal,
        "issuer": worker.issuer,
        "verified": worker.verified,
        "subjectType": worker.subject_type,
        "roles": worker.roles,
        "groups": worker.groups,
        "approvalScopes": worker.approval_scopes,
        "purpose": cfg.purpose,
        "mode": format!("{:?}", cfg.mode),
        "write": cfg.allow_write,
        "writePaths": cfg.write_paths,
        "allowExec": cfg.allow_exec,
        "execMode": format!("{:?}", cfg.exec_mode),
        "execPrograms": cfg.exec_programs,
        "sandbox": format!("{:?}/{:?}", cfg.sandbox_mode, cfg.sandbox_net),
        "repoPermissions": format!("{:?}", cfg.permissions),
        "policy": policy.effective.bundle,
        "contract": contract,
        "prompt": spec.prompt,
        "accept": spec.accept,
        "context": spec.context,
    }))
    .map_err(|error| error.to_string())?;
    Ok((worker, sha256_hex(&permissions)))
}

fn append(
    journal: &mut Journal,
    who: &VerifiedIdentity,
    saved: &Saved,
    action: &str,
    decision: &str,
    reason: &str,
) -> JResult<()> {
    journal
        .record(EventInput {
            tenant_id: &who.tenant,
            task_id: &saved.record.delivery.agent_task_id,
            run_id: &saved.record.delivery.id,
            invocation_id: &invocation(saved.record.generation),
            actor_id: &who.principal,
            worker_id: &who.principal,
            action,
            resource_digest: &saved.digest,
            decision,
            reason_code: reason,
            policy_revision: journal::RUNTIME_REVISION,
            approval_id: None,
            sandbox_receipt_id: None,
            data_classification: Classification::Internal,
        })
        .map(|_| ())
}

fn head(workspace: &worktree::Workspace, hooks: &Path) -> JResult<Option<String>> {
    workspace
        .branch
        .as_ref()
        .map(|_| {
            git_ok(
                &workspace.root,
                hooks,
                &["rev-parse", "--verify", "HEAD^{commit}"],
            )
            .map(|oid| oid.trim().to_string())
        })
        .transpose()
}

pub(super) fn seal(
    cfg: &Config,
    spec: &Spec,
    workspace: &worktree::Workspace,
    hooks: &Path,
    mut delivery: Delivery,
    commit: Option<queued_commit::Commit>,
    expected_binding: &str,
) -> JResult<Saved> {
    let generation = spec.queue_generation.ok_or("missing queue generation")?;
    let (who, binding) = worker_binding(cfg, spec)?;
    if binding != expected_binding {
        return Err(
            "worker authority changed during delivery; retry under the current authority".into(),
        );
    }
    let observed_head = head(workspace, hooks)?;
    if let Some(commit) = &commit {
        if observed_head.as_deref() != Some(commit.parent.as_str()) {
            return Err("delivery branch moved while preparing the candidate commit".into());
        }
        delivery.commit = Some(commit.oid.clone());
        delivery.changed_files.clone_from(&commit.paths);
        delivery.test_files_changed = delivery
            .changed_files
            .iter()
            .filter(|path| super::looks_like_test(path))
            .cloned()
            .collect();
    }
    let record = Record {
        schema: SCHEMA.into(),
        generation,
        binding,
        observed_head,
        base: workspace.base.clone(),
        commit,
        delivery,
    };
    let bytes = serde_json::to_vec(&record).map_err(|error| error.to_string())?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err("delivery finalization exceeds its size limit".into());
    }
    atomic_write(&path(&record.delivery.report_dir, generation), &bytes)?;
    let saved = Saved {
        record,
        digest: sha256_hex(&bytes),
    };
    let mut journal = Journal::open_with_anchor(&cfg.state_dir, cfg.audit_anchor.as_deref())?;
    append(
        &mut journal,
        &who,
        &saved,
        PREPARED,
        decision::STARTED,
        "delivery_prepared",
    )?;
    Ok(saved)
}

pub(super) fn finish(
    cfg: &Config,
    spec: &Spec,
    workspace: &worktree::Workspace,
    dir: &Path,
    hooks: &Path,
    saved: Saved,
    check_claim: &dyn Fn() -> JResult<()>,
) -> JResult<Delivery> {
    check_claim()?;
    let (who, binding) = worker_binding(cfg, spec)?;
    let record = &saved.record;
    let delivery = &record.delivery;
    if binding != record.binding
        || workspace.root != delivery.workdir
        || workspace.branch != delivery.branch
        || workspace.base != record.base
        || dir != delivery.report_dir
    {
        return Err(
            "worker authority or workspace changed; reconcile the saved delivery before retrying"
                .into(),
        );
    }
    let events =
        journal::read_verified_run(&cfg.state_dir.join(journal::JOURNAL_FILE), &delivery.id)?;
    let invocation = invocation(record.generation);
    let mut prepared = false;
    let mut terminal = false;
    let expected = if delivery.outcome == Outcome::Failed {
        decision::FAILED
    } else {
        decision::COMPLETED
    };
    for event in events.iter().filter(|event| {
        event.run_id == delivery.id
            && event.invocation_id == invocation
            && matches!(event.action.as_str(), PREPARED | action::DELIVERY_RUN)
    }) {
        if event.tenant_id != who.tenant
            || event.actor_id != who.principal
            || event.task_id != delivery.agent_task_id
            || event.resource_digest != saved.digest
        {
            return Err("audit evidence conflicts with the saved delivery finalization".into());
        }
        match event.action.as_str() {
            PREPARED
                if !prepared
                    && event.decision == decision::STARTED
                    && event.reason_code == "delivery_prepared" =>
            {
                prepared = true
            }
            action::DELIVERY_RUN
                if prepared
                    && !terminal
                    && event.decision == expected
                    && event.reason_code == delivery.outcome.reason() =>
            {
                terminal = true
            }
            _ => {
                return Err(
                    "delivery finalization audit events are out of order or duplicated".into(),
                )
            }
        }
    }
    if !prepared {
        return Err(
            "saved delivery finalization lacks a verified intent; reconcile it before retrying"
                .into(),
        );
    }
    if let Some(commit) = &record.commit {
        if terminal && head(workspace, hooks)?.as_deref() != Some(commit.oid.as_str()) {
            return Err("the completed delivery branch moved; reconcile it before retrying".into());
        }
        commit.apply(workspace, hooks, check_claim)?;
        if head(workspace, hooks)?.as_deref() != Some(commit.oid.as_str()) {
            return Err(
                "delivery branch moved during finalization; reconcile it before retrying".into(),
            );
        }
    } else if head(workspace, hooks)? != record.observed_head {
        return Err("the delivery branch no longer matches the verified snapshot".into());
    }
    check_claim()?;
    if !terminal {
        let mut journal = Journal::open_with_anchor(&cfg.state_dir, cfg.audit_anchor.as_deref())?;
        append(
            &mut journal,
            &who,
            &saved,
            action::DELIVERY_RUN,
            expected,
            delivery.outcome.reason(),
        )?;
    }
    check_claim()?;
    write_report(delivery, spec, record.base.as_deref(), true, hooks)?;
    Ok(saved.record.delivery)
}
