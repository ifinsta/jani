pub mod mirror;
//! Transactional single-host SQLite state and transition outbox.
//! No network service or coordinator is required.
//! External side effects are never claimed atomic with a SQLite commit.
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_BYTES: u64 = 8_000_000;
const DB_NAME: &str = "state.sqlite3";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TransitionEvent {
    pub tenant: String,
    pub actor: String,
    pub action: String,
    pub payload_digest: String,
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn safe(path: &Path) -> Result<(), String> {
    match fs::symlink_metadata(path) {
        Ok(m) if m.file_type().is_symlink() => Err(format!("symlink refused: {}", path.display())),
        Ok(_) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}

pub struct Store {
    db: Connection,
    path: PathBuf,
}

impl Store {
    pub fn open(state_dir: &Path) -> Result<Self, String> {
        let dir = state_dir.join("attention");
        safe(state_dir)?;
        safe(&dir)?;
        fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let path = dir.join(DB_NAME);
        safe(&path)?;
        let db = Connection::open(&path).map_err(|e| format!("open attention database: {e}"))?;
        db.busy_timeout(std::time::Duration::from_secs(5))
            .map_err(|e| e.to_string())?;
        db.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=FULL;
             PRAGMA foreign_keys=ON;
             CREATE TABLE IF NOT EXISTS schema_migrations (
                version INTEGER PRIMARY KEY, installed_at INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS attention_state (
                id INTEGER PRIMARY KEY CHECK (id=1),
                body TEXT NOT NULL,
                revision INTEGER NOT NULL DEFAULT 1
             );
             CREATE TABLE IF NOT EXISTS attention_outbox (
                seq INTEGER PRIMARY KEY AUTOINCREMENT,
                tenant TEXT NOT NULL,
                actor TEXT NOT NULL,
                action TEXT NOT NULL,
                payload_digest TEXT NOT NULL,
                created_at INTEGER NOT NULL
             );
             CREATE INDEX IF NOT EXISTS attention_outbox_tenant
                ON attention_outbox (tenant,seq);",
        )
        .map_err(|e| format!("attention schema migration: {e}"))?;
        db.execute(
            "INSERT OR IGNORE INTO schema_migrations (version,installed_at) VALUES (1,?1)",
            params![now()],
        )
        .map_err(|e| e.to_string())?;
        Ok(Self { db, path })
    }

    pub fn transact<S, T, F>(
        &mut self,
        legacy_path: &Path,
        initial: S,
        event: TransitionEvent,
        f: F,
    ) -> Result<T, String>
    where
        S: Serialize + DeserializeOwned,
        F: FnOnce(&mut S) -> Result<(T, bool), String>,
    {
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| format!("begin attention transaction: {e}"))?;
        let existing: Option<String> = tx
            .query_row(
                "SELECT body FROM attention_state WHERE id=1",
                [],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        let legacy_present = existing.is_none() && legacy_path.exists();
        let mut state: S = if let Some(body) = existing {
            serde_json::from_str(&body)
                .map_err(|e| format!("invalid stored attention snapshot: {e}"))?
        } else if legacy_present {
            safe(legacy_path)?;
            let size = fs::metadata(legacy_path).map_err(|e| e.to_string())?.len();
            if size > MAX_BYTES {
                return Err("legacy attention state exceeds limit".into());
            }
            let bytes = fs::read(legacy_path).map_err(|e| e.to_string())?;
            serde_json::from_slice(&bytes)
                .map_err(|e| format!("invalid legacy attention state; migration refused: {e}"))?
        } else {
            initial
        };
        let (value, changed) = f(&mut state)?;
        if changed || legacy_present {
            let body = serde_json::to_string(&state).map_err(|e| e.to_string())?;
            if body.len() as u64 > MAX_BYTES {
                return Err("attention snapshot exceeds storage ceiling".into());
            }
            tx.execute(
                "INSERT INTO attention_state (id,body) VALUES (1,?1)
                 ON CONFLICT(id) DO UPDATE SET body=excluded.body,revision=revision+1",
                params![body],
            )
            .map_err(|e| e.to_string())?;
        }
        if changed {
            tx.execute(
                "INSERT INTO attention_outbox
                 (tenant,actor,action,payload_digest,created_at) VALUES (?1,?2,?3,?4,?5)",
                params![
                    event.tenant,
                    event.actor,
                    event.action,
                    event.payload_digest,
                    now()
                ],
            )
            .map_err(|e| e.to_string())?;
        }
        tx.commit()
            .map_err(|e| format!("commit attention transaction: {e}"))?;
        Ok(value)
    }

    pub fn verify(&self) -> Result<Value, String> {
        let integrity: String = self
            .db
            .query_row("PRAGMA integrity_check", [], |r| r.get(0))
            .map_err(|e| e.to_string())?;
        let violations: i64 = self
            .db
            .query_row("PRAGMA foreign_key_check", [], |_| Ok(1_i64))
            .optional()
            .map_err(|e| e.to_string())?
            .unwrap_or(0);
        let events: i64 = self
            .db
            .query_row("SELECT COUNT(*) FROM attention_outbox", [], |r| r.get(0))
            .map_err(|e| e.to_string())?;
        let revision: Option<i64> = self
            .db
            .query_row("SELECT revision FROM attention_state WHERE id=1", [], |r| r.get(0))
            .optional()
            .map_err(|e| e.to_string())?;
        Ok(json!({
            "ok": integrity == "ok" && violations == 0,
            "integrity": integrity,
            "foreignKeyViolation": violations != 0,
            "outboxEvents": events,
            "snapshotRevision": revision,
            "path": self.path.display().to_string()
        }))
    }

    pub fn events(&self, tenant: &str, limit: u32) -> Result<Value, String> {
        let mut stmt = self
            .db
            .prepare(
                "SELECT seq,actor,action,payload_digest,created_at
                 FROM attention_outbox WHERE tenant=?1 ORDER BY seq DESC LIMIT ?2",
            )
            .map_err(|e| e.to_string())?;
        let items = stmt
            .query_map(params![tenant, limit.min(200)], |row| {
                Ok(json!({
                    "sequence": row.get::<_,i64>(0)?,
                    "actor": row.get::<_,String>(1)?,
                    "action": row.get::<_,String>(2)?,
                    "payloadDigest": row.get::<_,String>(3)?,
                    "createdAt": row.get::<_,i64>(4)?
                }))
            })
            .map_err(|e| e.to_string())?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|e| e.to_string())?;
        Ok(json!(items))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
    struct Counter {
        count: u32,
    }

    fn temp() -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "jani-store-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&p).unwrap();
        p
    }

    fn event() -> TransitionEvent {
        TransitionEvent {
            tenant: "org-a".into(),
            actor: "agent-1".into(),
            action: "test.counter.increment".into(),
            payload_digest: "sha256:test".into(),
        }
    }

    #[test]
    fn committed_state_and_event_survive_reopen() {
        let dir = temp();
        let legacy = dir.join("attention/state.json");
        {
            let mut store = Store::open(&dir).unwrap();
            store.transact(&legacy, Counter { count: 0 }, event(), |s| {
                s.count += 1;
                Ok(((), true))
            }).unwrap();
            assert_eq!(store.verify().unwrap()["outboxEvents"], 1);
        }
        let mut reopened = Store::open(&dir).unwrap();
        let count = reopened.transact(&legacy, Counter { count: 0 }, event(), |s| {
            Ok((s.count, false))
        }).unwrap();
        assert_eq!(count, 1);
        drop(reopened);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn rejected_changes_roll_back_without_event() {
        let dir = temp();
        let legacy = dir.join("attention/state.json");
        let mut store = Store::open(&dir).unwrap();
        let result: Result<(), String> = store.transact(
            &legacy,
            Counter { count: 0 },
            event(),
            |s| {
                s.count = 99;
                Err("rejected".into())
            },
        );
        assert!(result.is_err());
        assert_eq!(store.verify().unwrap()["outboxEvents"], 0);
        drop(store);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn legacy_snapshot_is_migrated_without_reset() {
        let dir = temp();
        fs::create_dir_all(dir.join("attention")).unwrap();
        let legacy = dir.join("attention/state.json");
        fs::write(&legacy, r#"{"count":7}"#).unwrap();
        let mut store = Store::open(&dir).unwrap();
        let value = store.transact(&legacy, Counter { count: 0 }, event(), |s| {
            Ok((s.count, false))
        }).unwrap();
        assert_eq!(value, 7);
        assert_eq!(store.verify().unwrap()["snapshotRevision"], 1);
        drop(store);
        fs::remove_dir_all(dir).unwrap();
    }
}
