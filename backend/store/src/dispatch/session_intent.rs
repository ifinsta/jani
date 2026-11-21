//! One-use session-to-dispatch handoff. Only a trusted SIWI host can issue
//! records and only an OS-privileged broker can inspect/claim them. Signed
//! identity is NEVER sent to the browser or workers. This is single-host.
use super::{digest, id, principal_ref, sql, DispatchStore, Result, Role, PLANNING_CONTEXT};
use rusqlite::{params, Connection};

#[derive(Clone, Debug)]
pub struct SessionIntent {
    pub id: String,
    pub tenant: String,
    pub department: String,
    pub task: String,
    pub epoch: u64,
    pub principal: String,
    pub issuer: String,
    pub client: String,
    pub subject: String,
    pub session_hash: String,
    pub identity_json: Vec<u8>,
    pub expires: u64,
    pub state: String,
}
#[derive(Clone, Copy)]
pub struct IssueSession<'a> {
    pub id: &'a str,
    pub tenant: &'a str,
    pub department: &'a str,
    pub task: &'a str,
    pub epoch: u64,
    pub principal: &'a str,
    pub issuer: &'a str,
    pub client: &'a str,
    pub subject: &'a str,
    pub session_hash: &'a str,
    pub identity_json: &'a [u8],
    pub expires: u64,
}
pub(super) fn setup(db: &Connection) -> Result<()> {
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS session_intent(
       id TEXT PRIMARY KEY,
       tenant TEXT NOT NULL,department TEXT NOT NULL,task TEXT NOT NULL,
       epoch INTEGER NOT NULL,principal TEXT NOT NULL,issuer TEXT NOT NULL,
       client TEXT NOT NULL,subject TEXT NOT NULL,session_hash TEXT NOT NULL,
       identity_json BLOB NOT NULL,expires INTEGER NOT NULL,
       state TEXT NOT NULL CHECK(state IN ('issued','claimed','revoked')),
       UNIQUE(tenant,task,epoch));",
    )
    .map_err(sql)
}
fn read(db: &Connection, id_value: &str) -> Result<SessionIntent> {
    db.query_row(
        "SELECT id,tenant,department,task,epoch,principal,issuer,client,
       subject,session_hash,identity_json,expires,state FROM session_intent WHERE id=?1",
        [id_value],
        |r| {
            Ok(SessionIntent {
                id: r.get(0)?,
                tenant: r.get(1)?,
                department: r.get(2)?,
                task: r.get(3)?,
                epoch: r.get(4)?,
                principal: r.get(5)?,
                issuer: r.get(6)?,
                client: r.get(7)?,
                subject: r.get(8)?,
                session_hash: r.get(9)?,
                identity_json: r.get(10)?,
                expires: r.get(11)?,
                state: r.get(12)?,
            })
        },
    )
    .map_err(sql)
}
impl DispatchStore {
    /// Caller must derive every field from the server-verified SIWI user,
    /// except tenant/task chosen as a requested scope. All organization
    /// permissions and maker-checker state are checked in this transaction.
    /// The signature is later independently validated by native Jani.
    pub fn issue_session(&mut self, i: IssueSession<'_>, now: u64) -> Result<()> {
        if ![i.id, i.tenant, i.department, i.task, i.principal]
            .iter()
            .all(|v| id(v))
            || !digest(i.session_hash)
            || i.epoch == 0
            || i.epoch > i64::MAX as u64
            || i.expires <= now
            || i.expires - now > 60
            || i.identity_json.len() > 4096
            || principal_ref(i.issuer, i.client, i.subject)? != i.principal
        {
            return Err("invalid authenticated session intent".into());
        }
        let assertion: serde_json::Value = serde_json::from_slice(i.identity_json)
            .map_err(|_| "invalid signed SIWI identity JSON")?;
        let signature = assertion
            .get("signature")
            .ok_or("missing identity signature")?;
        if assertion["apiVersion"] != "jani.dev/v1alpha1"
            || assertion["kind"] != "IdentityAssertion"
            || assertion["tenant"] != i.tenant
            || assertion["principal"] != i.principal
            || assertion["subjectType"] != "human"
            || assertion["issuedAt"].as_u64().is_none_or(|t| t > now)
            || assertion["expiresAt"].as_u64() != Some(i.expires)
            || signature["keyId"].as_str().is_none_or(|v| !id(v))
            || signature["value"].as_str().is_none_or(|v| {
                v.len() != 128
                    || !v
                        .bytes()
                        .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
            })
        {
            return Err("session identity claims mismatch".into());
        }
        let tx = self.tx()?;
        let l = Self::get(&tx, i.tenant, i.task)?;
        if l.offer.department != i.department
            || l.offer.operator != i.principal
            || l.offer.context != PLANNING_CONTEXT
            || l.offer.epoch != i.epoch
            || i.expires > l.offer.expires
        {
            return Err("planning offer differs from session identity".into());
        }
        Self::live(&l, i.epoch, now, "approved")?;
        Self::role(&tx, &l.offer, i.principal, Role::Operator)?;
        Self::role(&tx, &l.offer, &l.maker, Role::Manager)?;
        Self::role(
            &tx,
            &l.offer,
            l.approver.as_deref().ok_or("no independent approval")?,
            Role::Supervisor,
        )?;
        tx.execute(
            "INSERT INTO session_intent
           (id,tenant,department,task,epoch,principal,issuer,client,subject,
            session_hash,identity_json,expires,state)
            VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,'issued')",
            params![
                i.id,
                i.tenant,
                i.department,
                i.task,
                i.epoch,
                i.principal,
                i.issuer,
                i.client,
                i.subject,
                i.session_hash,
                i.identity_json,
                i.expires
            ],
        )
        .map_err(sql)?;
        Self::event(&tx, &l.offer, i.principal, "session-authorized", None, now)?;
        tx.commit().map_err(sql)
    }
    /// Trusted same-host broker only. Contains signed identity assertion.
    /// No public HTTP endpoint may call or serialize this method.
    pub fn pending_session(&self, id_value: &str) -> Result<SessionIntent> {
        if !id(id_value) {
            return Err("invalid session reference".into());
        }
        read(&self.db, id_value)
    }
    /// Call ONLY after verifying the exact session_hash is still active in
    /// the shared, encrypted SIWI Redis store. Retries fail after consumption.
    pub fn claim_session(
        &mut self,
        id_value: &str,
        verified_hash: &str,
        now: u64,
    ) -> Result<SessionIntent> {
        if !id(id_value) || !digest(verified_hash) {
            return Err("invalid verified session reference".into());
        }
        let tx = self.tx()?;
        let i = read(&tx, id_value)?;
        if i.state != "issued" || i.session_hash != verified_hash || i.expires <= now + 5 {
            return Err("session expired, revoked or previously claimed".into());
        }
        let l = Self::get(&tx, &i.tenant, &i.task)?;
        Self::live(&l, i.epoch, now, "approved")?;
        if l.offer.department != i.department
            || l.offer.operator != i.principal
            || l.offer.context != PLANNING_CONTEXT
        {
            return Err("approved planning scope changed".into());
        }
        Self::role(&tx, &l.offer, &i.principal, Role::Operator)?;
        Self::role(&tx, &l.offer, &l.maker, Role::Manager)?;
        Self::role(
            &tx,
            &l.offer,
            l.approver.as_deref().ok_or("missing approval")?,
            Role::Supervisor,
        )?;
        if tx
            .execute(
                "UPDATE session_intent SET state='claimed'
             WHERE id=?1 AND state='issued'",
                [id_value],
            )
            .map_err(sql)?
            != 1
        {
            return Err("session intent already claimed".into());
        }
        Self::event(&tx, &l.offer, &i.principal, "session-claimed", None, now)?;
        tx.commit().map_err(sql)?;
        Ok(i)
    }
}

#[cfg(test)]
mod tests {
    use super::super::{Offer, Role, PLANNING_CONTEXT};
    use super::*;
    use std::{
        path::PathBuf,
        thread,
        time::{SystemTime, UNIX_EPOCH},
    };
    struct Temp(PathBuf);
    impl Temp {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "jani-session-intent-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir(&root).unwrap();
            Self(root)
        }
        fn db(&self) -> PathBuf {
            self.0.join("dispatch.db")
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    const ISS: &str = "https://accounts.example.test/oidc";
    const RP: &str = "jani-control";
    const HASH: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    fn setup(db: &mut DispatchStore, task: &str) -> (String, String, String, String) {
        let m = db
            .enroll_identity(
                "bank-a",
                "risk",
                (ISS, RP, &format!("manager-{task}")),
                Role::Manager,
            )
            .unwrap();
        let s = db
            .enroll_identity(
                "bank-a",
                "risk",
                (ISS, RP, &format!("supervisor-{task}")),
                Role::Supervisor,
            )
            .unwrap();
        let o = db
            .enroll_identity(
                "bank-a",
                "risk",
                (ISS, RP, &format!("operator-{task}")),
                Role::Operator,
            )
            .unwrap();
        let a = db
            .enroll_identity(
                "bank-a",
                "risk",
                (ISS, RP, &format!("auditor-{task}")),
                Role::Auditor,
            )
            .unwrap();
        let offer = Offer {
            tenant: "bank-a".into(),
            department: "risk".into(),
            task: task.into(),
            operator: o.clone(),
            agent: "planning-only".into(),
            epoch: 1,
            expires: 300,
            context: PLANNING_CONTEXT.into(),
        };
        db.propose(&m, &offer, 100).unwrap();
        db.approve_planning(("bank-a", "risk"), task, 1, &s, 101)
            .unwrap();
        (m, s, o, a)
    }
    fn assertion(principal: &str, expires: u64) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "apiVersion":"jani.dev/v1alpha1","kind":"IdentityAssertion",
            "principal":principal,"tenant":"bank-a","subjectType":"human",
            "issuedAt":101,"expiresAt":expires,"nonce":"synthetic-nonce",
            "signature":{"keyId":"fixture-key","value":"b".repeat(128)}
        }))
        .unwrap()
    }
    fn issue<'a>(
        id: &'a str,
        task: &'a str,
        sub: &'a str,
        principal: &'a str,
        bytes: &'a [u8],
    ) -> IssueSession<'a> {
        IssueSession {
            id,
            tenant: "bank-a",
            department: "risk",
            task,
            epoch: 1,
            principal,
            issuer: ISS,
            client: RP,
            subject: sub,
            session_hash: HASH,
            identity_json: bytes,
            expires: 150,
        }
    }
    #[test]
    fn authenticated_one_time_claim_is_durable_and_auditable() {
        let tmp = Temp::new();
        let path = tmp.db();
        let mut db = DispatchStore::open(&path).unwrap();
        let (_, _, operator, auditor) = setup(&mut db, "T1");
        let bytes = assertion(&operator, 150);
        let i = issue("one-time-reference", "T1", "operator-T1", &operator, &bytes);
        db.issue_session(i, 102).unwrap();
        let pending = db.pending_session("one-time-reference").unwrap();
        assert_eq!(pending.state, "issued");
        assert_eq!(pending.identity_json, bytes);
        assert_eq!(db.audit("bank-a", "T1", &auditor).unwrap().1, 3);
        assert!(db
            .claim_session("one-time-reference", &"b".repeat(64), 103)
            .is_err());
        let got = db.claim_session("one-time-reference", HASH, 103).unwrap();
        assert_eq!(got.principal, operator);
        assert_eq!(db.audit("bank-a", "T1", &auditor).unwrap().1, 4);
        assert!(db.claim_session("one-time-reference", HASH, 104).is_err());
        drop(db);
        let mut reopened = DispatchStore::open(&path).unwrap();
        assert_eq!(
            reopened
                .pending_session("one-time-reference")
                .unwrap()
                .state,
            "claimed"
        );
        assert!(reopened
            .claim_session("one-time-reference", HASH, 105)
            .is_err());
    }
    #[test]
    fn issuance_requires_exact_role_scope_and_short_lived_human_identity() {
        let tmp = Temp::new();
        let mut db = DispatchStore::open(&tmp.db()).unwrap();
        let (_, _, op, _) = setup(&mut db, "T1");
        let bytes = assertion(&op, 150);
        let mut i = issue("grant-one", "T1", "operator-T1", &op, &bytes);
        i.principal = "fake";
        assert!(db.issue_session(i, 102).is_err());
        let mut i = issue("grant-one", "T1", "operator-T2", &op, &bytes);
        assert!(db.issue_session(i, 102).is_err());
        i.subject = "operator-T1";
        i.department = "sales";
        assert!(db.issue_session(i, 102).is_err());
        let mut i = issue("grant-one", "T1", "operator-T1", &op, &bytes);
        i.expires = 165;
        assert!(db.issue_session(i, 102).is_err());
        let mut i = issue("grant-one", "T1", "operator-T1", &op, &bytes);
        i.session_hash = "not-a-session-hash";
        assert!(db.issue_session(i, 102).is_err());
        let mut forgery: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        forgery["tenant"] = "bank-b".into();
        let tampered = serde_json::to_vec(&forgery).unwrap();
        assert!(db
            .issue_session(issue("grant-one", "T1", "operator-T1", &op, &tampered), 102)
            .is_err());
        assert!(db.pending_session("grant-one").is_err());
        db.issue_session(issue("grant-one", "T1", "operator-T1", &op, &bytes), 102)
            .unwrap();
        assert!(db
            .issue_session(issue("grant-two", "T1", "operator-T1", &op, &bytes), 102)
            .is_err());
    }
    #[test]
    fn revocations_transactionally_cancel_unclaimed_authorization() {
        let tmp = Temp::new();
        let mut db = DispatchStore::open(&tmp.db()).unwrap();
        let (_, supervisor, operator, _) = setup(&mut db, "T1");
        let bytes = assertion(&operator, 150);
        db.issue_session(
            issue("grant-one", "T1", "operator-T1", &operator, &bytes),
            102,
        )
        .unwrap();
        db.disable("bank-a", "risk", &operator).unwrap();
        assert_eq!(db.pending_session("grant-one").unwrap().state, "revoked");
        assert!(db.claim_session("grant-one", HASH, 103).is_err());
        let (_, supervisor2, operator2, _) = setup(&mut db, "T2");
        let bytes2 = assertion(&operator2, 150);
        db.issue_session(
            issue("grant-two", "T2", "operator-T2", &operator2, &bytes2),
            102,
        )
        .unwrap();
        db.revoke_planning(("bank-a", "risk"), "T2", 1, &supervisor2, 103)
            .unwrap();
        assert_eq!(db.pending_session("grant-two").unwrap().state, "revoked");
        assert!(db.claim_session("grant-two", HASH, 104).is_err());
        assert_ne!(supervisor, supervisor2);
    }
    #[test]
    fn concurrent_claim_cannot_consume_twice() {
        let tmp = Temp::new();
        let path = tmp.db();
        let mut db = DispatchStore::open(&path).unwrap();
        let (_, _, operator, _) = setup(&mut db, "T1");
        let bytes = assertion(&operator, 150);
        db.issue_session(
            issue("grant-one", "T1", "operator-T1", &operator, &bytes),
            102,
        )
        .unwrap();
        drop(db);
        let handles: Vec<_> = (0..4)
            .map(|_| {
                let p = path.clone();
                thread::spawn(move || {
                    DispatchStore::open(&p)
                        .unwrap()
                        .claim_session("grant-one", HASH, 103)
                        .is_ok()
                })
            })
            .collect();
        let passed = handles
            .into_iter()
            .map(|j| j.join().unwrap())
            .filter(|v| *v)
            .count();
        assert_eq!(passed, 1);
    }
    #[test]
    fn manager_or_supervisor_revocation_cancels_subordinate_intents() {
        let tmp = Temp::new();
        let mut db = DispatchStore::open(&tmp.db()).unwrap();
        let (maker, _, op, _) = setup(&mut db, "T1");
        let signed = assertion(&op, 150);
        db.issue_session(
            issue("manager-plan", "T1", "operator-T1", &op, &signed),
            102,
        )
        .unwrap();
        db.disable("bank-a", "risk", &maker).unwrap();
        assert_eq!(db.pending_session("manager-plan").unwrap().state, "revoked");
        assert!(db.claim_session("manager-plan", HASH, 103).is_err());

        let (_, checker, other, _) = setup(&mut db, "T2");
        let signed = assertion(&other, 150);
        db.issue_session(
            issue("checker-plan", "T2", "operator-T2", &other, &signed),
            102,
        )
        .unwrap();
        db.disable("bank-a", "risk", &checker).unwrap();
        assert_eq!(db.pending_session("checker-plan").unwrap().state, "revoked");
        assert!(db.claim_session("checker-plan", HASH, 103).is_err());
    }
}
