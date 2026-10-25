use crate::types::{new_id, now_unix, sha256_hex, ApprovalRequest, ApprovalStatus, RiskTier};
use serde_json::json;
use std::fs;
use std::path::{Path, PathBuf};

const APPROVALS_FILE: &str = "approvals.json";

pub struct ApprovalStore {
    path: PathBuf,
    requests: Vec<ApprovalRequest>,
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
        let requests = if path.is_file() {
            let text = fs::read_to_string(&path).map_err(|e| e.to_string())?;
            serde_json::from_str(&text).map_err(|e| format!("invalid approvals store: {e}"))?
        } else {
            Vec::new()
        };
        Ok(Self { path, requests })
    }

    fn save(&self) -> Result<(), String> {
        let temp = self
            .path
            .with_extension(format!("tmp-{}", crate::process::unique_id()));
        let text = serde_json::to_string_pretty(&self.requests).map_err(|e| e.to_string())?;
        fs::write(&temp, text).map_err(|e| e.to_string())?;
        fs::rename(&temp, &self.path).map_err(|e| e.to_string())?;
        Ok(())
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
        self.requests.push(request.clone());
        self.save()?;
        Ok(request)
    }

    pub fn decide(
        &mut self,
        approval_id: &str,
        approver: &str,
        approver_role: &str,
        approve: bool,
    ) -> Result<ApprovalRequest, String> {
        let index = self
            .requests
            .iter()
            .position(|r| r.approval_id == approval_id)
            .ok_or_else(|| format!("unknown approval '{approval_id}'"))?;
        let request = &mut self.requests[index];
        if request.status == ApprovalStatus::Pending && request.expires_at <= now_unix() {
            request.status = ApprovalStatus::Expired;
            let expired = request.clone();
            self.save()?;
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
            return Err("separation of duties: requester cannot approve their own action".into());
        }
        if !request.required_role.is_empty() && approver_role != request.required_role {
            return Err(format!(
                "approval requires role '{}'",
                request.required_role
            ));
        }
        if approve {
            request.status = ApprovalStatus::Approved;
            request.approver = Some(approver.into());
            request.decided_at = Some(now_unix());
        } else {
            request.status = ApprovalStatus::Rejected;
            request.approver = Some(approver.into());
            request.decided_at = Some(now_unix());
        }
        let decided = request.clone();
        self.save()?;
        Ok(decided)
    }

    /// Consume an approval. The binding must match exactly; a changed payload,
    /// resource or capability invalidates the approval.
    pub fn consume(
        &mut self,
        approval_id: &str,
        capability: &str,
        resource: &str,
        payload_digest: &str,
    ) -> Result<ApprovalRequest, String> {
        let request = self
            .requests
            .iter_mut()
            .find(|r| r.approval_id == approval_id)
            .ok_or_else(|| format!("unknown approval '{approval_id}'"))?;
        match request.status {
            ApprovalStatus::Approved => {}
            ApprovalStatus::Expired => return Err("approval has expired".into()),
            ApprovalStatus::Consumed => return Err("approval has already been consumed".into()),
            _ => return Err("approval was not granted".into()),
        }
        if request.expires_at <= now_unix() {
            request.status = ApprovalStatus::Expired;
            self.save()?;
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
        self.save()?;
        Ok(consumed)
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
            .decide(&request.approval_id, "human-requester", "approver", true)
            .is_err());
        assert!(store
            .decide(&request.approval_id, "human-2", "engineer", true)
            .is_err());
        let decided = store
            .decide(&request.approval_id, "human-2", "approver", true)
            .unwrap();
        assert_eq!(decided.status, ApprovalStatus::Approved);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn approval_binds_exact_payload_and_is_single_use() {
        let dir = temp_dir();
        let mut store = ApprovalStore::open(&dir).unwrap();
        let request = create(&mut store);
        store
            .decide(&request.approval_id, "human-2", "approver", true)
            .unwrap();
        assert!(store
            .consume(
                &request.approval_id,
                "plugin.write",
                "issue:43",
                "payload-hash"
            )
            .is_err());
        assert!(store
            .consume(
                &request.approval_id,
                "plugin.write",
                "issue:42",
                "changed-payload"
            )
            .is_err());
        store
            .consume(
                &request.approval_id,
                "plugin.write",
                "issue:42",
                "payload-hash",
            )
            .unwrap();
        assert!(store
            .consume(
                &request.approval_id,
                "plugin.write",
                "issue:42",
                "payload-hash"
            )
            .is_err());
        let _ = fs::remove_dir_all(dir);
    }
}
