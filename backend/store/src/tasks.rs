//! Durable checkpoint and idempotent effect tracking for long-lived digital-employee tasks.
use crate::{harden, now, safe};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs;
use std::path::Path;

const DIR_NAME: &str = "tasks";
const DB_NAME: &str = "state.sqlite3";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskCheckpoint {
    pub tenant: String,
    pub task_id: String,
    pub epoch: u64,
    pub status: String,
    pub run_id: Option<String>,
    pub approval_id: Option<String>,
    pub principal: String,
    pub purpose: String,
    pub workspace: String,
    pub messages: Vec<Value>,
    pub updated_at: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskSummary {
    pub tenant: String,
    pub task_id: String,
    pub epoch: u64,
    pub status: String,
    pub run_id: Option<String>,
    pub approval_id: Option<String>,
    pub principal: String,
    pub updated_at: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EffectGate {
    Proceed,
    AlreadyApplied(Option<Value>),
    InFlight,
}

pub struct TaskStore {
    conn: Connection,
}

impl TaskStore {
    pub fn open(state_dir: &Path) -> Result<Self, String> {
        let dir = state_dir.join(DIR_NAME);
        safe(state_dir)?;
        safe(&dir)?;
        fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let path = dir.join(DB_NAME);
        safe(&path)?;
        let conn = Connection::open(&path).map_err(|e| format!("open task store: {e}"))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o600));
        }
        harden(&conn)?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS task_sessions (
                tenant TEXT NOT NULL,
                task_id TEXT NOT NULL,
                epoch INTEGER NOT NULL DEFAULT 1,
                status TEXT NOT NULL,
                run_id TEXT,
                approval_id TEXT,
                principal TEXT NOT NULL,
                purpose TEXT NOT NULL,
                workspace TEXT NOT NULL,
                messages_json TEXT NOT NULL,
                updated_at INTEGER NOT NULL,
                PRIMARY KEY (tenant, task_id)
            );
            CREATE TABLE IF NOT EXISTS task_steps (
                seq INTEGER PRIMARY KEY AUTOINCREMENT,
                tenant TEXT NOT NULL,
                task_id TEXT NOT NULL,
                epoch INTEGER NOT NULL,
                step_idx INTEGER NOT NULL,
                kind TEXT NOT NULL,
                payload_json TEXT NOT NULL,
                created_at INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS effect_intents (
                tenant TEXT NOT NULL,
                task_id TEXT NOT NULL,
                idempotency_key TEXT NOT NULL,
                epoch INTEGER NOT NULL,
                tool TEXT NOT NULL,
                resource TEXT NOT NULL,
                payload_digest TEXT NOT NULL,
                status TEXT NOT NULL,
                result_json TEXT,
                updated_at INTEGER NOT NULL,
                PRIMARY KEY (tenant, task_id, idempotency_key)
            );",
        )
        .map_err(|e| format!("task schema migration: {e}"))?;
        Ok(Self { conn })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn checkpoint(
        &mut self,
        tenant: &str,
        task_id: &str,
        epoch: u64,
        status: &str,
        run_id: Option<&str>,
        approval_id: Option<&str>,
        principal: &str,
        purpose: &str,
        workspace: &str,
        messages: &[Value],
    ) -> Result<(), String> {
        let encoded = serde_json::to_string(messages).map_err(|e| e.to_string())?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| e.to_string())?;
        let current_epoch: Option<i64> = tx
            .query_row(
                "SELECT epoch FROM task_sessions WHERE tenant=?1 AND task_id=?2",
                params![tenant, task_id],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        if let Some(cur) = current_epoch {
            if (epoch as i64) < cur {
                return Err(format!("stale worker epoch {epoch} < persisted {cur}"));
            }
        }
        tx.execute(
            "INSERT INTO task_sessions(tenant,task_id,epoch,status,run_id,approval_id,principal,purpose,workspace,messages_json,updated_at)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)
             ON CONFLICT(tenant,task_id) DO UPDATE SET
               epoch=excluded.epoch,
               status=excluded.status,
               run_id=excluded.run_id,
               approval_id=excluded.approval_id,
               messages_json=excluded.messages_json,
               updated_at=excluded.updated_at",
            params![
                tenant,
                task_id,
                epoch as i64,
                status,
                run_id,
                approval_id,
                principal,
                purpose,
                workspace,
                encoded,
                now() as i64
            ],
        )
        .map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| e.to_string())
    }

    pub fn record_step(
        &mut self,
        tenant: &str,
        task_id: &str,
        epoch: u64,
        step_idx: usize,
        kind: &str,
        payload: &Value,
    ) -> Result<(), String> {
        let json = serde_json::to_string(payload).map_err(|e| e.to_string())?;
        self.conn
            .execute(
                "INSERT INTO task_steps(tenant,task_id,epoch,step_idx,kind,payload_json,created_at)
                 VALUES(?1,?2,?3,?4,?5,?6,?7)",
                params![
                    tenant,
                    task_id,
                    epoch as i64,
                    step_idx as i64,
                    kind,
                    json,
                    now() as i64
                ],
            )
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn claim_resume_epoch(&mut self, tenant: &str, task_id: &str) -> Result<u64, String> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| e.to_string())?;
        let current: Option<(i64, String)> = tx
            .query_row(
                "SELECT epoch, status FROM task_sessions WHERE tenant=?1 AND task_id=?2",
                params![tenant, task_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        let Some((cur, status)) = current else {
            return Err(format!("no durable checkpoint for task '{task_id}'"));
        };
        if status == "killed" {
            return Err(format!("task '{task_id}' was killed by fleet control"));
        }
        let next = cur.saturating_add(1);
        tx.execute(
            "UPDATE task_sessions SET epoch=?1, updated_at=?2 WHERE tenant=?3 AND task_id=?4",
            params![next, now() as i64, tenant, task_id],
        )
        .map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| e.to_string())?;
        Ok(next as u64)
    }

    pub fn load(&self, tenant: &str, task_id: &str) -> Result<Option<TaskCheckpoint>, String> {
        let row = self
            .conn
            .query_row(
                "SELECT epoch,status,run_id,approval_id,principal,purpose,workspace,messages_json,updated_at
                 FROM task_sessions WHERE tenant=?1 AND task_id=?2",
                params![tenant, task_id],
                |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, Option<String>>(2)?,
                        r.get::<_, Option<String>>(3)?,
                        r.get::<_, String>(4)?,
                        r.get::<_, String>(5)?,
                        r.get::<_, String>(6)?,
                        r.get::<_, String>(7)?,
                        r.get::<_, i64>(8)?,
                    ))
                },
            )
            .optional()
            .map_err(|e| e.to_string())?;
        let Some((
            epoch,
            status,
            run_id,
            approval_id,
            principal,
            purpose,
            workspace,
            messages_json,
            updated_at,
        )) = row
        else {
            return Ok(None);
        };
        let messages: Vec<Value> = serde_json::from_str(&messages_json)
            .map_err(|e| format!("corrupt task checkpoint messages: {e}"))?;
        Ok(Some(TaskCheckpoint {
            tenant: tenant.into(),
            task_id: task_id.into(),
            epoch: epoch.max(0) as u64,
            status,
            run_id,
            approval_id,
            principal,
            purpose,
            workspace,
            messages,
            updated_at: updated_at.max(0) as u64,
        }))
    }

    pub fn list(
        &self,
        tenant: &str,
        status_filter: Option<&str>,
    ) -> Result<Vec<TaskSummary>, String> {
        let mut out = Vec::new();
        let mut stmt = self
            .conn
            .prepare(
                "SELECT task_id,epoch,status,run_id,approval_id,principal,updated_at
                 FROM task_sessions WHERE tenant=?1 ORDER BY updated_at DESC LIMIT 200",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(params![tenant], |r| {
                Ok(TaskSummary {
                    tenant: tenant.into(),
                    task_id: r.get(0)?,
                    epoch: r.get::<_, i64>(1)?.max(0) as u64,
                    status: r.get(2)?,
                    run_id: r.get(3)?,
                    approval_id: r.get(4)?,
                    principal: r.get(5)?,
                    updated_at: r.get::<_, i64>(6)?.max(0) as u64,
                })
            })
            .map_err(|e| e.to_string())?;
        for row in rows {
            let item = row.map_err(|e| e.to_string())?;
            if status_filter.is_none_or(|f| item.status == f) {
                out.push(item);
            }
        }
        Ok(out)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn begin_effect(
        &mut self,
        tenant: &str,
        task_id: &str,
        epoch: u64,
        idempotency_key: &str,
        tool: &str,
        resource: &str,
        payload_digest: &str,
    ) -> Result<EffectGate, String> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| e.to_string())?;
        let existing: Option<(String, Option<String>, String)> = tx
            .query_row(
                "SELECT status, result_json, payload_digest
                 FROM effect_intents WHERE tenant=?1 AND task_id=?2 AND idempotency_key=?3",
                params![tenant, task_id, idempotency_key],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        if let Some((status, result_json, saved_digest)) = existing {
            if saved_digest != payload_digest {
                return Err("idempotency key reused with mismatched payload digest".into());
            }
            if status == "applied" {
                let parsed = result_json
                    .as_deref()
                    .map(serde_json::from_str)
                    .transpose()
                    .map_err(|e| e.to_string())?;
                return Ok(EffectGate::AlreadyApplied(parsed));
            }
            if status == "in_flight" {
                return Ok(EffectGate::InFlight);
            }
        }
        tx.execute(
            "INSERT INTO effect_intents(tenant,task_id,idempotency_key,epoch,tool,resource,payload_digest,status,result_json,updated_at)
             VALUES(?1,?2,?3,?4,?5,?6,?7,'in_flight',NULL,?8)
             ON CONFLICT(tenant,task_id,idempotency_key) DO UPDATE SET
               epoch=excluded.epoch, status='in_flight', result_json=NULL, updated_at=excluded.updated_at",
            params![
                tenant,
                task_id,
                idempotency_key,
                epoch as i64,
                tool,
                resource,
                payload_digest,
                now() as i64
            ],
        )
        .map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| e.to_string())?;
        Ok(EffectGate::Proceed)
    }

    pub fn finish_effect(
        &mut self,
        tenant: &str,
        task_id: &str,
        idempotency_key: &str,
        applied: bool,
        result: Option<&Value>,
    ) -> Result<(), String> {
        let status = if applied { "applied" } else { "failed" };
        let result_json = result
            .map(serde_json::to_string)
            .transpose()
            .map_err(|e| e.to_string())?;
        self.conn
            .execute(
                "UPDATE effect_intents SET status=?1, result_json=?2, updated_at=?3
                 WHERE tenant=?4 AND task_id=?5 AND idempotency_key=?6",
                params![
                    status,
                    result_json,
                    now() as i64,
                    tenant,
                    task_id,
                    idempotency_key
                ],
            )
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn kill_tenant(&mut self, tenant: &str) -> Result<usize, String> {
        self.conn
            .execute(
                "UPDATE task_sessions SET status='killed', epoch=epoch+1, updated_at=?1
                 WHERE tenant=?2 AND status != 'completed'",
                params![now() as i64, tenant],
            )
            .map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn checkpoints_epoch_fencing_and_idempotency_work() {
        let dir = std::env::temp_dir().join(format!("jani-tasks-store-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let mut store = TaskStore::open(&dir).unwrap();
        let msgs = vec![json!({"role": "user", "content": "fix bug"})];
        store
            .checkpoint(
                "t1",
                "task-1",
                1,
                "suspended_approval",
                Some("run-1"),
                Some("apr-1"),
                "u1",
                "SOFTWARE_DEV",
                ".",
                &msgs,
            )
            .unwrap();
        let loaded = store.load("t1", "task-1").unwrap().unwrap();
        assert_eq!(loaded.epoch, 1);
        assert_eq!(loaded.approval_id.as_deref(), Some("apr-1"));
        assert_eq!(loaded.messages.len(), 1);

        let next_epoch = store.claim_resume_epoch("t1", "task-1").unwrap();
        assert_eq!(next_epoch, 2);
        assert!(store
            .checkpoint(
                "t1",
                "task-1",
                1,
                "running",
                None,
                None,
                "u1",
                "SOFTWARE_DEV",
                ".",
                &msgs
            )
            .is_err());

        assert_eq!(
            store
                .begin_effect("t1", "task-1", 2, "idem-1", "repo.write", "repo:a.rs", "d1")
                .unwrap(),
            EffectGate::Proceed
        );
        assert_eq!(
            store
                .begin_effect("t1", "task-1", 2, "idem-1", "repo.write", "repo:a.rs", "d1")
                .unwrap(),
            EffectGate::InFlight
        );
        let res = json!({"written": true});
        store
            .finish_effect("t1", "task-1", "idem-1", true, Some(&res))
            .unwrap();
        assert_eq!(
            store
                .begin_effect("t1", "task-1", 2, "idem-1", "repo.write", "repo:a.rs", "d1")
                .unwrap(),
            EffectGate::AlreadyApplied(Some(res))
        );
        assert!(store
            .begin_effect(
                "t1",
                "task-1",
                2,
                "idem-1",
                "repo.write",
                "repo:a.rs",
                "different"
            )
            .is_err());

        let _ = fs::remove_dir_all(&dir);
    }
}
