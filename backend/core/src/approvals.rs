use crate::types::{new_id, now_unix, sha256_hex, ApprovalRequest, ApprovalStatus, RiskTier};
use serde_json::json;
use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};

const APPROVALS_FILE: &str = "approvals.json";

pub struct ApprovalStore {
    path: PathBuf,
    lock: File,
    requests: Vec<ApprovalRequest>,
}

fn read_requests(path: &Path) -> Result<Vec<ApprovalRequest>, String> {
    match fs::read_to_string(path) {
        Ok(text) => {
            serde_json::from_str(&text).map_err(|e| format!("invalid approvals store: {e}"))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e.to_string()),
    }
}

pub struct NewApproval<'a> {
    pub tenant_id: &'a str,
    pub task_id: &'a str,
    pub requester: &'a str,
    pub capability: &'a str,
    pub resource: &'a str,
    pub payload_digest: &'a str,
    pub risk: RiskTier,
    pub approval_class: &'a str,
    pub required_role: &'a str,
    pub ttl_seconds: u64,
}

impl ApprovalStore {
    pub fn open(state_dir: &Path) -> Result<Self, String> {
        fs::create_dir_all(state_dir)
            .map_err(|e| format!("cannot create state directory {}: {e}", state_dir.display()))?;
        let path = state_dir.join(APPROVALS_FILE);
        // The lock has a stable name because the data file is replaced on save.
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path.with_extension("lock"))
            .map_err(|e| format!("cannot open approvals lock: {e}"))?;
        lock.lock_shared()
            .map_err(|e| format!("cannot lock approvals store: {e}"))?;
        let requests = read_requests(&path);
        let _ = lock.unlock();
        let requests = requests?;
        Ok(Self {
            path,
            lock,
            requests,
        })
    }

    fn update<T>(
        &mut self,
        change: impl FnOnce(&mut Self) -> Result<T, String>,
    ) -> Result<T, String> {
        self.lock
            .lock()
            .map_err(|e| format!("cannot lock approvals store: {e}"))?;
        let result = (|| {
            self.requests = read_requests(&self.path)?;
            change(self)
        })();
        let _ = self.lock.unlock();
        result
    }

    fn save(&self) -> Result<(), String> {
        let temp = self
            .path
            .with_extension(format!("tmp-{}", crate::process::unique_id()));
        let text = serde_json::to_string_pretty(&self.requests).map_err(|e| e.to_string())?;
        let saved = (|| {
            fs::write(&temp, text).map_err(|e| e.to_string())?;
            OpenOptions::new()
                .read(true)
                .write(true)
                .open(&temp)
                .and_then(|file| file.sync_all())
                .map_err(|e| e.to_string())?;
            fs::rename(&temp, &self.path).map_err(|e| e.to_string())?;
            #[cfg(unix)]
            fs::File::open(self.path.parent().unwrap_or(Path::new(".")))
                .and_then(|dir| dir.sync_all())
                .map_err(|e| e.to_string())?;
            Ok(())
        })();
        if saved.is_err() {
            let _ = fs::remove_file(&temp);
        }
        saved
    }

    pub fn create(&mut self, spec: NewApproval<'_>) -> Result<ApprovalRequest, String> {
        let expires_at = now_unix().saturating_add(spec.ttl_seconds.max(1));
        let nonce_material = format!(
            "{}:{}:{}:{}:{}:{}:{}",
            spec.tenant_id,
            spec.task_id,
            spec.requester,
            spec.capability,
            spec.resource,
            spec.payload_digest,
            now_unix()
        );
        let request = ApprovalRequest {
            approval_id: new_id("approval"),
            tenant_id: spec.tenant_id.into(),
            task_id: spec.task_id.into(),
            requester: spec.requester.into(),
            capability: spec.capability.into(),
            resource: spec.resource.into(),
            payload_digest: spec.payload_digest.into(),
            risk: spec.risk,
            approval_class: spec.approval_class.into(),
            required_role: spec.required_role.into(),
            created_at: now_unix(),
            expires_at,
            nonce: sha256_hex(nonce_material.as_bytes())[..32].to_string(),
            status: ApprovalStatus::Pending,
            approver: None,
            decided_at: None,
            consumed_at: None,
        };
        self.update(|store| {
            store.requests.push(request.clone());
            store.save()?;
            Ok(request)
        })
    }

    pub fn decide(
        &mut self,
        approval_id: &str,
        approver_tenant: &str,
        approver: &str,
        subject_type: &str,
        approver_role: &str,
        approve: bool,
    ) -> Result<ApprovalRequest, String> {
        self.decide_with_scopes(
            approval_id,
            approver_tenant,
            approver,
            subject_type,
            approver_role,
            &[],
            approve,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn decide_with_scopes(
        &mut self,
        approval_id: &str,
        approver_tenant: &str,
        approver: &str,
        subject_type: &str,
        approver_role: &str,
        approval_scopes: &[String],
        approve: bool,
    ) -> Result<ApprovalRequest, String> {
        self.update(|store| {
            let index = store
                .requests
                .iter()
                .position(|r| r.approval_id == approval_id)
                .ok_or_else(|| format!("unknown approval '{approval_id}'"))?;
            let request = &mut store.requests[index];
            if request.tenant_id != approver_tenant {
                return Err("approval tenant does not match approver".into());
            }
            if subject_type != "human" {
                return Err("approval requires a verified human subject".into());
            }
            if request.status == ApprovalStatus::Pending && request.expires_at <= now_unix() {
                request.status = ApprovalStatus::Expired;
                let expired = request.clone();
                store.save()?;
                return Err(format!("approval {} has expired", expired.approval_id));
            }
            if request.status != ApprovalStatus::Pending {
                return Err(format!(
                    "approval is not pending (status {:?})",
                    request.status
                ));
            }
            if approver.trim().is_empty() {
                return Err("approver identity is required".into());
            }
            if approver == request.requester {
                return Err(
                    "separation of duties: requester cannot approve their own action".into(),
                );
            }
            if !request.required_role.is_empty() && approver_role != request.required_role {
                return Err(format!(
                    "approval requires role '{}'",
                    request.required_role
                ));
            }
            if !approval_scopes.is_empty()
                && !approval_scopes
                    .iter()
                    .any(|scope| scope == "*" || scope == &request.approval_class)
            {
                return Err(format!(
                    "approver identity lacks approvalScope for '{}'",
                    request.approval_class
                ));
            }
            if approve {
                request.status = ApprovalStatus::Approved;
            } else {
                request.status = ApprovalStatus::Rejected;
            }
            request.approver = Some(approver.into());
            request.decided_at = Some(now_unix());
            let decided = request.clone();
            store.save()?;
            Ok(decided)
        })
    }

    /// Consume an approval. The binding must match exactly; a changed payload,
    /// resource, capability, tenant or task invalidates the approval.
    pub fn consume(
        &mut self,
        approval_id: &str,
        tenant_id: &str,
        task_id: &str,
        capability: &str,
        resource: &str,
        payload_digest: &str,
    ) -> Result<ApprovalRequest, String> {
        self.update(|store| {
            let request = store
                .requests
                .iter_mut()
                .find(|r| r.approval_id == approval_id)
                .ok_or_else(|| format!("unknown approval '{approval_id}'"))?;
            if request.tenant_id != tenant_id || request.task_id != task_id {
                return Err("approval does not belong to this tenant and task".into());
            }
            match request.status {
                ApprovalStatus::Approved => {}
                ApprovalStatus::Expired => return Err("approval has expired".into()),
                ApprovalStatus::Consumed => return Err("approval has already been consumed".into()),
                _ => return Err("approval was not granted".into()),
            }
            if request.expires_at <= now_unix() {
                request.status = ApprovalStatus::Expired;
                store.save()?;
                return Err("approval has expired".into());
            }
            if request.capability != capability
                || request.resource != resource
                || request.payload_digest != payload_digest
            {
                return Err("approval does not bind this exact action and payload".into());
            }
            request.status = ApprovalStatus::Consumed;
            request.consumed_at = Some(now_unix());
            let consumed = request.clone();
            store.save()?;
            Ok(consumed)
        })
    }

    pub fn refresh(&mut self) -> Result<(), String> {
        self.lock
            .lock_shared()
            .map_err(|e| format!("cannot lock approvals store: {e}"))?;
        let loaded = read_requests(&self.path);
        let _ = self.lock.unlock();
        self.requests = loaded?;
        Ok(())
    }

    pub fn find_live(&self, approval_id: &str) -> Result<Option<ApprovalRequest>, String> {
        self.lock
            .lock_shared()
            .map_err(|e| format!("cannot lock approvals store: {e}"))?;
        let loaded = read_requests(&self.path);
        let _ = self.lock.unlock();
        Ok(loaded?
            .into_iter()
            .find(|request| request.approval_id == approval_id))
    }

    pub fn list_live(&self) -> Result<Vec<ApprovalRequest>, String> {
        self.lock
            .lock_shared()
            .map_err(|e| format!("cannot lock approvals store: {e}"))?;
        let loaded = read_requests(&self.path);
        let _ = self.lock.unlock();
        loaded
    }

    pub fn find(&self, approval_id: &str) -> Option<&ApprovalRequest> {
        self.requests
            .iter()
            .find(|request| request.approval_id == approval_id)
    }

    pub fn find_pending(
        &self,
        tenant_id: &str,
        task_id: &str,
        capability: &str,
        resource: &str,
        payload_digest: &str,
    ) -> Option<&ApprovalRequest> {
        self.requests.iter().find(|request| {
            request.status == ApprovalStatus::Pending
                && request.expires_at > now_unix()
                && request.tenant_id == tenant_id
                && request.task_id == task_id
                && request.capability == capability
                && request.resource == resource
                && request.payload_digest == payload_digest
        })
    }

    pub fn to_json(&self) -> serde_json::Value {
        json!(self.requests)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir() -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("jani-approvals-{}", crate::process::unique_id()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn create(store: &mut ApprovalStore) -> ApprovalRequest {
        store
            .create(NewApproval {
                tenant_id: "tenant-a",
                task_id: "task-1",
                requester: "human-requester",
                capability: "plugin.write",
                resource: "issue:42",
                payload_digest: "payload-hash",
                risk: RiskTier::R3,
                approval_class: "external-write",
                required_role: "approver",
                ttl_seconds: 300,
            })
            .unwrap()
    }

    #[test]
    fn separation_of_duties_and_role_are_enforced() {
        let dir = temp_dir();
        let mut store = ApprovalStore::open(&dir).unwrap();
        let request = create(&mut store);
        assert!(store
            .decide(
                &request.approval_id,
                "tenant-a",
                "human-requester",
                "human",
                "approver",
                true
            )
            .is_err());
        assert!(store
            .decide(
                &request.approval_id,
                "tenant-a",
                "human-2",
                "human",
                "engineer",
                true
            )
            .is_err());
        let decided = store
            .decide(
                &request.approval_id,
                "tenant-a",
                "human-2",
                "human",
                "approver",
                true,
            )
            .unwrap();
        assert_eq!(decided.status, ApprovalStatus::Approved);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn approvers_must_be_human_and_in_the_request_tenant() {
        let dir = temp_dir();
        let mut store = ApprovalStore::open(&dir).unwrap();
        let request = create(&mut store);
        assert!(store
            .decide(
                &request.approval_id,
                "tenant-b",
                "human-2",
                "human",
                "approver",
                true
            )
            .is_err());
        assert!(store
            .decide(
                &request.approval_id,
                "tenant-a",
                "service-2",
                "service",
                "approver",
                true
            )
            .is_err());
        assert_eq!(
            store.find(&request.approval_id).unwrap().status,
            ApprovalStatus::Pending
        );
        store
            .decide(
                &request.approval_id,
                "tenant-a",
                "human-2",
                "human",
                "approver",
                true,
            )
            .unwrap();
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn stale_stores_cannot_resurrect_consumed_approvals() {
        let dir = temp_dir();
        let mut stale = ApprovalStore::open(&dir).unwrap();
        let mut writer = ApprovalStore::open(&dir).unwrap();
        let request = create(&mut writer);
        writer
            .decide(
                &request.approval_id,
                "tenant-a",
                "human-2",
                "human",
                "approver",
                true,
            )
            .unwrap();
        stale
            .consume(
                &request.approval_id,
                "tenant-a",
                "task-1",
                "plugin.write",
                "issue:42",
                "payload-hash",
            )
            .unwrap();
        assert!(stale
            .decide(
                &request.approval_id,
                "tenant-a",
                "human-3",
                "human",
                "approver",
                true,
            )
            .is_err());
        let reloaded = ApprovalStore::open(&dir).unwrap();
        assert_eq!(
            reloaded.find(&request.approval_id).unwrap().status,
            ApprovalStatus::Consumed
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn concurrent_writers_do_not_lose_requests() {
        const WRITERS: usize = 12;
        let dir = temp_dir();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(WRITERS));
        let threads: Vec<_> = (0..WRITERS)
            .map(|_| {
                let dir = dir.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    let mut store = ApprovalStore::open(&dir).unwrap();
                    barrier.wait();
                    create(&mut store).approval_id
                })
            })
            .collect();
        let ids: Vec<_> = threads
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .collect();
        let unique: std::collections::HashSet<_> = ids.iter().collect();
        assert_eq!(unique.len(), WRITERS);
        let store = ApprovalStore::open(&dir).unwrap();
        assert_eq!(store.requests.len(), WRITERS);
        assert!(ids.iter().all(|id| store.find(id).is_some()));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn concurrent_process_writers_do_not_lose_requests() {
        const WRITERS: usize = 4;
        const CHILD_DIR: &str = "JANI_APPROVAL_PROCESS_TEST_DIR";
        if let Ok(dir) = std::env::var(CHILD_DIR) {
            let mut store = ApprovalStore::open(Path::new(&dir)).unwrap();
            create(&mut store);
            return;
        }
        let dir = temp_dir();
        let children: Vec<_> = (0..WRITERS)
            .map(|_| {
                std::process::Command::new(std::env::current_exe().unwrap())
                    .env_clear()
                    .env(CHILD_DIR, &dir)
                    .args([
                        "--exact",
                        "approvals::tests::concurrent_process_writers_do_not_lose_requests",
                        "--nocapture",
                    ])
                    .output()
                    .unwrap()
            })
            .collect();
        for child in children {
            assert!(
                child.status.success(),
                "{}",
                String::from_utf8_lossy(&child.stderr)
            );
        }
        let store = ApprovalStore::open(&dir).unwrap();
        assert_eq!(store.requests.len(), WRITERS);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn approval_binds_exact_payload_and_is_single_use() {
        let dir = temp_dir();
        let mut store = ApprovalStore::open(&dir).unwrap();
        let request = create(&mut store);
        store
            .decide(
                &request.approval_id,
                "tenant-a",
                "human-2",
                "human",
                "approver",
                true,
            )
            .unwrap();
        assert!(store
            .consume(
                &request.approval_id,
                "tenant-b",
                "task-1",
                "plugin.write",
                "issue:42",
                "payload-hash"
            )
            .is_err());
        assert!(store
            .consume(
                &request.approval_id,
                "tenant-a",
                "task-2",
                "plugin.write",
                "issue:42",
                "payload-hash"
            )
            .is_err());
        assert!(store
            .consume(
                &request.approval_id,
                "tenant-a",
                "task-1",
                "plugin.write",
                "issue:43",
                "payload-hash"
            )
            .is_err());
        assert!(store
            .consume(
                &request.approval_id,
                "tenant-a",
                "task-1",
                "plugin.write",
                "issue:42",
                "changed-payload"
            )
            .is_err());
        store
            .consume(
                &request.approval_id,
                "tenant-a",
                "task-1",
                "plugin.write",
                "issue:42",
                "payload-hash",
            )
            .unwrap();
        assert!(store
            .consume(
                &request.approval_id,
                "tenant-a",
                "task-1",
                "plugin.write",
                "issue:42",
                "payload-hash"
            )
            .is_err());
        let _ = fs::remove_dir_all(dir);
    }
}
