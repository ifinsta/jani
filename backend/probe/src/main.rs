//! Jani Probe: read-only explicitly enrolled client, bounded metrics/log source collection,
//! durable local queue and validated TLS1.3/mTLS JSMT/1 delivery.
//! Intentionally NO command, policy-update, self-update or shell execution interface.
#[cfg(target_os = "linux")]
mod journal;
use jani_jsmt::{
    create_envelope, identity_ok, Ack, Coverage, Log, Metric, MAX_COVERAGE, MAX_LOGS,
    MAX_LOG_CHARS, VERSION,
};
use regex::Regex;
use reqwest::blocking::Client;
use reqwest::{Certificate, Identity, Url};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::Deserialize;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
#[cfg(not(unix))]
use std::fs::File;
#[cfg(unix)]
use std::fs::OpenOptions;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::OnceLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
const MAX_QUEUE: i64 = 1000;
const MAX_SPOOL_BYTES: i64 = 64 * 1024 * 1024;
const MAX_LINES: usize = MAX_LOGS;
const MAX_CAPTURE_BYTES: usize = 128 * 1024;
const MAX_SERVICES: usize = 240;
const FLUSH_BATCH: i64 = 32;
const HTTP_TIMEOUT: Duration = Duration::from_secs(10);
const WATCH_INTERVAL: Duration = Duration::from_secs(30);
#[cfg(target_os = "linux")]
const SUBPROCESS_DEADLINE: Duration = Duration::from_secs(3);
#[cfg(target_os = "linux")]
const SUBPROCESS_POLL: Duration = Duration::from_millis(25);

/// First existing regular file among candidates (absolute tool paths only).
#[cfg(target_os = "linux")]
fn which(candidates: &[&'static str]) -> Option<std::path::PathBuf> {
    candidates
        .iter()
        .map(Path::new)
        .find(|path| path.is_file())
        .map(|path| path.to_path_buf())
}

/// Spawn a subprocess and capture stdout with a hard deadline and a byte cap.
/// Kills on timeout; stderr and stdin are closed to the child.
#[cfg(target_os = "linux")]
fn run_captured(
    bin: &Path,
    args: &[&str],
    deadline: Duration,
    cap: usize,
) -> std::io::Result<Vec<u8>> {
    use std::process::{Command, Stdio};
    use std::time::Instant;
    let mut child = Command::new(bin)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let Some(pipe) = child.stdout.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return Err(std::io::Error::other("no output pipe"));
    };
    let reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let read = pipe.take(cap as u64 + 1).read_to_end(&mut bytes);
        (read, bytes)
    });
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break Some(status);
        }
        if started.elapsed() >= deadline {
            break None;
        }
        std::thread::sleep(SUBPROCESS_POLL);
    };
    if status.is_none() {
        let _ = child.kill();
        let _ = child.wait();
    }
    let (read, bytes) = reader
        .join()
        .map_err(|_| std::io::Error::other("output reader panicked"))?;
    read?;
    if !status.is_some_and(|s| s.success()) || bytes.len() > cap {
        return Err(std::io::Error::other(
            "command timed out, failed or produced oversized output",
        ));
    }
    Ok(bytes)
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Source {
    service: String,
    path: String,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceFile {
    services: Vec<Source>,
}
#[derive(Default)]
struct Observations {
    metrics: Vec<Metric>,
    logs: Vec<Log>,
    coverage: Vec<Coverage>,
    offsets: Vec<(String, i64, String)>,
    journal_offsets: Vec<(String, String)>,
}
fn now_nanos() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
        .min(i64::MAX as u128) as i64
}
fn observed_at() -> String {
    // For full timestamp semantic compatibility, use UTC in a future protocol revision.
    // v1 validator constrains length and identity only.
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    let days = seconds / 86400;
    let secs = seconds % 86400;
    let z = days + 719468;
    let era = z / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = mp + if mp < 10 { 3 } else { -9 };
    let year = y + if m <= 2 { 1 } else { 0 };
    format!(
        "{year:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        secs / 3600,
        (secs % 3600) / 60,
        secs % 60
    )
}
fn metric(name: &str, value: f64, attrs: BTreeMap<String, String>) -> Metric {
    Metric {
        name: name.into(),
        unit: jani_jsmt::metric_unit(name).unwrap_or("1").into(),
        value,
        attributes: attrs,
    }
}
#[cfg_attr(not(target_os = "linux"), allow(unused_variables))]
fn linux_metrics(out: &mut Observations) {
    #[cfg(target_os = "linux")]
    {
        if let Ok(raw) = fs::read_to_string("/proc/uptime") {
            if let Some(n) = raw
                .split_whitespace()
                .next()
                .and_then(|n| n.parse::<f64>().ok())
            {
                out.metrics
                    .push(metric("system.uptime", n, BTreeMap::new()));
            }
        }
        if let Ok(raw) = fs::read_to_string("/proc/meminfo") {
            let mut values = HashMap::new();
            for line in raw.lines() {
                if let Some((key, rest)) = line.split_once(':') {
                    if let Some(value) = rest
                        .split_whitespace()
                        .next()
                        .and_then(|v| v.parse::<u64>().ok())
                    {
                        values.insert(key.to_string(), value * 1024);
                    }
                }
            }
            if let (Some(total), Some(available)) =
                (values.get("MemTotal"), values.get("MemAvailable"))
            {
                let used = total.saturating_sub(*available);
                out.metrics.push(metric(
                    "system.memory.usage",
                    used as f64,
                    BTreeMap::from([("system.memory.state".into(), "used".into())]),
                ));
                out.metrics.push(metric(
                    "system.memory.usage",
                    *available as f64,
                    BTreeMap::from([("system.memory.state".into(), "free".into())]),
                ));
            }
        }
        if let Ok(raw) = fs::read_to_string("/proc/stat") {
            if let Some(cpu_line) = raw.lines().next() {
                let mut parts = cpu_line.split_whitespace();
                if parts.next() == Some("cpu") {
                    let values = parts
                        .take(8)
                        .map(|v| v.parse::<f64>())
                        .collect::<std::result::Result<Vec<_>, _>>();
                    if let Ok(v) = values {
                        if v.len() >= 7 {
                            let ticks = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
                            if ticks > 0 {
                                let hz = ticks as f64;
                                for (mode, time) in [
                                    ("user", (v[0] + v[1]) / hz),
                                    ("system", (v[2] + v[5] + v[6]) / hz),
                                    ("idle", (v[3] + v[4]) / hz),
                                ] {
                                    out.metrics.push(metric(
                                        "system.cpu.time",
                                        time,
                                        BTreeMap::from([("cpu.mode".into(), mode.into())]),
                                    ));
                                }
                            }
                        }
                    }
                }
            }
        }
        {
            use std::ffi::CString;
            use std::mem::MaybeUninit;
            let path = CString::new("/").expect("static mount");
            let mut stat = MaybeUninit::<libc::statvfs>::uninit();
            if unsafe { libc::statvfs(path.as_ptr(), stat.as_mut_ptr()) } == 0 {
                let st = unsafe { stat.assume_init() };
                let bytes = st.f_frsize as u128;
                let used = (st.f_blocks as u128)
                    .saturating_sub(st.f_bfree as u128)
                    .saturating_mul(bytes)
                    .min(u64::MAX as u128) as u64;
                out.metrics.push(metric(
                    "jani.filesystem.used",
                    used as f64,
                    BTreeMap::from([("filesystem.mountpoint".into(), "/".into())]),
                ));
            }
        }
        if let Ok(iter) = fs::read_dir("/proc") {
            let count = iter
                .flatten()
                .filter(|item| {
                    item.file_name().to_str().is_some_and(|name| {
                        !name.is_empty() && name.bytes().all(|b| b.is_ascii_digit())
                    })
                })
                .count();
            out.metrics.push(metric(
                "system.process.count",
                count as f64,
                BTreeMap::new(),
            ));
        }
    }
}
fn scrub_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(&format!(
            r#"(?i)(authorization\s*[:=]\s*bearer\s+|["']?(?:{})["']?\s*[:=]\s*)(?:"(?:\\[\s\S]|[^"\\])*(?:"|$)|'(?:\\[\s\S]|[^'\\])*(?:'|$)|[^\s,;\]\}}]*)"#,
            jani_jsmt::SECRET_KEYS.join("|")
        ))
        .expect("secret-key regex")
    })
}
fn scrub(line: &str) -> String {
    // Minimal recognized-secret redaction, not sufficient to certify comprehensive DLP.
    let redacted = scrub_regex().replace_all(line, |caps: &regex::Captures<'_>| {
        format!("{}[REDACTED]", &caps[1])
    });
    let mut end = 0;
    for (chars, (idx, ch)) in redacted.char_indices().enumerate() {
        if chars >= 4096 {
            break;
        }
        end = idx + ch.len_utf8();
    }
    if end < redacted.len() {
        let marker = "[REDACTED]";
        let mut safe_end = end;
        for offset in (end.saturating_sub(marker.len() - 1)..end).rev() {
            if redacted.is_char_boundary(offset) && redacted[offset..].starts_with(marker) {
                safe_end = offset;
                break;
            }
        }
        return redacted[..safe_end].to_string();
    }
    redacted.into_owned()
}
fn coverage(service: &str, source: &str, status: &str, reason: &str) -> Coverage {
    Coverage {
        service: service.into(),
        source: source.into(),
        status: status.into(),
        reason: reason.chars().take(160).collect(),
    }
}
fn collect(
    sources: &[Source],
    cursors: &HashMap<String, (i64, String)>,
    journal_cursors: &HashMap<String, String>,
) -> Result<Observations> {
    let _ = journal_cursors;
    let mut out = Observations::default();
    let mut unique_paths = HashSet::new();
    for src in sources {
        if src.service.is_empty()
            || src.service.len() > 100
            || !src
                .service
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b'@'))
        {
            return Err("invalid source service ID".into());
        }
        #[cfg(target_os = "linux")]
        if let Some(unit) = src.path.strip_prefix("journal:") {
            if !unique_paths.insert(src.path.as_str()) {
                return Err("duplicate journal unit log source".into());
            }
            let capture = journal::capture(
                &src.service,
                unit,
                journal_cursors.get(&src.path).map(String::as_str),
                MAX_LINES.saturating_sub(out.logs.len()),
            )
            .map_err(|e| format!("journal: {e}"))?;
            out.metrics.push(metric(
                "jani.service.log.covered",
                if capture.coverage.status == "covered" {
                    1.0
                } else {
                    0.0
                },
                BTreeMap::from([("service.name".into(), src.service.clone())]),
            ));
            out.coverage.push(capture.coverage);
            out.logs.extend(capture.logs);
            if let Some(cursor) = capture.cursor {
                out.journal_offsets.push((src.path.clone(), cursor));
            }
            continue;
        }
        if src.path.is_empty() || src.path.len() > 140 || !Path::new(&src.path).is_absolute() {
            return Err("source paths must be absolute and <=140 bytes".into());
        }
        if !unique_paths.insert(src.path.as_str()) {
            return Err(
                "duplicated log source path; multiple service owners would duplicate readings"
                    .into(),
            );
        }
        let source = format!("file:{}", src.path);
        if source.len() > 160 {
            return Err("source identifier too long".into());
        }
        let (previous, previous_identity) = cursors
            .get(&source)
            .map(|(n, id)| (*n, id.as_str()))
            .unwrap_or((0, ""));
        let metadata = match fs::symlink_metadata(&src.path) {
            Ok(m) => m,
            Err(e) => {
                let status = if e.kind() == std::io::ErrorKind::PermissionDenied {
                    "permission_denied"
                } else {
                    "missing"
                };
                out.coverage.push(coverage(
                    &src.service,
                    &source,
                    status,
                    "source inaccessible",
                ));
                out.metrics.push(metric(
                    "jani.service.log.covered",
                    0.0,
                    BTreeMap::from([("service.name".into(), src.service.clone())]),
                ));
                continue;
            }
        };
        if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
            out.coverage.push(coverage(
                &src.service,
                &source,
                "unsupported",
                "not a regular file or symlink",
            ));
            out.metrics.push(metric(
                "jani.service.log.covered",
                0.0,
                BTreeMap::from([("service.name".into(), src.service.clone())]),
            ));
            continue;
        }
        #[cfg(unix)]
        let open_attempt = {
            use std::os::unix::fs::OpenOptionsExt;
            OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW)
                .open(&src.path)
        };
        #[cfg(not(unix))]
        let open_attempt = File::open(&src.path);
        let mut file = match open_attempt {
            Ok(f) => f,
            Err(_) => {
                out.coverage.push(coverage(
                    &src.service,
                    &source,
                    "permission_denied",
                    "file cannot be opened",
                ));
                out.metrics.push(metric(
                    "jani.service.log.covered",
                    0.0,
                    BTreeMap::from([("service.name".into(), src.service.clone())]),
                ));
                continue;
            }
        };
        #[cfg(unix)]
        let identity = {
            use std::os::unix::fs::MetadataExt;
            format!("{}:{}", metadata.dev(), metadata.ino())
        };
        #[cfg(not(unix))]
        let identity = String::new(); // Rotation-safe native Windows adapter is not implemented.
        let replaced = !previous_identity.is_empty() && previous_identity != identity;
        let rotated_or_truncated = replaced || metadata.len() < previous.max(0) as u64;
        let begin = if rotated_or_truncated {
            0
        } else {
            previous.max(0) as u64
        };
        file.seek(SeekFrom::Start(begin))?;
        // Bounded read. Cursor advances only through complete lines actually delivered.
        let mut chunk = Vec::new();
        file.take(MAX_CAPTURE_BYTES as u64)
            .read_to_end(&mut chunk)?;
        let bytes_read = chunk.len();
        let finished = chunk
            .iter()
            .rposition(|b| *b == b'\n')
            .map(|p| p + 1)
            .unwrap_or(0);
        let mut consumed = 0;
        let mut truncated = false;
        let mut invalid_utf8 = false;
        let remaining = MAX_LINES.saturating_sub(out.logs.len());
        if finished == 0 && bytes_read == MAX_CAPTURE_BYTES && remaining > 0 {
            invalid_utf8 |= std::str::from_utf8(&chunk).is_err();
            let line = String::from_utf8_lossy(&chunk);
            truncated = true;
            let mut attrs = BTreeMap::new();
            attrs.insert("jani.log.source".into(), source.chars().take(100).collect());
            out.logs.push(Log {
                time_unix_nano: now_nanos(),
                severity_text: "INFO".into(),
                body: scrub(line.trim_end_matches('\r')),
                resource: BTreeMap::from([("service.name".into(), src.service.clone())]),
                attributes: attrs,
            });
            consumed = bytes_read;
        } else {
            for raw_line in chunk[..finished]
                .split_inclusive(|b| *b == b'\n')
                .take(remaining)
            {
                invalid_utf8 |= std::str::from_utf8(raw_line).is_err();
                let line = String::from_utf8_lossy(&raw_line[..raw_line.len() - 1]);
                truncated |= line.chars().count() > MAX_LOG_CHARS;
                let mut attrs = BTreeMap::new();
                attrs.insert("jani.log.source".into(), source.chars().take(100).collect());
                out.logs.push(Log {
                    time_unix_nano: now_nanos(),
                    severity_text: "INFO".into(),
                    body: scrub(line.trim_end_matches('\r')),
                    resource: BTreeMap::from([("service.name".into(), src.service.clone())]),
                    attributes: attrs,
                });
                consumed += raw_line.len();
            }
        }
        let partial = rotated_or_truncated
            || (bytes_read == MAX_CAPTURE_BYTES && (begin + bytes_read as u64) < metadata.len())
            || truncated
            || invalid_utf8
            || consumed < bytes_read;
        out.coverage.push(coverage(
            &src.service,
            &source,
            if partial { "partial" } else { "covered" },
            if rotated_or_truncated {
                "source truncated; earlier log loss possible"
            } else if truncated {
                "log line exceeded 4096 chars; truncated"
            } else if invalid_utf8 {
                "non-UTF8 input decoded with replacement"
            } else if partial {
                "backlog or incomplete line remains"
            } else {
                ""
            },
        ));
        out.metrics.push(metric(
            "jani.service.log.covered",
            if partial { 0.0 } else { 1.0 },
            BTreeMap::from([("service.name".into(), src.service.clone())]),
        ));
        out.offsets
            .push((source, (begin + consumed as u64) as i64, identity));
    }
    linux_metrics(&mut out);
    Ok(out)
}
#[cfg(target_os = "linux")]
fn safe_systemd_inventory() -> std::io::Result<String> {
    let bin = which(&["/usr/bin/systemctl", "/bin/systemctl"])
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "systemctl missing"))?;
    let bytes = run_captured(
        &bin,
        &[
            "list-units",
            "--type=service",
            "--all",
            "--no-legend",
            "--no-pager",
            "--plain",
        ],
        SUBPROCESS_DEADLINE,
        MAX_CAPTURE_BYTES,
    )?;
    String::from_utf8(bytes).map_err(|_| std::io::Error::other("systemd inventory invalid UTF8"))
}
#[cfg(target_os = "linux")]
fn discover_gap(out: &mut Observations, sources: &[Source]) {
    // systemctl is only a bounded source of service INVENTORY, not of service logs.
    let stdout = match safe_systemd_inventory() {
        Ok(out) => out,
        Err(_) => {
            out.coverage.push(coverage(
                "host-discovery",
                "systemd",
                "unsupported",
                "systemd inventory unavailable, failed or timed out",
            ));
            return;
        }
    };
    let names: Vec<&str> = stdout.lines().collect();
    let max_available = MAX_COVERAGE.saturating_sub(out.coverage.len() + 1);
    if names.len() > max_available {
        out.coverage.push(coverage(
            "host-discovery",
            "systemd",
            "partial",
            "service inventory exceeds envelope limit",
        ));
    }
    for line in names.into_iter().take(max_available) {
        if out.coverage.len() >= MAX_COVERAGE {
            break;
        }
        let name = line
            .split_whitespace()
            .next()
            .unwrap_or("")
            .trim_end_matches(".service");
        if name.is_empty() || sources.iter().any(|s| s.service == name) {
            continue;
        }
        out.coverage.push(coverage(
            name,
            "systemd",
            "unsupported",
            "service log adapter not configured",
        ));
    }
}
#[cfg(not(target_os = "linux"))]
fn discover_gap(out: &mut Observations, _sources: &[Source]) {
    if out.coverage.len() < MAX_COVERAGE {
        out.coverage.push(coverage(
            "host-discovery",
            "os-adapter",
            "unsupported",
            "automatic OS service discovery not implemented",
        ));
    }
}
fn db(path: &str) -> Result<Connection> {
    if let Some(parent) = Path::new(path).parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }
    let new_file = !Path::new(path).exists();
    let conn = Connection::open(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if new_file {
            fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
        }
    }
    #[cfg(not(unix))]
    let _ = new_file;
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;
       CREATE TABLE IF NOT EXISTS pending(seq INTEGER PRIMARY KEY,payload BLOB NOT NULL,hash TEXT NOT NULL);
       CREATE TABLE IF NOT EXISTS meta(key TEXT PRIMARY KEY,value TEXT NOT NULL);
       CREATE TABLE IF NOT EXISTS offsets(source TEXT PRIMARY KEY,value INTEGER NOT NULL);
       CREATE TABLE IF NOT EXISTS source_identity(source TEXT PRIMARY KEY, fingerprint TEXT NOT NULL);
       CREATE TABLE IF NOT EXISTS journal_cursors(source TEXT PRIMARY KEY, cursor TEXT NOT NULL);")?;
    Ok(conn)
}
fn enqueue(path: &str, tenant: &str, device: &str, sources: &[Source]) -> Result<i64> {
    let mut conn = db(path)?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    for (key, value) in [("tenant_id", tenant), ("device_id", device)] {
        let previous: Option<String> = tx
            .query_row("SELECT value FROM meta WHERE key=?1", params![key], |r| {
                r.get(0)
            })
            .optional()?;
        if let Some(previous) = previous {
            if previous != value {
                return Err("spool belongs to a different tenant or device".into());
            }
        } else {
            tx.execute(
                "INSERT INTO meta(key,value) VALUES(?1,?2)",
                params![key, value],
            )?;
        }
    }
    let count: i64 = tx.query_row("SELECT COUNT(*) FROM pending", [], |r| r.get(0))?;
    if count >= MAX_QUEUE {
        return Err("spool full; refusing to lose logs".into());
    }
    let used: i64 = tx.query_row(
        "SELECT COALESCE(SUM(LENGTH(payload)),0) FROM pending",
        [],
        |r| r.get(0),
    )?;
    if used >= MAX_SPOOL_BYTES {
        return Err("64 MiB spool payload limit reached".into());
    }
    let mut cursors = HashMap::new();
    {
        let mut stmt=tx.prepare("SELECT o.source,o.value,COALESCE(i.fingerprint,'') FROM offsets o LEFT JOIN source_identity i ON o.source=i.source")?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?;
        for pair in rows {
            let (k, v, f) = pair?;
            cursors.insert(k, (v, f));
        }
    }
    let mut journal_cursors = HashMap::new();
    {
        let mut stmt = tx.prepare("SELECT source,cursor FROM journal_cursors")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        for pair in rows {
            let (source, cursor) = pair?;
            journal_cursors.insert(source, cursor);
        }
    }
    let mut o = collect(sources, &cursors, &journal_cursors)?;
    discover_gap(&mut o, sources);
    let seq: Option<i64> = tx
        .query_row("SELECT value FROM meta WHERE key='sequence'", [], |r| {
            let s: String = r.get(0)?;
            Ok(s.parse::<i64>().unwrap_or(0))
        })
        .optional()?;
    let seq = seq.unwrap_or(0) + 1;
    let (payload, hash) = create_envelope(
        tenant,
        device,
        seq,
        observed_at(),
        o.metrics,
        o.logs,
        o.coverage,
    )
    .map_err(|e| format!("invalid normalized telemetry: {e}"))?;
    if used.saturating_add(payload.len() as i64) > MAX_SPOOL_BYTES {
        return Err("spool payload quota would be exceeded; cursors unchanged".into());
    }
    tx.execute(
        "INSERT INTO pending VALUES(?1,?2,?3)",
        params![seq, payload, hash],
    )?;
    tx.execute(
        "INSERT INTO meta(key,value) VALUES('sequence',?1)
        ON CONFLICT(key) DO UPDATE SET value=excluded.value",
        params![seq.to_string()],
    )?;
    for (source, pos, fingerprint) in o.offsets {
        tx.execute(
            "INSERT INTO offsets VALUES(?1,?2)
           ON CONFLICT(source) DO UPDATE SET value=excluded.value",
            params![source, pos],
        )?;
        tx.execute(
            "INSERT INTO source_identity VALUES(?1,?2)
           ON CONFLICT(source) DO UPDATE SET fingerprint=excluded.fingerprint",
            params![source, fingerprint],
        )?;
    }
    for (source, cursor) in o.journal_offsets {
        tx.execute(
            "INSERT INTO journal_cursors VALUES(?1,?2)
           ON CONFLICT(source) DO UPDATE SET cursor=excluded.cursor",
            params![source, cursor],
        )?;
    }
    tx.commit()?;
    Ok(seq)
}
fn client(ca: &str, cert: &str, key: &str) -> Result<Client> {
    let root = Certificate::from_pem(&fs::read(ca)?)?;
    let mut identity = fs::read(cert)?;
    identity.extend_from_slice(b"\n");
    identity.extend_from_slice(&fs::read(key)?);
    let id = Identity::from_pem(&identity)?;
    Ok(Client::builder()
        .https_only(true)
        .http1_only()
        .min_tls_version(reqwest::tls::Version::TLS_1_3)
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .tls_built_in_root_certs(false)
        .add_root_certificate(root)
        .identity(id)
        .timeout(HTTP_TIMEOUT)
        .build()?)
}
fn flush(conn: &mut Connection, http: &Client, origin: &str) -> Result<usize> {
    let mut stmt = conn.prepare(&format!(
        "SELECT seq,payload,hash FROM pending ORDER BY seq LIMIT {FLUSH_BATCH}"
    ))?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, Vec<u8>>(1)?,
            r.get::<_, String>(2)?,
        ))
    })?;
    let pending = rows.collect::<rusqlite::Result<Vec<_>>>()?;
    drop(stmt);
    let mut sent = 0;
    for (seq, payload, hash) in pending {
        let response = http
            .post(format!("{origin}/v1/ingest"))
            .header("Content-Type", "application/json")
            .body(payload)
            .send()?;
        if !response.status().is_success() {
            return Err(format!("gateway refused telemetry: HTTP {}", response.status()).into());
        }
        let ack: Ack = response.json()?;
        if ack.version != VERSION || ack.ack_sequence != seq || ack.payload_sha256 != hash {
            return Err("unauthenticated or mismatched application ACK".into());
        }
        conn.execute(
            "DELETE FROM pending WHERE seq=?1 AND hash=?2",
            params![seq, hash],
        )?;
        sent += 1;
    }
    Ok(sent)
}
fn required(args: &[String], flag: &str) -> Result<String> {
    args.windows(2)
        .find(|w| w[0] == flag)
        .map(|w| w[1].clone())
        .ok_or_else(|| format!("missing {flag}").into())
}
fn run() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).is_some_and(|a| a == "status") {
        let db_path = required(&args, "--spool")?;
        let conn =
            Connection::open_with_flags(&db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let queued: i64 = conn.query_row("SELECT COUNT(*) FROM pending", [], |r| r.get(0))?;
        let max_seq: Option<i64> =
            conn.query_row("SELECT MAX(seq) FROM pending", [], |r| r.get(0))?;
        println!(
            "{}",
            serde_json::json!({"queued_envelopes":queued,"max_queued_sequence":max_seq,
            "spool_limit":MAX_QUEUE,"spool_byte_limit":MAX_SPOOL_BYTES,"delivery":"queued records require valid gateway ACK"})
        );
        return Ok(());
    }
    if args.iter().any(|a| a == "--version") {
        println!("jani-probe {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    if args.iter().any(|a| a == "--help") {
        println!("jani-probe --gateway https://host:port --tenant ID --device ID --spool state.sqlite --sources sources.json --ca CA.pem --cert device.crt --key device.key [--watch]\njani-probe status --spool state.sqlite");
        return Ok(());
    }
    let gateway = required(&args, "--gateway")?;
    let uri = Url::parse(&gateway)?;
    if uri.scheme() != "https"
        || uri.username() != ""
        || uri.password().is_some()
        || uri.path() != "/"
        || uri.query().is_some()
        || uri.fragment().is_some()
    {
        return Err("JSMT requires a clean HTTPS origin".into());
    }
    let tenant = required(&args, "--tenant")?;
    let device = required(&args, "--device")?;
    if !identity_ok(&tenant) || !identity_ok(&device) {
        return Err("invalid tenant/device".into());
    }
    let spool = required(&args, "--spool")?;
    let config: SourceFile =
        serde_json::from_str(&fs::read_to_string(required(&args, "--sources")?)?)?;
    if config.services.len() > MAX_SERVICES {
        return Err("too many explicitly configured services".into());
    }
    let http = client(
        &required(&args, "--ca")?,
        &required(&args, "--cert")?,
        &required(&args, "--key")?,
    )?;
    let watch = args.iter().any(|a| a == "--watch");
    let mut conn = db(&spool)?;
    loop {
        // Drain old durable records *before* enqueueing. Saturated offline queues
        // must recover on reconnect instead of permanently failing collection.
        if let Err(e) = flush(&mut conn, &http, gateway.trim_end_matches('/')) {
            eprintln!("jani-probe: old queued telemetry not yet delivered: {e}");
        }
        match enqueue(&spool, &tenant, &device, &config.services) {
            Ok(seq) => match flush(&mut conn, &http, gateway.trim_end_matches('/')) {
                Ok(sent) => eprintln!("jani-probe: seq={seq}, delivered={sent}"),
                Err(e) => {
                    eprintln!("jani-probe: delivery unavailable; telemetry retained: {e}");
                    if !watch {
                        return Err(e);
                    }
                }
            },
            Err(e) => {
                eprintln!("jani-probe: collection paused without dropping data: {e}");
                if !watch {
                    return Err(e);
                }
            }
        }
        if !watch {
            break;
        }
        std::thread::sleep(WATCH_INTERVAL);
    }
    Ok(())
}
fn main() {
    if let Err(e) = run() {
        eprintln!("jani-probe: {e}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scrub_preserves_key_but_redacts_value() {
        let result = scrub("login password=supersecret token:abc123 Authorization: Bearer xyz");
        assert!(!result.contains("supersecret"));
        assert!(!result.contains("abc123"));
        assert!(!result.contains(" xyz"));
        assert!(result.contains("password=[REDACTED]"));
        assert!(result.contains("token:[REDACTED]"));
    }
    #[test]
    fn log_backlog_drains_with_persisted_offsets() {
        let path = std::env::temp_dir().join(format!(
            "jani-probe-backlog-{}-{}.log",
            std::process::id(),
            now_nanos()
        ));
        let spool = path.with_extension("sqlite");
        let expected: Vec<String> = (0..MAX_LINES * 2 + 17)
            .map(|i| format!("line-{i}"))
            .collect();
        fs::write(&path, format!("{}\n", expected.join("\n"))).unwrap();
        let sources = [Source {
            service: "api".into(),
            path: path.to_str().unwrap().into(),
        }];
        let source = format!("file:{}", sources[0].path);
        let mut delivered = 0;
        for (poll, count) in [MAX_LINES, MAX_LINES, 17, 0].into_iter().enumerate() {
            let seq = enqueue(spool.to_str().unwrap(), "tenant", "device", &sources).unwrap();
            assert_eq!(seq, poll as i64 + 1);
            // Reopen the spool so every poll must use committed, durable offsets.
            let conn = Connection::open(&spool).unwrap();
            let payload: Vec<u8> = conn
                .query_row(
                    "SELECT payload FROM pending WHERE seq=?1",
                    params![seq],
                    |r| r.get(0),
                )
                .unwrap();
            let (env, _, _) = jani_jsmt::validate(&payload).unwrap();
            assert_eq!(env.logs.len(), count);
            for (log, line) in env.logs.iter().zip(&expected[delivered..delivered + count]) {
                assert_eq!(&log.body, line);
            }
            delivered += count;
            let offset: i64 = conn
                .query_row(
                    "SELECT value FROM offsets WHERE source=?1",
                    params![&source],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(
                offset,
                expected[..delivered]
                    .iter()
                    .map(|line| line.len() + 1)
                    .sum::<usize>() as i64
            );
            assert_eq!(
                env.coverage
                    .iter()
                    .find(|c| c.source == source)
                    .unwrap()
                    .status,
                if delivered < expected.len() {
                    "partial"
                } else {
                    "covered"
                }
            );
        }
        assert_eq!(delivered, expected.len());
        fs::remove_file(path).unwrap();
        fs::remove_file(spool).unwrap();
    }

    #[test]
    fn multiple_sources_share_log_batch_cap() {
        let base = std::env::temp_dir().join(format!(
            "jani-probe-shared-{}-{}",
            std::process::id(),
            now_nanos()
        ));
        let mut sources = Vec::new();
        for (i, count) in [MAX_LINES - 50, 100, 1].into_iter().enumerate() {
            let path = base.with_extension(format!("{i}.log"));
            fs::write(&path, format!("source-{i}\n").repeat(count)).unwrap();
            sources.push(Source {
                service: format!("service-{i}"),
                path: path.to_str().unwrap().into(),
            });
        }
        let first = collect(&sources, &HashMap::new(), &HashMap::new()).unwrap();
        assert_eq!(first.logs.len(), MAX_LINES);
        for (i, (count, status)) in [(MAX_LINES - 50, "covered"), (50, "partial"), (0, "partial")]
            .into_iter()
            .enumerate()
        {
            assert_eq!(first.offsets[i].1, (count * "source-0\n".len()) as i64);
            assert_eq!(first.coverage[i].status, status);
            assert_eq!(
                first
                    .logs
                    .iter()
                    .filter(|log| log.body == format!("source-{i}"))
                    .count(),
                count
            );
        }
        let cursors = first
            .offsets
            .into_iter()
            .map(|(s, n, id)| (s, (n, id)))
            .collect();
        let second = collect(&sources, &cursors, &HashMap::new()).unwrap();
        assert_eq!(second.logs.len(), 51);
        assert!(second.logs[..50].iter().all(|log| log.body == "source-1"));
        assert_eq!(second.logs[50].body, "source-2");
        assert!(second.coverage.iter().all(|c| c.status == "covered"));
        for src in sources {
            fs::remove_file(src.path).unwrap();
        }
    }

    #[test]
    fn crlf_and_multibyte_lines_advance_by_raw_bytes() {
        let path = std::env::temp_dir().join(format!(
            "jani-probe-utf8-{}-{}.log",
            std::process::id(),
            now_nanos()
        ));
        let prefix = format!("\r\n\n{}", "é雪𐐀\r\n".repeat(MAX_LINES - 2));
        let contents = format!("{prefix}最後\r\n");
        fs::write(&path, &contents).unwrap();
        let sources = [Source {
            service: "api".into(),
            path: path.to_str().unwrap().into(),
        }];
        let first = collect(&sources, &HashMap::new(), &HashMap::new()).unwrap();
        assert_eq!(first.logs.len(), MAX_LINES);
        assert_eq!(first.logs[0].body, "");
        assert_eq!(first.logs[1].body, "");
        assert!(first.logs[2..].iter().all(|log| log.body == "é雪𐐀"));
        assert_eq!(first.offsets[0].1, prefix.len() as i64);
        assert_eq!(first.coverage[0].status, "partial");
        let cursors = first
            .offsets
            .into_iter()
            .map(|(s, n, id)| (s, (n, id)))
            .collect();
        let second = collect(&sources, &cursors, &HashMap::new()).unwrap();
        assert_eq!(second.logs.len(), 1);
        assert_eq!(second.logs[0].body, "最後");
        assert_eq!(second.offsets[0].1, contents.len() as i64);
        assert_eq!(second.coverage[0].status, "covered");
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn incomplete_trailing_line_is_retained_until_newline() {
        use std::io::Write;
        let path = std::env::temp_dir().join(format!(
            "jani-probe-tail-{}-{}.log",
            std::process::id(),
            now_nanos()
        ));
        fs::write(&path, "ready\n雪").unwrap();
        let sources = [Source {
            service: "api".into(),
            path: path.to_str().unwrap().into(),
        }];
        let first = collect(&sources, &HashMap::new(), &HashMap::new()).unwrap();
        assert_eq!(first.logs.len(), 1);
        assert_eq!(first.logs[0].body, "ready");
        assert_eq!(first.offsets[0].1, "ready\n".len() as i64);
        assert_eq!(first.coverage[0].status, "partial");
        let cursors = first
            .offsets
            .into_iter()
            .map(|(s, n, id)| (s, (n, id)))
            .collect();
        let second = collect(&sources, &cursors, &HashMap::new()).unwrap();
        assert!(second.logs.is_empty());
        assert_eq!(second.offsets[0].1, "ready\n".len() as i64);
        assert_eq!(second.coverage[0].status, "partial");
        fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"\r\n")
            .unwrap();
        let third = collect(&sources, &cursors, &HashMap::new()).unwrap();
        assert_eq!(third.logs.len(), 1);
        assert_eq!(third.logs[0].body, "雪");
        assert_eq!(third.offsets[0].1, "ready\n雪\r\n".len() as i64);
        assert_eq!(third.coverage[0].status, "covered");
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn invalid_utf8_advances_by_raw_bytes_and_reports_partial() {
        let path = std::env::temp_dir().join(format!(
            "jani-probe-lossy-{}-{}.log",
            std::process::id(),
            now_nanos()
        ));
        let prefix = b"\xff\r\n".repeat(MAX_LINES);
        let mut contents = prefix.clone();
        contents.extend_from_slice(b"done\n");
        fs::write(&path, &contents).unwrap();
        let sources = [Source {
            service: "api".into(),
            path: path.to_str().unwrap().into(),
        }];
        let first = collect(&sources, &HashMap::new(), &HashMap::new()).unwrap();
        assert_eq!(first.logs.len(), MAX_LINES);
        assert!(first.logs.iter().all(|log| log.body == "\u{fffd}"));
        assert_eq!(first.offsets[0].1, prefix.len() as i64);
        assert_eq!(first.coverage[0].status, "partial");
        assert_eq!(
            first.coverage[0].reason,
            "non-UTF8 input decoded with replacement"
        );
        let cursors = first
            .offsets
            .into_iter()
            .map(|(s, n, id)| (s, (n, id)))
            .collect();
        let second = collect(&sources, &cursors, &HashMap::new()).unwrap();
        assert_eq!(second.logs.len(), 1);
        assert_eq!(second.logs[0].body, "done");
        assert_eq!(second.offsets[0].1, contents.len() as i64);
        assert_eq!(second.coverage[0].status, "covered");
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn byte_limit_does_not_skip_a_split_line() {
        let path = std::env::temp_dir().join(format!(
            "jani-probe-bytes-{}-{}.log",
            std::process::id(),
            now_nanos()
        ));
        let line = format!("{}\n", "x".repeat(1000));
        let contents = line.repeat(300);
        fs::write(&path, &contents).unwrap();
        let sources = [Source {
            service: "api".into(),
            path: path.to_str().unwrap().into(),
        }];
        let mut cursors = HashMap::new();
        let mut delivered = 0;
        let full_batch = MAX_CAPTURE_BYTES / line.len();
        for count in [full_batch, full_batch, 300 - full_batch * 2] {
            let got = collect(&sources, &cursors, &HashMap::new()).unwrap();
            assert_eq!(got.logs.len(), count);
            assert!(got
                .logs
                .iter()
                .all(|log| log.body == line.trim_end_matches('\n')));
            delivered += count;
            assert_eq!(got.offsets[0].1, (delivered * line.len()) as i64);
            assert_eq!(
                got.coverage[0].status,
                if delivered < 300 {
                    "partial"
                } else {
                    "covered"
                }
            );
            cursors = got
                .offsets
                .into_iter()
                .map(|(s, n, id)| (s, (n, id)))
                .collect();
        }
        assert_eq!(delivered, 300);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn line_and_file_truncation_keep_partial_coverage_and_raw_offsets() {
        let path = std::env::temp_dir().join(format!(
            "jani-probe-truncated-{}-{}.log",
            std::process::id(),
            now_nanos()
        ));
        let contents = format!("{}\n", "x".repeat(MAX_LOG_CHARS + 1));
        fs::write(&path, &contents).unwrap();
        let sources = [Source {
            service: "api".into(),
            path: path.to_str().unwrap().into(),
        }];
        let first = collect(&sources, &HashMap::new(), &HashMap::new()).unwrap();
        assert_eq!(first.logs.len(), 1);
        assert_eq!(first.logs[0].body.chars().count(), MAX_LOG_CHARS);
        assert_eq!(first.offsets[0].1, contents.len() as i64);
        assert_eq!(first.coverage[0].status, "partial");
        assert_eq!(
            first.coverage[0].reason,
            "log line exceeded 4096 chars; truncated"
        );
        let cursors = first
            .offsets
            .into_iter()
            .map(|(s, n, id)| (s, (n, id)))
            .collect();
        fs::write(&path, "new\n").unwrap();
        let second = collect(&sources, &cursors, &HashMap::new()).unwrap();
        assert_eq!(second.logs.len(), 1);
        assert_eq!(second.logs[0].body, "new");
        assert_eq!(second.offsets[0].1, "new\n".len() as i64);
        assert_eq!(second.coverage[0].status, "partial");
        assert_eq!(
            second.coverage[0].reason,
            "source truncated; earlier log loss possible"
        );
        let cursors = second
            .offsets
            .into_iter()
            .map(|(s, n, id)| (s, (n, id)))
            .collect();
        let third = collect(&sources, &cursors, &HashMap::new()).unwrap();
        assert!(third.logs.is_empty());
        assert_eq!(third.offsets[0].1, "new\n".len() as i64);
        assert_eq!(third.coverage[0].status, "covered");
        fs::remove_file(path).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn file_rotation_restarts_at_first_undelivered_byte() {
        let path = std::env::temp_dir().join(format!(
            "jani-probe-rotated-{}-{}.log",
            std::process::id(),
            now_nanos()
        ));
        let rotated = path.with_extension("old");
        fs::write(&path, "old\n").unwrap();
        let sources = [Source {
            service: "api".into(),
            path: path.to_str().unwrap().into(),
        }];
        let first = collect(&sources, &HashMap::new(), &HashMap::new()).unwrap();
        assert_eq!(first.offsets[0].1, "old\n".len() as i64);
        let identity = first.offsets[0].2.clone();
        let cursors = first
            .offsets
            .into_iter()
            .map(|(s, n, id)| (s, (n, id)))
            .collect();
        fs::rename(&path, &rotated).unwrap();
        fs::write(&path, "replacement\n").unwrap();
        let second = collect(&sources, &cursors, &HashMap::new()).unwrap();
        assert_eq!(second.logs.len(), 1);
        assert_eq!(second.logs[0].body, "replacement");
        assert_eq!(second.offsets[0].1, "replacement\n".len() as i64);
        assert_ne!(second.offsets[0].2, identity);
        assert_eq!(second.coverage[0].status, "partial");
        fs::remove_file(path).unwrap();
        fs::remove_file(rotated).unwrap();
    }

    #[test]
    fn empty_secret_assignment_at_eol_is_jsmt_accepted() {
        for key in jani_jsmt::SECRET_KEYS {
            for separator in ["=", ":"] {
                let input = format!("login {key}{separator}");
                assert!(!jani_jsmt::redacted(&input));
                let result = scrub(&input);
                assert_eq!(result, format!("{input}[REDACTED]"));
                assert!(jani_jsmt::redacted(&result));
            }
        }
        let path = std::env::temp_dir().join(format!(
            "jani-probe-empty-{}-{}.log",
            std::process::id(),
            now_nanos()
        ));
        fs::write(&path, "password=\n").unwrap();
        let sources = [Source {
            service: "api".into(),
            path: path.to_str().unwrap().into(),
        }];
        let got = collect(&sources, &HashMap::new(), &HashMap::new()).unwrap();
        assert_eq!(got.logs.len(), 1);
        assert_eq!(got.logs[0].body, "password=[REDACTED]");
        assert!(jani_jsmt::redacted(&got.logs[0].body));
        assert_eq!(got.offsets[0].1, "password=\n".len() as i64);
        let (payload, _) = create_envelope(
            "tenant",
            "device",
            1,
            observed_at(),
            got.metrics,
            got.logs,
            got.coverage,
        )
        .unwrap();
        assert!(jani_jsmt::validate(&payload).is_ok());
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn missing_log_produces_coverage_gap() {
        let bad = Source {
            service: "api".into(),
            path: std::env::temp_dir()
                .join(format!(
                    "jani-probe-missing-{}-impossible.log",
                    std::process::id()
                ))
                .to_string_lossy()
                .into_owned(),
        };
        let got = collect(&[bad], &HashMap::new(), &HashMap::new()).unwrap();
        assert_eq!(got.coverage[0].status, "missing");
        assert!(got.logs.is_empty());
    }
}
