//! Tenant-scoped, transactional native metadata persistence. Never stores source bodies.
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};

pub struct State {
    conn: Connection,
    pub root: PathBuf,
    pub state_dir: PathBuf,
    pub tenant: String,
    pub task: String,
}

fn valid_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 100
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

impl State {
    pub fn open(root: &Path, state_dir: &Path, tenant: &str, task: &str) -> Result<Self, String> {
        if !valid_id(tenant) || !valid_id(task) {
            return Err("invalid native tenant/task id".into());
        }
        let root = root.canonicalize().map_err(|e| e.to_string())?;
        if !root.is_dir() {
            return Err("native root is not a directory".into());
        }
        let directory = state_dir.join("native");
        for path in [state_dir, directory.as_path()] {
            if let Ok(meta) = fs::symlink_metadata(path) {
                if meta.file_type().is_symlink() {
                    return Err("symlinked native state refused".into());
                }
            }
        }
        fs::create_dir_all(&directory).map_err(|e| e.to_string())?;
        let file = directory.join("native.sqlite3");
        if let Ok(meta) = fs::symlink_metadata(&file) {
            if meta.file_type().is_symlink() {
                return Err("symlinked native database refused".into());
            }
        }
        let conn = Connection::open(&file).map_err(|e| e.to_string())?;
        conn.busy_timeout(std::time::Duration::from_millis(3000))
            .map_err(|e| e.to_string())?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=FULL;
             PRAGMA foreign_keys=ON;
             CREATE TABLE IF NOT EXISTS objects(
               tenant TEXT NOT NULL, task TEXT NOT NULL,
               kind TEXT NOT NULL, object_id TEXT NOT NULL,
               body TEXT NOT NULL, revision INTEGER NOT NULL DEFAULT 1,
               PRIMARY KEY(tenant,task,kind,object_id)
             );
             CREATE INDEX IF NOT EXISTS objects_by_kind
               ON objects(tenant,task,kind);",
        )
        .map_err(|e| e.to_string())?;
        Ok(Self {
            conn,
            root,
            state_dir: state_dir.to_path_buf(),
            tenant: tenant.into(),
            task: task.into(),
        })
    }
    pub fn get(&self, kind: &str, id: &str) -> Result<Option<Value>, String> {
        let v = self
            .conn
            .query_row(
                "SELECT body FROM objects WHERE tenant=?1 AND task=?2 AND kind=?3 AND object_id=?4",
                params![self.tenant, self.task, kind, id],
                |r| r.get::<_, String>(0),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        v.map(|raw| serde_json::from_str(&raw).map_err(|e| e.to_string()))
            .transpose()
    }
    pub fn set(&mut self, kind: &str, id: &str, value: &Value) -> Result<(), String> {
        if !valid_id(kind) || !valid_id(id) {
            return Err("invalid native object name".into());
        }
        let text = serde_json::to_string(value).map_err(|e| e.to_string())?;
        if text.len() > 131_072 {
            return Err("native metadata entry too large".into());
        }
        self.conn
            .execute(
                "INSERT INTO objects(tenant,task,kind,object_id,body) VALUES(?1,?2,?3,?4,?5)
             ON CONFLICT(tenant,task,kind,object_id)
             DO UPDATE SET body=excluded.body,revision=objects.revision+1",
                params![self.tenant, self.task, kind, id, text],
            )
            .map_err(|e| e.to_string())?;
        Ok(())
    }
    pub fn delete(&mut self, kind: &str, id: &str) -> Result<usize, String> {
        self.conn
            .execute(
                "DELETE FROM objects WHERE tenant=?1 AND task=?2 AND kind=?3 AND object_id=?4",
                params![self.tenant, self.task, kind, id],
            )
            .map_err(|e| e.to_string())
    }
    pub fn list(&self, kind: &str, limit: usize) -> Result<Vec<(String, Value)>, String> {
        let mut stmt = self.conn.prepare(
            "SELECT object_id,body FROM objects WHERE tenant=?1 AND task=?2 AND kind=?3 ORDER BY object_id LIMIT ?4"
        ).map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(
                params![self.tenant, self.task, kind, limit.min(50_000) as i64],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
            )
            .map_err(|e| e.to_string())?;
        let mut result = Vec::new();
        for row in rows {
            let (id, raw) = row.map_err(|e| e.to_string())?;
            result.push((id, serde_json::from_str(&raw).map_err(|e| e.to_string())?));
        }
        Ok(result)
    }
    pub fn count(&self, kind: &str) -> Result<usize, String> {
        self.conn
            .query_row(
                "SELECT count(*) FROM objects WHERE tenant=?1 AND task=?2 AND kind=?3",
                params![self.tenant, self.task, kind],
                |r| r.get::<_, i64>(0),
            )
            .map(|x| x as usize)
            .map_err(|e| e.to_string())
    }
}
