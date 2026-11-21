//! Single-host dispatch state machine for a trusted manager service.
//! Callers must authenticate principals outside this library. The state directory
//! must be inaccessible to workers; possession of a database handle is privileged.
use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Manager,
    Supervisor,
    Operator,
    Reviewer,
    Auditor,
}
impl Role {
    pub fn name(self) -> &'static str {
        match self {
            Self::Manager => "manager",
            Self::Supervisor => "supervisor",
            Self::Operator => "operator",
            Self::Reviewer => "reviewer",
            Self::Auditor => "auditor",
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Offer {
    pub tenant: String,
    pub department: String,
    pub task: String,
    pub operator: String,
    pub agent: String,
    pub epoch: u64,
    pub expires: u64,
    // Exact serialized, independently derived assignment context.
    pub context: String,
}
#[derive(Clone, Debug)]
pub struct Lease {
    pub offer: Offer,
    pub state: String,
    pub maker: String,
    pub approver: Option<String>,
    pub evidence: Option<String>,
}
pub struct Review<'a> {
    pub expected: &'a str,
    pub review_digest: &'a str,
    pub accept: bool,
}
/// Reserved non-executable planning context.
pub const PLANNING_CONTEXT: &str = "PLANNING_ONLY_NOT_EXECUTABLE";

/// An authenticated RP identity, supplied only by a trusted application.
/// Tuple boundaries are provided by JSON serialization. This is a stable
/// reference, not a secret, signature, or bearer credential.
pub fn principal_ref(issuer: &str, client: &str, subject: &str) -> Result<String> {
    if ![issuer, client, subject]
        .iter()
        .all(|v| !v.is_empty() && v.len() <= 2048 && !v.chars().any(char::is_control))
    {
        return Err("invalid relying-party identity".into());
    }
    let bytes = serde_json::to_vec(&(issuer, client, subject)).map_err(|e| e.to_string())?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct MembershipSummary {
    pub tenant: String,
    pub department: String,
    pub role: Role,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TaskSummary {
    pub task: String,
    pub epoch: u64,
    pub expires: u64,
    pub state: String,
    pub agent: String,
}

impl Role {
    fn parse(value: &str) -> Result<Self> {
        match value {
            "manager" => Ok(Self::Manager),
            "supervisor" => Ok(Self::Supervisor),
            "operator" => Ok(Self::Operator),
            "reviewer" => Ok(Self::Reviewer),
            "auditor" => Ok(Self::Auditor),
            _ => Err("invalid stored role".into()),
        }
    }
}
pub struct DispatchStore {
    db: Connection,
}
type Result<T> = std::result::Result<T, String>;
fn sql(e: rusqlite::Error) -> String {
    e.to_string()
}
fn id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}
fn digest(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
impl DispatchStore {
    pub fn open(path: &Path) -> Result<Self> {
        super::safe(path)?;
        let db = Connection::open(path).map_err(sql)?;
        super::harden(&db)?;
        db.execute_batch(
            "CREATE TABLE IF NOT EXISTS member(
            tenant TEXT NOT NULL, department TEXT NOT NULL, principal TEXT NOT NULL,
            role TEXT NOT NULL, enabled INTEGER NOT NULL DEFAULT 1,
            PRIMARY KEY(tenant,department,principal));
        CREATE TABLE IF NOT EXISTS lease(
            tenant TEXT NOT NULL,task TEXT NOT NULL,body TEXT NOT NULL,state TEXT NOT NULL,
            maker TEXT NOT NULL,approver TEXT,evidence TEXT,PRIMARY KEY(tenant,task));
        CREATE TABLE IF NOT EXISTS nonce(
            tenant TEXT NOT NULL,nonce TEXT NOT NULL,PRIMARY KEY(tenant,nonce));
        CREATE TABLE IF NOT EXISTS event(
            seq INTEGER PRIMARY KEY AUTOINCREMENT,tenant TEXT NOT NULL,task TEXT NOT NULL,
            epoch INTEGER NOT NULL,actor TEXT NOT NULL,action TEXT NOT NULL,
            evidence TEXT,at INTEGER NOT NULL);",
        )
        .map_err(sql)?;
        Ok(Self { db })
    }
    // Administrative bootstrap only; intentionally not an agent-facing operation.
    pub fn provision(
        &mut self,
        tenant: &str,
        department: &str,
        principal: &str,
        role: Role,
    ) -> Result<()> {
        if ![tenant, department, principal].iter().all(|v| id(v)) {
            return Err("invalid member".into());
        }
        self.db
            .execute(
                "INSERT INTO member(tenant,department,principal,role) VALUES(?1,?2,?3,?4)",
                params![tenant, department, principal, role.name()],
            )
            .map_err(sql)?;
        Ok(())
    }
    /// Offline privileged enrollment. No email, display name, or role claim
    /// is used. A disabled membership cannot be silently re-enrolled.
    pub fn enroll_identity(
        &mut self,
        tenant: &str,
        department: &str,
        identity: (&str, &str, &str),
        role: Role,
    ) -> Result<String> {
        let principal = principal_ref(identity.0, identity.1, identity.2)?;
        self.provision(tenant, department, &principal, role)?;
        Ok(principal)
    }

    /// Only live memberships of the independently authenticated caller.
    /// The caller must not expose an arbitrary principal lookup to browsers.
    pub fn memberships(&self, principal: &str) -> Result<Vec<MembershipSummary>> {
        if !id(principal) {
            return Err("invalid principal".into());
        }
        let mut stmt = self
            .db
            .prepare(
                "SELECT tenant,department,role FROM member
            WHERE principal=?1 AND enabled=1 ORDER BY tenant,department LIMIT 101",
            )
            .map_err(sql)?;
        let rows = stmt
            .query_map([principal], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })
            .map_err(sql)?;
        let mut result = Vec::new();
        for row in rows {
            let (tenant, department, role) = row.map_err(sql)?;
            result.push(MembershipSummary {
                tenant,
                department,
                role: Role::parse(&role)?,
            });
        }
        if result.len() > 100 {
            return Err("membership listing limit exceeded".into());
        }
        Ok(result)
    }

    /// Read-only redacted dashboard. Membership and records are evaluated in
    /// one SQLite read transaction; no contract, evidence, or user identifiers
    /// are disclosed. Operators see only their own assignments.
    pub fn scope_tasks(
        &mut self,
        principal: &str,
        tenant: &str,
        department: &str,
        after: &str,
        limit: u32,
    ) -> Result<Vec<TaskSummary>> {
        self.list_tasks(principal, tenant, department, after, limit, false)
    }
    pub fn planning_tasks(
        &mut self,
        principal: &str,
        tenant: &str,
        department: &str,
        after: &str,
        limit: u32,
    ) -> Result<Vec<TaskSummary>> {
        self.list_tasks(principal, tenant, department, after, limit, true)
    }
    fn list_tasks(
        &mut self,
        principal: &str,
        tenant: &str,
        department: &str,
        after: &str,
        limit: u32,
        planning_only: bool,
    ) -> Result<Vec<TaskSummary>> {
        if ![principal, tenant, department].iter().all(|v| id(v))
            || (!after.is_empty() && !id(after))
            || !(1..=100).contains(&limit)
        {
            return Err("invalid dashboard query".into());
        }
        let tx = self.db.transaction().map_err(sql)?;
        let role: String = tx
            .query_row(
                "SELECT role FROM member WHERE tenant=?1 AND department=?2
            AND principal=?3 AND enabled=1",
                params![tenant, department, principal],
                |r| r.get(0),
            )
            .map_err(sql)?;
        let role = Role::parse(&role)?;
        let rows = {
            let mut stmt = tx
                .prepare(
                    "SELECT body,state FROM lease WHERE tenant=?1
                AND json_extract(body,'$.department')=?2 AND task>?3
                AND (?4=0 OR json_extract(body,'$.operator')=?5)
                AND (?7=0 OR json_extract(body,'$.context')=?8)
                ORDER BY task LIMIT ?6",
                )
                .map_err(sql)?;
            let values = stmt
                .query_map(
                    params![
                        tenant,
                        department,
                        after,
                        role == Role::Operator,
                        principal,
                        limit,
                        planning_only,
                        PLANNING_CONTEXT
                    ],
                    |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
                )
                .map_err(sql)?;
            let mut result = Vec::new();
            for row in values {
                let (body, state) = row.map_err(sql)?;
                let offer: Offer = serde_json::from_str(&body).map_err(|e| e.to_string())?;
                result.push(TaskSummary {
                    task: offer.task,
                    epoch: offer.epoch,
                    expires: offer.expires,
                    state,
                    agent: offer.agent,
                });
            }
            result
        };
        tx.commit().map_err(sql)?;
        Ok(rows)
    }

    // Trusted IDP/admin revocation; admission and subsequent effects recheck membership.
    pub fn disable(&mut self, tenant: &str, department: &str, principal: &str) -> Result<()> {
        if self
            .db
            .execute(
                "UPDATE member SET enabled=0 WHERE tenant=?1 AND department=?2 AND principal=?3",
                params![tenant, department, principal],
            )
            .map_err(sql)?
            != 1
        {
            return Err("unknown member".into());
        }
        Ok(())
    }
    fn tx(&mut self) -> Result<Transaction<'_>> {
        self.db
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(sql)
    }
    fn role(db: &Connection, o: &Offer, actor: &str, role: Role) -> Result<()> {
        let found:Option<String>=db.query_row("SELECT role FROM member WHERE tenant=?1 AND department=?2 AND principal=?3 AND enabled=1",
            params![o.tenant,o.department,actor],|r|r.get(0)).optional().map_err(sql)?;
        if found.as_deref() != Some(role.name()) {
            return Err("role or scope denied".into());
        }
        Ok(())
    }
    fn get(db: &Connection, tenant: &str, task: &str) -> Result<Lease> {
        let (body, state, maker, approver, evidence): (
            String,
            String,
            String,
            Option<String>,
            Option<String>,
        ) = db
            .query_row(
                "SELECT body,state,maker,approver,evidence FROM lease WHERE tenant=?1 AND task=?2",
                params![tenant, task],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .map_err(sql)?;
        Ok(Lease {
            offer: serde_json::from_str(&body).map_err(|e| e.to_string())?,
            state,
            maker,
            approver,
            evidence,
        })
    }
    fn event(
        db: &Connection,
        o: &Offer,
        actor: &str,
        action: &str,
        evidence: Option<&str>,
        now: u64,
    ) -> Result<()> {
        db.execute("INSERT INTO event(tenant,task,epoch,actor,action,evidence,at) VALUES(?1,?2,?3,?4,?5,?6,?7)",
            params![o.tenant,o.task,o.epoch,actor,action,evidence,now]).map_err(sql)?;
        Ok(())
    }
    fn live(l: &Lease, epoch: u64, now: u64, state: &str) -> Result<()> {
        if l.state != state || l.offer.epoch != epoch || l.offer.expires <= now {
            return Err("stale or invalid lease".into());
        }
        Ok(())
    }
    pub fn propose(&mut self, actor: &str, o: &Offer, now: u64) -> Result<()> {
        if ![&o.tenant, &o.department, &o.task, &o.operator, &o.agent]
            .iter()
            .all(|v| id(v))
            || o.epoch == 0
            || o.epoch > i64::MAX as u64
            || o.expires <= now
            || o.expires - now > 900
            || o.context.is_empty()
            || o.context.len() > 16384
        {
            return Err("invalid offer".into());
        }
        let tx = self.tx()?;
        Self::role(&tx, o, actor, Role::Manager)?;
        Self::role(&tx, o, &o.operator, Role::Operator)?;
        let previous: Option<String> = tx
            .query_row(
                "SELECT state FROM lease WHERE tenant=?1 AND task=?2",
                params![o.tenant, o.task],
                |r| r.get(0),
            )
            .optional()
            .map_err(sql)?;
        if previous.is_some() {
            let old = Self::get(&tx, &o.tenant, &o.task)?;
            // No automatic retry of running/uncertain or submitted work.
            if !matches!(old.state.as_str(), "revoked" | "rejected")
                || old.offer.department != o.department
                || old.offer.epoch.checked_add(1) != Some(o.epoch)
            {
                return Err("reoffer requires resolved prior epoch".into());
            }
        } else if o.epoch != 1 {
            return Err("first epoch must be one".into());
        }
        let body = serde_json::to_string(o).map_err(|e| e.to_string())?;
        tx.execute("INSERT INTO lease(tenant,task,body,state,maker) VALUES(?1,?2,?3,'proposed',?4)
            ON CONFLICT(tenant,task) DO UPDATE SET body=excluded.body,state='proposed',maker=excluded.maker,approver=NULL,evidence=NULL",
            params![o.tenant,o.task,body,actor]).map_err(sql)?;
        Self::event(&tx, o, actor, "propose", None, now)?;
        tx.commit().map_err(sql)
    }
    pub fn approve(
        &mut self,
        tenant: &str,
        task: &str,
        epoch: u64,
        actor: &str,
        now: u64,
    ) -> Result<()> {
        self.approve_checked(tenant, task, epoch, actor, now, None)
    }
    /// Application boundary: require the exact requested department inside
    /// the state-changing transaction, not via a separate preflight read.
    pub fn approve_scoped(
        &mut self,
        scope: (&str, &str),
        task: &str,
        epoch: u64,
        actor: &str,
        now: u64,
    ) -> Result<()> {
        self.approve_checked(scope.0, task, epoch, actor, now, Some((scope.1, false)))
    }
    /// Preview-only decision. The marker check is inside the write transaction.
    pub fn approve_planning(
        &mut self,
        scope: (&str, &str),
        task: &str,
        epoch: u64,
        actor: &str,
        now: u64,
    ) -> Result<()> {
        self.approve_checked(scope.0, task, epoch, actor, now, Some((scope.1, true)))
    }
    fn approve_checked(
        &mut self,
        tenant: &str,
        task: &str,
        epoch: u64,
        actor: &str,
        now: u64,
        boundary: Option<(&str, bool)>,
    ) -> Result<()> {
        let tx = self.tx()?;
        let l = Self::get(&tx, tenant, task)?;
        if boundary.is_some_and(|(d, planning)| {
            d != l.offer.department || (planning && l.offer.context != PLANNING_CONTEXT)
        }) {
            return Err("scope denied".into());
        }
        Self::role(&tx, &l.offer, actor, Role::Supervisor)?;
        Self::role(&tx, &l.offer, &l.maker, Role::Manager)?;
        Self::role(&tx, &l.offer, &l.offer.operator, Role::Operator)?;
        Self::live(&l, epoch, now, "proposed")?;
        if actor == l.maker || actor == l.offer.operator {
            return Err("independent approval required".into());
        }
        tx.execute(
            "UPDATE lease SET state='approved',approver=?3 WHERE tenant=?1 AND task=?2",
            params![tenant, task, actor],
        )
        .map_err(sql)?;
        Self::event(&tx, &l.offer, actor, "approve", None, now)?;
        tx.commit().map_err(sql)
    }
    // The trusted host supplies the real signature verifier. It receives the
    // stored offer, not a worker-provided expected context. Nonce and state
    // commit together; verification failure commits neither.
    pub fn admit<F>(
        &mut self,
        tenant: &str,
        task: &str,
        epoch: u64,
        actor: &str,
        now: u64,
        verify: F,
    ) -> Result<()>
    where
        F: FnOnce(&Offer) -> Result<(String, u64)>,
    {
        let tx = self.tx()?;
        let l = Self::get(&tx, tenant, task)?;
        Self::role(&tx, &l.offer, actor, Role::Operator)?;
        Self::live(&l, epoch, now, "approved")?;
        if l.offer.context == PLANNING_CONTEXT {
            return Err("planning-only assignment cannot be admitted".into());
        }
        if actor != l.offer.operator {
            return Err("wrong operator".into());
        }
        Self::role(&tx, &l.offer, &l.maker, Role::Manager)?;
        Self::role(
            &tx,
            &l.offer,
            l.approver.as_deref().ok_or("missing approval")?,
            Role::Supervisor,
        )?;
        let (nonce, signed_expiry) = verify(&l.offer)?;
        if !id(&nonce) || signed_expiry <= now || signed_expiry > l.offer.expires {
            return Err("invalid nonce or signed expiry".into());
        }
        let mut admitted = l.offer.clone();
        admitted.expires = signed_expiry;
        let body = serde_json::to_string(&admitted).map_err(|e| e.to_string())?;
        tx.execute(
            "INSERT INTO nonce(tenant,nonce) VALUES(?1,?2)",
            params![tenant, nonce],
        )
        .map_err(sql)?;
        tx.execute(
            "UPDATE lease SET state='running',body=?3 WHERE tenant=?1 AND task=?2",
            params![tenant, task, body],
        )
        .map_err(sql)?;
        Self::event(&tx, &l.offer, actor, "admit", None, now)?;
        tx.commit().map_err(sql)
    }
    // Revocation fences further local result commits; it is not OS termination.
    // Running assignments become uncertain, never automatically eligible for retry.
    pub fn revoke(
        &mut self,
        tenant: &str,
        task: &str,
        epoch: u64,
        actor: &str,
        now: u64,
    ) -> Result<()> {
        let tx = self.tx()?;
        let l = Self::get(&tx, tenant, task)?;
        Self::role(&tx, &l.offer, actor, Role::Supervisor)?;
        if l.offer.epoch != epoch
            || !matches!(l.state.as_str(), "proposed" | "approved" | "running")
        {
            return Err("cannot revoke lease".into());
        }
        let state = if l.state == "running" {
            "uncertain"
        } else {
            "revoked"
        };
        tx.execute(
            "UPDATE lease SET state=?3 WHERE tenant=?1 AND task=?2",
            params![tenant, task, state],
        )
        .map_err(sql)?;
        Self::event(&tx, &l.offer, actor, state, None, now)?;
        tx.commit().map_err(sql)
    }
    /// Planning cancellation has no authority to terminate a running task or
    /// affect any non-preview assignment, even if the wrong DB was configured.
    pub fn revoke_planning(
        &mut self,
        scope: (&str, &str),
        task: &str,
        epoch: u64,
        actor: &str,
        now: u64,
    ) -> Result<()> {
        let tx = self.tx()?;
        let l = Self::get(&tx, scope.0, task)?;
        Self::role(&tx, &l.offer, actor, Role::Supervisor)?;
        if l.offer.department != scope.1
            || l.offer.context != PLANNING_CONTEXT
            || l.offer.epoch != epoch
            || !matches!(l.state.as_str(), "proposed" | "approved")
        {
            return Err("planning cancellation denied".into());
        }
        tx.execute(
            "UPDATE lease SET state='revoked' WHERE tenant=?1 AND task=?2",
            params![scope.0, task],
        )
        .map_err(sql)?;
        Self::event(&tx, &l.offer, actor, "revoked", None, now)?;
        tx.commit().map_err(sql)
    }
    pub fn submit(
        &mut self,
        tenant: &str,
        task: &str,
        epoch: u64,
        actor: &str,
        evidence: &str,
        now: u64,
    ) -> Result<()> {
        let tx = self.tx()?;
        let l = Self::get(&tx, tenant, task)?;
        Self::role(&tx, &l.offer, actor, Role::Operator)?;
        Self::live(&l, epoch, now, "running")?;
        if actor != l.offer.operator || !digest(evidence) {
            return Err("invalid submission".into());
        }
        Self::role(&tx, &l.offer, &l.maker, Role::Manager)?;
        Self::role(
            &tx,
            &l.offer,
            l.approver.as_deref().ok_or("missing approval")?,
            Role::Supervisor,
        )?;
        tx.execute(
            "UPDATE lease SET state='submitted',evidence=?3 WHERE tenant=?1 AND task=?2",
            params![tenant, task, evidence],
        )
        .map_err(sql)?;
        Self::event(&tx, &l.offer, actor, "submit", Some(evidence), now)?;
        tx.commit().map_err(sql)
    }
    // Reviewer must provide independent evidence digest; host must independently
    // verify its semantics before calling accept. A digest is not proof of truth.
    pub fn review(
        &mut self,
        tenant: &str,
        task: &str,
        epoch: u64,
        actor: &str,
        review: Review<'_>,
        now: u64,
    ) -> Result<()> {
        let Review {
            expected,
            review_digest,
            accept,
        } = review;
        let tx = self.tx()?;
        let l = Self::get(&tx, tenant, task)?;
        Self::role(&tx, &l.offer, actor, Role::Reviewer)?;
        if l.state != "submitted"
            || l.offer.epoch != epoch
            || l.evidence.as_deref() != Some(expected)
            || !digest(review_digest)
            || expected == review_digest
            || actor == l.offer.operator
            || actor == l.maker
            || l.approver.as_deref() == Some(actor)
        {
            return Err("independent review required".into());
        }
        if accept {
            Self::role(&tx, &l.offer, &l.offer.operator, Role::Operator)?;
            Self::role(&tx, &l.offer, &l.maker, Role::Manager)?;
            Self::role(
                &tx,
                &l.offer,
                l.approver.as_deref().ok_or("missing approval")?,
                Role::Supervisor,
            )?;
        }
        let state = if accept { "accepted" } else { "rejected" };
        tx.execute(
            "UPDATE lease SET state=?3 WHERE tenant=?1 AND task=?2",
            params![tenant, task, state],
        )
        .map_err(sql)?;
        Self::event(&tx, &l.offer, actor, state, Some(review_digest), now)?;
        tx.commit().map_err(sql)
    }
    pub fn audit(&self, tenant: &str, task: &str, actor: &str) -> Result<(Lease, u64)> {
        let l = Self::get(&self.db, tenant, task)?;
        Self::role(&self.db, &l.offer, actor, Role::Auditor)?;
        let n = self
            .db
            .query_row(
                "SELECT COUNT(*) FROM event WHERE tenant=?1 AND task=?2",
                params![tenant, task],
                |r| r.get(0),
            )
            .map_err(sql)?;
        Ok((l, n))
    }
}

#[cfg(test)]
mod dashboard_tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};
    struct Temp(std::path::PathBuf);
    impl Temp {
        fn new() -> Self {
            let p = std::env::temp_dir().join(format!(
                "jani-dispatch-dashboard-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir(&p).unwrap();
            Self(p)
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    const ISS: &str = "https://accounts.example.test/oidc";
    const RP: &str = "jani-control";
    fn enroll(db: &mut DispatchStore, tenant: &str, dept: &str, sub: &str, role: Role) -> String {
        db.enroll_identity(tenant, dept, (ISS, RP, sub), role)
            .unwrap()
    }
    fn offer(tenant: &str, dept: &str, task: &str, operator: &str) -> Offer {
        Offer {
            tenant: tenant.into(),
            department: dept.into(),
            task: task.into(),
            operator: operator.into(),
            agent: "jani-review".into(),
            epoch: 1,
            expires: 200,
            context: "PRIVATE_CONTRACT".into(),
        }
    }
    #[test]
    fn exact_rp_identity_and_persistent_memberships() {
        let temp = Temp::new();
        let path = temp.0.join("dispatch.sqlite");
        let mut db = DispatchStore::open(&path).unwrap();
        let a = enroll(&mut db, "bank-a", "risk", "subject", Role::Manager);
        assert_eq!(a, principal_ref(ISS, RP, "subject").unwrap());
        for other in [
            principal_ref("https://other.example.test", RP, "subject").unwrap(),
            principal_ref(ISS, "other-rp", "subject").unwrap(),
            principal_ref(ISS, RP, "other").unwrap(),
        ] {
            assert_ne!(a, other);
            assert!(db.memberships(&other).unwrap().is_empty());
        }
        assert!(db
            .enroll_identity("bank-a", "risk", (ISS, RP, "subject"), Role::Supervisor)
            .is_err());
        assert!(principal_ref(ISS, RP, "").is_err());
        drop(db);
        let mut db = DispatchStore::open(&path).unwrap();
        assert_eq!(
            db.memberships(&a).unwrap(),
            vec![MembershipSummary {
                tenant: "bank-a".into(),
                department: "risk".into(),
                role: Role::Manager
            }]
        );
        db.disable("bank-a", "risk", &a).unwrap();
        assert!(db.memberships(&a).unwrap().is_empty());
        assert!(db
            .enroll_identity("bank-a", "risk", (ISS, RP, "subject"), Role::Manager)
            .is_err());
    }
    #[test]
    fn dashboard_scope_owner_redaction_and_paging() {
        let temp = Temp::new();
        let mut db = DispatchStore::open(&temp.0.join("d.db")).unwrap();
        let m = enroll(&mut db, "bank-a", "risk", "m", Role::Manager);
        let op = enroll(&mut db, "bank-a", "risk", "o", Role::Operator);
        let other = enroll(&mut db, "bank-a", "risk", "o2", Role::Operator);
        let a = enroll(&mut db, "bank-a", "risk", "a", Role::Auditor);
        let m2 = enroll(&mut db, "bank-a", "sales", "m", Role::Manager);
        enroll(&mut db, "bank-a", "sales", "o", Role::Operator);
        db.propose(&m, &offer("bank-a", "risk", "T1", &op), 100)
            .unwrap();
        db.propose(&m, &offer("bank-a", "risk", "T2", &other), 100)
            .unwrap();
        db.propose(&m2, &offer("bank-a", "sales", "T3", &op), 100)
            .unwrap();
        let rows = db.scope_tasks(&a, "bank-a", "risk", "", 100).unwrap();
        assert_eq!(
            rows.iter().map(|v| v.task.as_str()).collect::<Vec<_>>(),
            ["T1", "T2"]
        );
        assert_eq!(
            db.scope_tasks(&a, "bank-a", "risk", "T1", 1).unwrap()[0].task,
            "T2"
        );
        assert_eq!(
            db.scope_tasks(&op, "bank-a", "risk", "", 100)
                .unwrap()
                .len(),
            1
        );
        assert!(!serde_json::to_string(&rows)
            .unwrap()
            .contains("PRIVATE_CONTRACT"));
        assert!(!serde_json::to_string(&rows).unwrap().contains(&op));
        for (tenant, dept) in [("bank-b", "risk"), ("bank-a", "sales")] {
            assert!(db.scope_tasks(&a, tenant, dept, "", 100).is_err());
        }
        assert!(db.scope_tasks(&a, "bank-a", "risk", "", 101).is_err());
        db.disable("bank-a", "risk", &a).unwrap();
        assert!(db.scope_tasks(&a, "bank-a", "risk", "", 100).is_err());
    }
    #[test]
    fn approval_is_transactionally_scoped_and_rechecks_revocation() {
        let temp = Temp::new();
        let mut db = DispatchStore::open(&temp.0.join("d.db")).unwrap();
        let m = enroll(&mut db, "bank-a", "risk", "m", Role::Manager);
        let op = enroll(&mut db, "bank-a", "risk", "o", Role::Operator);
        let s = enroll(&mut db, "bank-a", "risk", "s", Role::Supervisor);
        enroll(&mut db, "bank-a", "sales", "s", Role::Supervisor);
        let a = enroll(&mut db, "bank-a", "risk", "a", Role::Auditor);
        db.propose(&m, &offer("bank-a", "risk", "T1", &op), 100)
            .unwrap();
        assert!(db
            .approve_scoped(("bank-a", "sales"), "T1", 1, &s, 101)
            .is_err());
        assert!(db
            .approve_scoped(("bank-a", "risk"), "T1", 2, &s, 101)
            .is_err());
        assert!(db
            .approve_scoped(("bank-a", "risk"), "T1", 1, &m, 101)
            .is_err());
        assert_eq!(db.audit("bank-a", "T1", &a).unwrap().1, 1);
        db.approve_scoped(("bank-a", "risk"), "T1", 1, &s, 101)
            .unwrap();
        assert!(db
            .approve_scoped(("bank-a", "risk"), "T1", 1, &s, 101)
            .is_err());
        db.propose(&m, &offer("bank-a", "risk", "T2", &op), 100)
            .unwrap();
        db.disable("bank-a", "risk", &m).unwrap();
        assert!(db
            .approve_scoped(("bank-a", "risk"), "T2", 1, &s, 101)
            .is_err());
        assert_eq!(db.audit("bank-a", "T2", &a).unwrap().0.state, "proposed");
    }
    #[test]
    fn planning_marker_is_never_executable_and_cannot_touch_live_offers() {
        let temp = Temp::new();
        let mut db = DispatchStore::open(&temp.0.join("d.db")).unwrap();
        let m = enroll(&mut db, "bank-a", "risk", "m", Role::Manager);
        let op = enroll(&mut db, "bank-a", "risk", "o", Role::Operator);
        let s = enroll(&mut db, "bank-a", "risk", "s", Role::Supervisor);
        let a = enroll(&mut db, "bank-a", "risk", "a", Role::Auditor);
        db.propose(&m, &offer("bank-a", "risk", "T1", &op), 100)
            .unwrap();
        let mut plan = offer("bank-a", "risk", "T2", &op);
        plan.context = PLANNING_CONTEXT.into();
        db.propose(&m, &plan, 100).unwrap();
        assert_eq!(
            db.planning_tasks(&a, "bank-a", "risk", "", 100)
                .unwrap()
                .iter()
                .map(|r| r.task.as_str())
                .collect::<Vec<_>>(),
            ["T2"]
        );
        assert!(db
            .approve_planning(("bank-a", "risk"), "T1", 1, &s, 101)
            .is_err());
        assert!(db
            .revoke_planning(("bank-a", "risk"), "T1", 1, &s, 101)
            .is_err());
        db.approve_planning(("bank-a", "risk"), "T2", 1, &s, 101)
            .unwrap();
        assert!(db
            .admit("bank-a", "T2", 1, &op, 102, |_| panic!(
                "must never call verifier"
            ))
            .is_err());
        db.revoke_planning(("bank-a", "risk"), "T2", 1, &s, 103)
            .unwrap();
        assert_eq!(db.audit("bank-a", "T2", &a).unwrap().0.state, "revoked");
    }
}
