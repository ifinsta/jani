//! Transactional query projections for accepted JSMT telemetry.
//! The raw envelope and all normalized observations commit in one SQLite transaction.
use jani_jsmt::Envelope;
use rusqlite::{params, Connection, Transaction};
use std::time::{SystemTime, UNIX_EPOCH};

pub fn epoch() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .min(i64::MAX as u64) as i64
}

pub fn schema(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch("
        CREATE TABLE IF NOT EXISTS metric_samples (
            tenant TEXT NOT NULL, device TEXT NOT NULL, sequence INTEGER NOT NULL,
            metric_index INTEGER NOT NULL, name TEXT NOT NULL, unit TEXT NOT NULL,
            value REAL NOT NULL, attributes TEXT NOT NULL, received_unix INTEGER NOT NULL,
            PRIMARY KEY(tenant,device,sequence,metric_index)
        );
        CREATE INDEX IF NOT EXISTS ix_metrics_subject ON metric_samples(tenant,device,name,received_unix);
        CREATE TABLE IF NOT EXISTS service_logs (
            tenant TEXT NOT NULL, device TEXT NOT NULL, sequence INTEGER NOT NULL,
            log_index INTEGER NOT NULL, service TEXT NOT NULL, severity TEXT NOT NULL,
            body TEXT NOT NULL, time_unix_nano INTEGER NOT NULL, attributes TEXT NOT NULL,
            received_unix INTEGER NOT NULL,
            PRIMARY KEY(tenant,device,sequence,log_index)
        );
        CREATE INDEX IF NOT EXISTS ix_logs_service ON service_logs(tenant,device,service,received_unix);
        CREATE TABLE IF NOT EXISTS coverage_latest (
            tenant TEXT NOT NULL, device TEXT NOT NULL, service TEXT NOT NULL, source TEXT NOT NULL,
            status TEXT NOT NULL, reason TEXT NOT NULL, sequence INTEGER NOT NULL,
            last_reported_unix INTEGER NOT NULL,
            PRIMARY KEY(tenant,device,service,source)
        );
        CREATE TABLE IF NOT EXISTS incidents (
            tenant TEXT NOT NULL, device TEXT NOT NULL, service TEXT NOT NULL, source TEXT NOT NULL,
            status TEXT NOT NULL CHECK(status IN ('open','resolved')),
            reason TEXT NOT NULL, opened_unix INTEGER NOT NULL,
            last_changed_unix INTEGER NOT NULL, last_sequence INTEGER NOT NULL,
            PRIMARY KEY(tenant,device,service,source)
        );
        CREATE TABLE IF NOT EXISTS incident_outbox (
            id INTEGER PRIMARY KEY AUTOINCREMENT, tenant TEXT NOT NULL,
            device TEXT NOT NULL, service TEXT NOT NULL, source TEXT NOT NULL,
            event TEXT NOT NULL, reason TEXT NOT NULL,
            sequence INTEGER NOT NULL, created_unix INTEGER NOT NULL
        );
        CREATE TABLE IF NOT EXISTS rate_limits (
            tenant TEXT NOT NULL, device TEXT NOT NULL, minute INTEGER NOT NULL,
            count INTEGER NOT NULL, PRIMARY KEY(tenant,device,minute)
        );")
}

pub fn project(tx: &Transaction<'_>, data: &Envelope) -> rusqlite::Result<()> {
    let now = epoch();
    for (i, m) in data.metrics.iter().enumerate() {
        tx.execute(
            "INSERT INTO metric_samples VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            params![
                data.tenant_id,
                data.device_id,
                data.sequence,
                i as i64,
                m.name,
                m.unit,
                m.value,
                serde_json::to_string(&m.attributes).unwrap_or_default(),
                now
            ],
        )?;
    }
    for (i, log) in data.logs.iter().enumerate() {
        let service = log
            .resource
            .get("service.name")
            .map(String::as_str)
            .unwrap_or("unknown");
        tx.execute(
            "INSERT INTO service_logs VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
            params![
                data.tenant_id,
                data.device_id,
                data.sequence,
                i as i64,
                service,
                log.severity_text,
                log.body,
                log.time_unix_nano,
                serde_json::to_string(&log.attributes).unwrap_or_default(),
                now
            ],
        )?;
    }
    for item in &data.coverage {
        tx.execute(
            "INSERT INTO coverage_latest VALUES(?1,?2,?3,?4,?5,?6,?7,?8)
            ON CONFLICT(tenant,device,service,source) DO UPDATE SET
            status=excluded.status,reason=excluded.reason,sequence=excluded.sequence,
            last_reported_unix=excluded.last_reported_unix",
            params![
                data.tenant_id,
                data.device_id,
                item.service,
                item.source,
                item.status,
                item.reason,
                data.sequence,
                now
            ],
        )?;
        // Only configured collectors may generate incidents, not every yet-unsupported
        // systemd unit. All coverage gaps remain queryable in coverage_latest.
        if !(item.source.starts_with("file:") || item.source.starts_with("journal:")) {
            continue;
        }
        let existing: Option<String>=tx.query_row(
            "SELECT status FROM incidents WHERE tenant=?1 AND device=?2 AND service=?3 AND source=?4",
            params![data.tenant_id,data.device_id,item.service,item.source],|r|r.get(0),
        ).optional()?;
        let next = if item.status == "covered" {
            "resolved"
        } else {
            "open"
        };
        match existing.as_deref() {
            Some(previous) if previous == next => {}
            Some(_) => {
                tx.execute(
                    "UPDATE incidents SET status=?5, reason=?6, last_changed_unix=?7,
                    last_sequence=?8 WHERE tenant=?1 AND device=?2 AND service=?3 AND source=?4",
                    params![
                        data.tenant_id,
                        data.device_id,
                        item.service,
                        item.source,
                        next,
                        item.reason,
                        now,
                        data.sequence
                    ],
                )?;
                emit(tx, data, item, next, now)?;
            }
            None if next == "open" => {
                tx.execute(
                    "INSERT INTO incidents VALUES(?1,?2,?3,?4,'open',?5,?6,?6,?7)",
                    params![
                        data.tenant_id,
                        data.device_id,
                        item.service,
                        item.source,
                        item.reason,
                        now,
                        data.sequence
                    ],
                )?;
                emit(tx, data, item, "open", now)?;
            }
            None => {}
        }
    }
    Ok(())
}
use rusqlite::OptionalExtension;
fn emit(
    tx: &Transaction<'_>,
    data: &Envelope,
    item: &jani_jsmt::Coverage,
    event: &str,
    now: i64,
) -> rusqlite::Result<()> {
    tx.execute("INSERT INTO incident_outbox(tenant,device,service,source,event,reason,sequence,created_unix)
        VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",params![
        data.tenant_id,data.device_id,item.service,item.source,event,item.reason,data.sequence,now
    ])?;
    Ok(())
}
pub fn rate_limit(
    tx: &Transaction<'_>,
    tenant: &str,
    device: &str,
    per_min: i64,
) -> rusqlite::Result<bool> {
    let minute = epoch() / 60;
    let count: i64 = tx
        .query_row(
            "SELECT count FROM rate_limits WHERE tenant=?1 AND device=?2 AND minute=?3",
            params![tenant, device, minute],
            |r| r.get(0),
        )
        .optional()?
        .unwrap_or(0);
    if count >= per_min {
        return Ok(false);
    }
    tx.execute(
        "INSERT INTO rate_limits VALUES(?1,?2,?3,1)
        ON CONFLICT(tenant,device,minute) DO UPDATE SET count=count+1",
        params![tenant, device, minute],
    )?;
    Ok(true)
}
pub fn purge(conn: &Connection, days: i64) -> rusqlite::Result<()> {
    let before = epoch().saturating_sub(days.saturating_mul(86_400));
    conn.execute(
        "DELETE FROM metric_samples WHERE received_unix < ?1",
        params![before],
    )?;
    conn.execute(
        "DELETE FROM service_logs WHERE received_unix < ?1",
        params![before],
    )?;
    conn.execute(
        "DELETE FROM incident_outbox WHERE created_unix < ?1",
        params![before],
    )?;
    conn.execute(
        "DELETE FROM accepted WHERE received_unix < ?1",
        params![before],
    )?;
    conn.execute(
        "DELETE FROM rate_limits WHERE minute < ?1",
        params![epoch() / 60 - 2],
    )?;
    // Device cursors and incident state persist to preserve replay protection and
    // active incident continuity; removed raw payloads cannot be ACKed as duplicates.
    // Filesystem snapshots, SQLite WAL files and external backups need separate
    // bounded retention and secure-deletion policies before customer deployment.
    Ok(())
}
pub fn inspect(
    db: &str,
    tenant: &str,
) -> Result<serde_json::Value, Box<dyn std::error::Error + Send + Sync>> {
    if tenant.is_empty() || !jani_jsmt::identity_ok(tenant) {
        return Err("invalid tenant".into());
    }
    let conn = Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM accepted WHERE tenant=?1",
        params![tenant],
        |r| r.get(0),
    )?;
    let metrics: i64 = conn.query_row(
        "SELECT COUNT(*) FROM metric_samples WHERE tenant=?1",
        params![tenant],
        |r| r.get(0),
    )?;
    let logs: i64 = conn.query_row(
        "SELECT COUNT(*) FROM service_logs WHERE tenant=?1",
        params![tenant],
        |r| r.get(0),
    )?;
    let gaps: i64 = conn.query_row(
        "SELECT COUNT(*) FROM coverage_latest WHERE tenant=?1 AND status!='covered'",
        params![tenant],
        |r| r.get(0),
    )?;
    let open: i64 = conn.query_row(
        "SELECT COUNT(*) FROM incidents WHERE tenant=?1 AND status='open'",
        params![tenant],
        |r| r.get(0),
    )?;
    Ok(
        serde_json::json!({"tenant":tenant,"accepted_envelopes":count,"metric_samples":metrics,
        "log_events":logs,"uncovered_sources":gaps,"open_incidents":open}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use jani_jsmt::{Coverage, Envelope, Log, Metric};
    use std::collections::BTreeMap;

    #[test]
    fn projection_and_incident_lifecycle_are_transactional() {
        let mut c = Connection::open_in_memory().unwrap();
        schema(&c).unwrap();
        let mut env = Envelope {
            version: jani_jsmt::VERSION.into(),
            tenant_id: "tenant".into(),
            device_id: "node".into(),
            sequence: 1,
            observed_at: "2026-10-25T12:00:00Z".into(),
            metrics: vec![Metric {
                name: "system.uptime".into(),
                unit: "s".into(),
                value: 42.0,
                attributes: BTreeMap::new(),
            }],
            logs: vec![Log {
                time_unix_nano: 1,
                severity_text: "WARN".into(),
                body: "test failure".into(),
                resource: BTreeMap::from([("service.name".into(), "api".into())]),
                attributes: BTreeMap::new(),
            }],
            coverage: vec![Coverage {
                service: "api".into(),
                source: "file:/var/log/api.log".into(),
                status: "missing".into(),
                reason: "not found".into(),
            }],
        };
        {
            let tx = c.transaction().unwrap();
            project(&tx, &env).unwrap();
            tx.commit().unwrap();
        }
        let count: i64 = c
            .query_row("SELECT COUNT(*) FROM service_logs", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);
        let count: i64 = c
            .query_row("SELECT COUNT(*) FROM metric_samples", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);
        let open: String = c
            .query_row("SELECT status FROM incidents", [], |r| r.get(0))
            .unwrap();
        assert_eq!(open, "open");
        env.sequence = 2;
        env.coverage[0].status = "covered".into();
        env.coverage[0].reason.clear();
        {
            let tx = c.transaction().unwrap();
            project(&tx, &env).unwrap();
            tx.commit().unwrap();
        }
        let state: String = c
            .query_row("SELECT status FROM incidents", [], |r| r.get(0))
            .unwrap();
        assert_eq!(state, "resolved");
        let n: i64 = c
            .query_row("SELECT COUNT(*) FROM incident_outbox", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 2);
    }
    #[test]
    fn rate_limit_is_per_device_and_transactional() {
        let mut c = Connection::open_in_memory().unwrap();
        schema(&c).unwrap();
        let tx = c.transaction().unwrap();
        assert!(rate_limit(&tx, "tenant", "device", 1).unwrap());
        assert!(!rate_limit(&tx, "tenant", "device", 1).unwrap());
        assert!(rate_limit(&tx, "tenant", "other", 1).unwrap());
        tx.commit().unwrap();
    }
}
