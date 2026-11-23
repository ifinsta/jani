//! Single-host dispatch state machine for a trusted manager service.
//! Callers must authenticate principals outside this library. The state directory
//! must be inaccessible to workers; possession of a database handle is privileged.
use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Manager,
    Supervisor,
    Operator,
    Reviewer,
    Auditor,
}
impl Role {
    fn name(self) -> &'static str {
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
        let tx = self.tx()?;
        let l = Self::get(&tx, tenant, task)?;
        Self::role(&tx, &l.offer, actor, Role::Supervisor)?;
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
