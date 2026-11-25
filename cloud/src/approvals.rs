//! Approver access: who may decide other people's held actions, and how a
//! decision is recorded.
//!
//! accounts has no roles or groups, so "approver" is an operator decision: a list
//! of ifinsta subjects (`sub:...`) and verified email addresses (`email:...`).
//! The decision itself is made by Jani's own approval store, which enforces the
//! role, expiry and the rule that a requester can never approve their own
//! action. The gateway adds the identity check in front, writes the store as the
//! requester's own Unix user (never as root, never through a path the requester
//! controls), and appends a hash-chained record of who decided what.

use crate::auth::Person;
use crate::workspace::{principal_for, user_key};
use anyhow::{bail, Context, Result};
use jani_core::{
    approvals::ApprovalStore,
    data::redact_text,
    journal::{self, EventInput, Journal},
    types::{
        now_unix, verification_receipt_path, ApprovalRequest, ApprovalStatus, Classification,
        VerificationReceipt,
    },
};
use jani_store::tasks::TaskStore;
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Mutex,
    time::SystemTime,
};

#[derive(Clone, Debug, PartialEq, Eq)]
enum Entry {
    Sub(String),
    Email(String),
}

fn parse_list(text: &str) -> Vec<Entry> {
    text.lines()
        .map(|l| l.split('#').next().unwrap_or(""))
        .flat_map(|l| l.split([',', ' ', '\t']))
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .filter_map(|t| {
            if let Some(sub) = t.strip_prefix("sub:") {
                Some(Entry::Sub(sub.to_string()))
            } else {
                t.strip_prefix("email:")
                    .map(|e| Entry::Email(e.to_ascii_lowercase()))
            }
        })
        .collect()
}

pub struct Approvers {
    fixed: Vec<Entry>,
    file: Option<PathBuf>,
    cache: Mutex<(Option<SystemTime>, Vec<Entry>)>,
}

impl Approvers {
    pub fn from_env() -> Self {
        let fixed = std::env::var("JANI_CLOUD_APPROVERS")
            .map(|v| parse_list(&v))
            .unwrap_or_default();
        let file = std::env::var("JANI_CLOUD_APPROVERS_FILE")
            .ok()
            .map(PathBuf::from);
        Self {
            fixed,
            file,
            cache: Mutex::new((None, Vec::new())),
        }
    }

    #[cfg(test)]
    pub fn fixed(list: &str) -> Self {
        Self {
            fixed: parse_list(list),
            file: None,
            cache: Mutex::new((None, Vec::new())),
        }
    }

    /// The approvers file is re-read when it changes, so adding an approver needs no restart.
    fn file_entries(&self) -> Vec<Entry> {
        let Some(path) = &self.file else {
            return Vec::new();
        };
        let mtime = std::fs::metadata(path).and_then(|m| m.modified()).ok();
        let Ok(mut cache) = self.cache.lock() else {
            return Vec::new();
        };
        if cache.0 != mtime || mtime.is_none() {
            cache.1 = std::fs::read_to_string(path)
                .map(|t| parse_list(&t))
                .unwrap_or_default();
            cache.0 = mtime;
        }
        cache.1.clone()
    }

    pub fn is_approver(&self, person: &Person) -> bool {
        let matches = |entries: &[Entry]| {
            entries.iter().any(|e| match e {
                Entry::Sub(s) => *s == person.sub,
                // An unverified address proves nothing about who owns it.
                Entry::Email(addr) => {
                    person.email_verified
                        && person
                            .email
                            .as_deref()
                            .is_some_and(|m| m.eq_ignore_ascii_case(addr))
                }
            })
        };
        matches(&self.fixed) || matches(&self.file_entries())
    }

    pub fn configured(&self) -> bool {
        !self.fixed.is_empty() || self.file.is_some()
    }
}

/// Who a workspace belongs to, for an approver's eyes only.
pub struct Profiles {
    dir: PathBuf,
    seen: Mutex<HashMap<String, String>>,
}

impl Profiles {
    pub fn new(data_dir: &Path) -> Self {
        let dir = data_dir.join("profiles");
        let _ = std::fs::create_dir_all(&dir);
        Self {
            dir,
            seen: Mutex::new(HashMap::new()),
        }
    }

    /// Remember a person's email and name next to their workspace key, writing only on change.
    pub fn record(&self, key: &str, person: &Person) {
        let value = json!({"email": person.email, "name": person.name, "emailVerified": person.email_verified}).to_string();
        let Ok(mut seen) = self.seen.lock() else {
            return;
        };
        if seen.get(key) == Some(&value) {
            return;
        }
        let path = self.dir.join(format!("{key}.json"));
        let temp = self.dir.join(format!("{key}.tmp-{}", std::process::id()));
        if std::fs::write(&temp, &value)
            .and_then(|_| std::fs::rename(&temp, &path))
            .is_ok()
        {
            seen.insert(key.to_string(), value);
        }
    }

    fn email_of(&self, key: &str) -> Option<String> {
        let text = std::fs::read_to_string(self.dir.join(format!("{key}.json"))).ok()?;
        serde_json::from_str::<Value>(&text).ok()?["email"]
            .as_str()
            .map(str::to_string)
    }
}

fn is_key(s: &str) -> bool {
    s.len() == 24 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// A regular, modest-size file that is not a symlink: safe to read as root
/// even though a different user owns the directory it sits in.
fn plain_file(path: &Path) -> bool {
    std::fs::symlink_metadata(path)
        .map(|m| m.is_file() && m.len() < 1024 * 1024)
        .unwrap_or(false)
}

/// Every unexpired pending request across all workspaces.
pub fn list_pending(data_dir: &Path, profiles: &Profiles, me: &str) -> Vec<Value> {
    let mut out = Vec::new();
    let Ok(users) = std::fs::read_dir(data_dir.join("users")) else {
        return out;
    };
    let now = now_unix();
    for user in users.flatten() {
        let key = user.file_name().to_string_lossy().into_owned();
        if !is_key(&key) {
            continue;
        }
        let state_dir = user.path().join("state");
        let file = state_dir.join("approvals.json");
        if !plain_file(&file) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&file) else {
            continue;
        };
        let Ok(list) = serde_json::from_str::<Vec<ApprovalRequest>>(&text) else {
            continue;
        };
        let task_store = TaskStore::open(&state_dir).ok();
        for r in list {
            if r.status != ApprovalStatus::Pending || r.expires_at <= now {
                continue;
            }
            let (resource_preview, redacted_count) = redact_text(&r.resource);
            let task_ctx = task_store
                .as_ref()
                .and_then(|s| s.load(&r.tenant_id, &r.task_id).ok().flatten())
                .map(|cp| {
                    json!({
                        "purpose": cp.purpose,
                        "status": cp.status,
                        "epoch": cp.epoch,
                    })
                });
            let verification = verification_receipt_path(&state_dir, &r.task_id)
                .ok()
                .filter(|p| plain_file(p))
                .and_then(|p| VerificationReceipt::load(&p).ok())
                .filter(|vr| vr.task_id == r.task_id && vr.tenant_id == r.tenant_id)
                .map(|vr| {
                    json!({
                        "present": true,
                        "passed": vr.passed,
                        "signed": vr.signature.is_some(),
                        "verificationId": vr.verification_id,
                    })
                });
            out.push(json!({
                "user": key,
                "own": key == me,
                "requester": profiles.email_of(&key).unwrap_or_else(|| principal_for(&key)),
                "approvalId": r.approval_id,
                "capability": r.capability,
                "resource": r.resource,
                "resourcePreview": resource_preview,
                "redactedTokens": redacted_count,
                "risk": r.risk,
                "approvalClass": r.approval_class,
                "taskId": r.task_id,
                "taskContext": task_ctx,
                "verification": verification,
                "payloadDigest": r.payload_digest,
                "createdAt": r.created_at,
                "expiresAt": r.expires_at,
            }));
        }
    }
    out.sort_by_key(|v| std::cmp::Reverse(v["createdAt"].as_u64().unwrap_or(0)));
    out
}

/// Record the decision in the gateway's own hash-chained journal.
fn audit(data_dir: &Path, decided: &ApprovalRequest, approver: &str, approve: bool) -> Result<()> {
    let dir = data_dir.join("audit");
    let mut journal = Journal::open(&dir).map_err(|e| anyhow::anyhow!(e))?;
    journal
        .record(EventInput {
            tenant_id: &decided.tenant_id,
            task_id: &decided.task_id,
            run_id: "cloud-approvals",
            invocation_id: &decided.approval_id,
            actor_id: approver,
            worker_id: "jani-cloud",
            action: "approval.decide",
            resource_digest: &decided.payload_digest,
            decision: if approve {
                journal::decision::PERMIT
            } else {
                journal::decision::DENY
            },
            reason_code: "approver_decision",
            policy_revision: "jani-cloud-v1",
            approval_id: Some(&decided.approval_id),
            sandbox_receipt_id: None,
            data_classification: Classification::Internal,
        })
        .map_err(|e| anyhow::anyhow!(e))?;
    Ok(())
}

/// Decide one request. The store is written by a helper running as the
/// requester's own Unix user (when per-person users are on), so nothing here
/// writes into a user-controlled directory with the gateway's privileges.
pub async fn decide(
    data_dir: &Path,
    self_bin: &Path,
    uid_base_enabled: bool,
    tenant: &str,
    approver: &Person,
    user: &str,
    id: &str,
    approve: bool,
) -> Result<Value> {
    if approver.degraded {
        bail!("approval decisions require a live identity provider session, not outage grace mode");
    }
    if !is_key(user) {
        bail!("unknown workspace");
    }
    if id.is_empty()
        || id.len() > 128
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        bail!("malformed approval id");
    }
    let state = data_dir.join("users").join(user).join("state");
    let meta = std::fs::symlink_metadata(&state).context("unknown workspace")?;
    if !meta.is_dir() {
        bail!("unknown workspace");
    }
    let principal = principal_for(&user_key(&approver.sub));
    let uid: Option<u32> = if uid_base_enabled {
        let uid_text = std::fs::read_to_string(data_dir.join("uids").join(user))
            .context("missing per-user UID allocation; refusing to fall back to privileged UID")?;
        let parsed = uid_text
            .trim()
            .parse()
            .context("corrupt per-user UID allocation; refusing to fall back to privileged UID")?;
        Some(parsed)
    } else {
        None
    };
    let mut command = tokio::process::Command::new(self_bin);
    command
        .arg("decide-approval")
        .args([
            "--state",
            &state.display().to_string(),
            "--id",
            id,
            "--principal",
            &principal,
            "--tenant",
            tenant,
        ])
        .arg(if approve { "--approve" } else { "--reject" })
        .env_clear()
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    #[cfg(unix)]
    {
        let pre = crate::cgroup::PreExec::new(
            &crate::cgroup::Joined::default(),
            &crate::cgroup::Limits::default(),
            uid,
        );
        unsafe {
            command.pre_exec(move || pre.apply());
        }
    }
    let output = tokio::time::timeout(std::time::Duration::from_secs(20), command.output())
        .await
        .context("the approval store did not answer")?
        .context("cannot run the approval helper")?;
    if !output.status.success() {
        let why = String::from_utf8_lossy(&output.stderr).trim().to_string();
        bail!(
            "{}",
            if why.is_empty() {
                "the approval store refused the decision"
            } else {
                &why
            }
        );
    }
    let decided: ApprovalRequest =
        serde_json::from_slice(&output.stdout).context("unreadable helper output")?;
    if let Err(e) = audit(data_dir, &decided, &principal, approve) {
        tracing::error!(error = %e, "approval decision made but not journaled");
    }
    tracing::info!(approver = %principal, requester = %user, approval = %decided.approval_id, approve, "approval decided");
    Ok(json!({"approvalId": decided.approval_id, "status": decided.status, "approver": principal}))
}

/// `jani-cloud decide-approval ...`: runs as the requester's own Unix user.
pub fn helper(args: &[String]) -> Result<()> {
    let (mut state, mut id, mut principal, mut tenant, mut approve) =
        (None, None, None, None, None);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--state" => state = it.next().cloned(),
            "--id" => id = it.next().cloned(),
            "--principal" => principal = it.next().cloned(),
            "--tenant" => tenant = it.next().cloned(),
            "--approve" => approve = Some(true),
            "--reject" => approve = Some(false),
            other => bail!("unexpected argument {other:?}"),
        }
    }
    let (state, id, principal, tenant, approve) = (
        state.context("--state")?,
        id.context("--id")?,
        principal.context("--principal")?,
        tenant.context("--tenant")?,
        approve.context("--approve or --reject")?,
    );
    let mut store = ApprovalStore::open(Path::new(&state)).map_err(|e| anyhow::anyhow!(e))?;
    // The role is fixed: reaching this helper at all means the gateway verified an approver.
    let decided = store
        .decide(&id, &tenant, &principal, "human", "approver", approve)
        .map_err(|e| anyhow::anyhow!(e))?;
    println!("{}", serde_json::to_string(&decided)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn person(sub: &str, email: Option<&str>, verified: bool) -> Person {
        Person {
            sub: sub.into(),
            email: email.map(str::to_string),
            email_verified: verified,
            name: None,
        }
    }

    #[test]
    fn lists_parse_subs_and_emails_and_ignore_comments() {
        let e = parse_list(
            "sub:abc, email:Bob@Example.test\n# a comment\nemail:c@d.test  # trailing\nnonsense",
        );
        assert_eq!(
            e,
            vec![
                Entry::Sub("abc".into()),
                Entry::Email("bob@example.test".into()),
                Entry::Email("c@d.test".into())
            ]
        );
    }

    #[test]
    fn an_unverified_email_never_grants_approval() {
        let a = Approvers::fixed("email:bob@example.test");
        assert!(
            a.is_approver(&person("s1", Some("Bob@example.test"), true)),
            "verified, case-insensitive"
        );
        assert!(
            !a.is_approver(&person("s1", Some("bob@example.test"), false)),
            "unverified"
        );
        assert!(!a.is_approver(&person("s1", Some("other@example.test"), true)));
        assert!(!a.is_approver(&person("s1", None, true)));
    }

    #[test]
    fn a_subject_entry_matches_exactly_regardless_of_email() {
        let a = Approvers::fixed("sub:sub_123");
        assert!(a.is_approver(&person("sub_123", None, false)));
        assert!(!a.is_approver(&person("sub_1234", None, true)));
        assert!(!Approvers::fixed("").is_approver(&person("sub_123", Some("x@y.z"), true)));
    }

    #[test]
    fn the_approvers_file_is_reread_when_it_changes() {
        let dir = std::env::temp_dir().join(format!("jani-cloud-appr-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("approvers");
        std::fs::write(&file, "sub:one\n").unwrap();
        let a = Approvers {
            fixed: vec![],
            file: Some(file.clone()),
            cache: Mutex::new((None, vec![])),
        };
        assert!(a.is_approver(&person("one", None, false)));
        assert!(!a.is_approver(&person("two", None, false)));
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&file, "sub:one\nsub:two\n").unwrap();
        // mtime granularity can be coarse; force a different timestamp.
        let later = SystemTime::now() + std::time::Duration::from_secs(5);
        let f = std::fs::OpenOptions::new().write(true).open(&file).unwrap();
        f.set_modified(later).unwrap();
        assert!(
            a.is_approver(&person("two", None, false)),
            "added without a restart"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn keys_and_ids_are_validated_before_touching_the_filesystem() {
        assert!(is_key(&user_key("x")));
        assert!(!is_key("../../etc"));
        assert!(!is_key(&"g".repeat(24)));
    }

    #[test]
    fn list_pending_enriches_redacted_preview_and_verification_summary() {
        let dir = std::env::temp_dir().join(format!("jani-cloud-pending-{}", std::process::id()));
        let key = user_key("alice");
        let state = dir.join("users").join(&key).join("state");
        std::fs::create_dir_all(&state).unwrap();

        let mut store = ApprovalStore::open(&state).unwrap();
        let req = store
            .create(jani_core::approvals::NewApproval {
                tenant_id: "acme",
                task_id: "task-enrich-1",
                requester: "bot-1",
                capability: "connector.github.comment",
                resource: "https://api.github.com/repos/org/repo?token=ghp_1234567890abcdef",
                payload_digest: "deadbeef",
                risk: jani_core::types::RiskTier::R2,
                approval_class: "external-write",
                required_role: "approver",
                ttl_seconds: 600,
            })
            .unwrap();

        TaskStore::open(&state)
            .unwrap()
            .checkpoint(
                "acme",
                "task-enrich-1",
                1,
                "waiting_approval",
                None,
                Some(&req.approval_id),
                "bot-1",
                "pr-review",
                "/work/repo",
                &[],
            )
            .unwrap();

        let vr = VerificationReceipt {
            api_version: "jani.dev/v1alpha1".into(),
            kind: "VerificationReceipt".into(),
            verification_id: "ver-99".into(),
            tenant_id: "acme".into(),
            task_id: "task-enrich-1".into(),
            contract_digest: "cfg".into(),
            verified_at: now_unix(),
            passed: true,
            verifier_id: "verifier".into(),
            checks: vec![],
            changed_files: vec![],
            out_of_scope_files: vec![],
            signature: None,
        };
        let vr_path = verification_receipt_path(&state, "task-enrich-1").unwrap();
        std::fs::create_dir_all(vr_path.parent().unwrap()).unwrap();
        std::fs::write(&vr_path, serde_json::to_vec(&vr).unwrap()).unwrap();

        let profiles = Profiles::new(&dir);
        let pending = list_pending(&dir, &profiles, "other-user");
        assert_eq!(pending.len(), 1);
        assert!(pending[0]["resourcePreview"]
            .as_str()
            .unwrap()
            .contains("[REDACTED]"));
        assert_eq!(pending[0]["taskContext"]["purpose"], "pr-review");
        assert_eq!(pending[0]["verification"]["passed"], true);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
