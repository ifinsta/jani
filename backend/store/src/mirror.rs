//! Explicit, durable local register for Mirror proposals.
//! A proposal is evidence, not permission or an executable workflow change.
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default().as_secs()
}

pub struct MirrorRegister {
    conn: Connection,
}

impl MirrorRegister {
    pub fn open(state_dir: &Path) -> Result<Self, String> {
        let root = state_dir.join("mirror");
        for path in [state_dir, root.as_path()] {
            if let Ok(metadata) = fs::symlink_metadata(path) {
                if metadata.file_type().is_symlink() {
                    return Err("symlinked Mirror state refused".into());
                }
            }
        }
        fs::create_dir_all(&root).map_err(|e| e.to_string())?;
        let db_path = root.join("improvements.sqlite3");
        if let Ok(metadata) = fs::symlink_metadata(&db_path) {
            if metadata.file_type().is_symlink() {
                return Err("symlinked Mirror database refused".into());
            }
        }
        let conn = Connection::open(&db_path).map_err(|e| e.to_string())?;
        conn.busy_timeout(std::time::Duration::from_secs(5))
            .map_err(|e| e.to_string())?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=FULL;
             PRAGMA foreign_keys=ON;
             CREATE TABLE IF NOT EXISTS mirror_findings (
                id TEXT NOT NULL,
                tenant TEXT NOT NULL,
                run_id TEXT NOT NULL,
                code TEXT NOT NULL,
                body TEXT NOT NULL,
                created_at INTEGER NOT NULL,
                PRIMARY KEY (tenant, id)
             );
             CREATE INDEX IF NOT EXISTS mirror_findings_runs
                 ON mirror_findings(tenant,run_id);"
        ).map_err(|e| format!("Mirror migration: {e}"))?;
        Ok(Self { conn })
    }

    pub fn record(&mut self, tenant: &str, proposals: &[Value]) -> Result<usize, String> {
        let tx = self.conn.transaction().map_err(|e| e.to_string())?;
        let count: i64 = tx.query_row(
            "SELECT COUNT(*) FROM mirror_findings WHERE tenant=?1",
            [tenant], |r| r.get(0)
        ).map_err(|e| e.to_string())?;
        let mut inserted = 0;
        for item in proposals {
            let field = |key| item.get(key).and_then(Value::as_str);
            let id = field("id").ok_or("finding missing id")?;
            let run_id = field("runId").ok_or("finding missing runId")?;
            let code = field("code").ok_or("finding missing code")?;
            if !id.starts_with("mir-") || !id[4..].bytes().all(|b| b.is_ascii_hexdigit())
                || id.len() != 28 || run_id.len() > 128 || code.len() > 64 {
                return Err("invalid Mirror finding identifiers".into());
            }
            if item.get("status").and_then(Value::as_str) != Some("proposed") {
                return Err("only unapproved proposals may enter the register".into());
            }
            let encoded = serde_json::to_string(item).map_err(|e| e.to_string())?;
            if encoded.len() > 16_384 {
                return Err("Mirror proposal exceeds limit".into());
            }
            if let Some(existing) = tx.query_row(
                "SELECT body FROM mirror_findings WHERE tenant=?1 AND id=?2",
                params![tenant,id], |r| r.get::<_,String>(0)
            ).optional().map_err(|e| e.to_string())? {
                if existing != encoded {
                    return Err("Mirror recommendation id collision with different evidence".into());
                }
                continue;
            }
            if count + inserted >= 10_000 {
                return Err("Mirror register at capacity".into());
            }
            tx.execute(
                "INSERT INTO mirror_findings(id,tenant,run_id,code,body,created_at)
                 VALUES(?1,?2,?3,?4,?5,?6)",
                params![id,tenant,run_id,code,encoded,now()]
            ).map_err(|e| e.to_string())?;
            inserted += 1;
        }
        tx.commit().map_err(|e| e.to_string())?;
        Ok(inserted)
    }

    pub fn list(&self, tenant: &str, limit: u32) -> Result<Value, String> {
        let mut stmt = self.conn.prepare(
            "SELECT body FROM mirror_findings WHERE tenant=?1
             ORDER BY created_at DESC, id DESC LIMIT ?2"
        ).map_err(|e| e.to_string())?;
        let values = stmt.query_map(params![tenant, limit.min(200)], |r| {
            r.get::<_,String>(0)
        }).map_err(|e| e.to_string())?
            .map(|r| {
                let raw = r.map_err(|e| e.to_string())?;
                serde_json::from_str::<Value>(&raw).map_err(|e| e.to_string())
            })
            .collect::<Result<Vec<_>,_>>()?;
        Ok(json!({"status":"proposed_only", "recommendations":values}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn register_deduplicates_and_scopes_to_tenant() {
        let path = std::env::temp_dir().join(format!(
            "jani-mirror-test-{}-{}",std::process::id(),std::time::SystemTime::now()
                .duration_since(UNIX_EPOCH).unwrap().as_nanos()));
        let mut store = MirrorRegister::open(&path).unwrap();
        let proposal = json!({
            "id":format!("mir-{}", "a".repeat(24)), "runId":"run-1",
            "code":"failed_run", "status":"proposed", "evidence":["evt-1"]
        });
        assert_eq!(store.record("tenant-a",&[proposal.clone()]).unwrap(),1);
        assert_eq!(store.record("tenant-a",&[proposal.clone()]).unwrap(),0);
        assert_eq!(store.list("tenant-a",100).unwrap()["recommendations"].as_array().unwrap().len(),1);
        assert_eq!(store.list("tenant-b",100).unwrap()["recommendations"].as_array().unwrap().len(),0);
        assert!(store.record("tenant-a",&[json!({"id":proposal["id"],"runId":"run-2","code":"failed_run","status":"proposed"})]).is_err());
        drop(store);
        fs::remove_dir_all(path).unwrap();
    }
}
