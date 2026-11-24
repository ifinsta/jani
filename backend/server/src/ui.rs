//! `jani ui`: a local web workspace over the same state the CLI reads.
//!
//! Trust boundary. The server binds loopback only, rejects any request whose
//! `Host` is not the exact loopback origin it printed (DNS rebinding), and
//! requires a per-process random token in `X-Jani-Token` for every `/api/`
//! call. The token is printed in the URL fragment so it never reaches a server
//! log or a `Referer`. Mutating calls additionally require a JSON content type
//! and, when an `Origin` is present, the same origin. The page is served with a
//! nonce-based CSP and no third-party hosts.
//!
//! Authority is the authority of the `jani ui` process: scopes are frozen at
//! startup exactly as in the terminal session, and approvals are decided with
//! the same verified-identity path as `jani approvals decide`. The browser can
//! never widen what the process was started with.

use crate::agent::{Agent, TurnEvent};
use crate::config::{Config, JResult};
use crate::policy::PolicyEngine;
use crate::repl::{execute_command, Flow};
use crate::skills::Catalog;
use crate::{approvals, budget, evidence, identity, journal, recovery};
use serde_json::{json, Map, Value};
use std::collections::hash_map::RandomState;
use std::collections::BTreeMap;
use std::hash::{BuildHasher, Hasher};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, SocketAddrV4, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Mutex};
use std::time::Duration;

const INDEX_HTML: &str = include_str!("ui/index.html");
const MAX_BODY: usize = 64 * 1024;
const MAX_HEADERS: usize = 64;
const MAX_LINE: usize = 8 * 1024;
const MAX_CONNECTIONS: usize = 32;
const JOURNAL_PAGE: usize = 500;
const TRANSCRIPT_CAP: usize = 2000;

enum Job {
    Line(String),
}

#[derive(Default)]
struct Transcript {
    entries: Vec<Value>,
    next: u64,
    busy: bool,
    step: String,
    ended: bool,
    turns: u64,
    model: String,
    task_id: String,
    fault: Option<String>,
}

struct Shared<'a> {
    cfg: &'a Config,
    catalog: &'a Catalog,
    token: String,
    nonce: String,
    hosts: Vec<String>,
    transcript: Mutex<Transcript>,
    jobs: Mutex<Option<mpsc::Sender<Job>>>,
    cancel: AtomicBool,
    live: AtomicUsize,
}

fn random_hex(bytes: usize) -> String {
    let mut out = String::with_capacity(bytes * 2);
    while out.len() < bytes * 2 {
        // RandomState is seeded from the operating system's entropy source.
        let mut hasher = RandomState::new().build_hasher();
        hasher.write_usize(out.len());
        out.push_str(&format!("{:016x}", hasher.finish()));
    }
    out.truncate(bytes * 2);
    out
}

fn constant_time_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let mut diff = a.len() ^ b.len();
    for i in 0..a.len().max(b.len()) {
        diff |= usize::from(a.get(i).copied().unwrap_or(0) ^ b.get(i).copied().unwrap_or(0));
    }
    diff == 0
}

pub fn command(cfg: &Config, catalog: &Catalog) -> JResult<()> {
    let listener = TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, cfg.ui_port))
        .map_err(|e| format!("cannot bind 127.0.0.1:{}: {e}", cfg.ui_port))?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    let shared = Shared {
        cfg,
        catalog,
        token: random_hex(24),
        nonce: random_hex(12),
        hosts: vec![format!("127.0.0.1:{port}"), format!("localhost:{port}")],
        transcript: Mutex::new(Transcript::default()),
        jobs: Mutex::new(None),
        cancel: AtomicBool::new(false),
        live: AtomicUsize::new(0),
    };
    {
        // A closed stdout must not take the server down.
        let mut out = std::io::stdout();
        let _ = writeln!(
            out,
            "Jani workspace: http://127.0.0.1:{port}/#token={}",
            shared.token
        );
        let _ = writeln!(
            out,
            "Loopback only. Authority is frozen at startup (writes: {}, commands: {}). Press Ctrl+C to stop.",
            cfg.allow_write, cfg.allow_exec
        );
    }
    let (tx, rx) = mpsc::channel();
    *shared.jobs.lock().map_err(|_| "ui state poisoned")? = Some(tx);
    std::thread::scope(|scope| {
        scope.spawn(|| worker(&shared, rx));
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            if shared.live.fetch_add(1, Ordering::SeqCst) >= MAX_CONNECTIONS {
                shared.live.fetch_sub(1, Ordering::SeqCst);
                let mut stream = stream;
                let _ = respond(&mut stream, 503, "text/plain", b"busy", &[]);
                continue;
            }
            let shared = &shared;
            scope.spawn(move || {
                handle(shared, stream);
                shared.live.fetch_sub(1, Ordering::SeqCst);
            });
        }
    });
    Ok(())
}

// ---------------------------------------------------------------- session

impl Shared<'_> {
    fn push(&self, kind: &str, text: impl Into<String>, extra: Value) {
        let Ok(mut t) = self.transcript.lock() else {
            return;
        };
        let mut entry = Map::new();
        entry.insert("n".into(), json!(t.next));
        entry.insert("kind".into(), json!(kind));
        entry.insert("text".into(), json!(text.into()));
        if let Value::Object(map) = extra {
            for (key, value) in map {
                entry.insert(key, value);
            }
        }
        t.next += 1;
        t.entries.push(Value::Object(entry));
        if t.entries.len() > TRANSCRIPT_CAP {
            let drop = t.entries.len() - TRANSCRIPT_CAP;
            t.entries.drain(..drop);
        }
    }

    fn meta(&self, f: impl FnOnce(&mut Transcript)) {
        if let Ok(mut t) = self.transcript.lock() {
            f(&mut t);
        }
    }
}

fn worker(shared: &Shared<'_>, rx: mpsc::Receiver<Job>) {
    let mut agent = match Agent::new(shared.cfg, shared.catalog) {
        Ok(agent) => agent,
        Err(error) => {
            shared.meta(|t| t.fault = Some(error.clone()));
            shared.push(
                "error",
                format!("jani: cannot start the session: {error}"),
                json!({}),
            );
            for _ in rx {
                shared.push(
                    "error",
                    "jani: this session did not start; the read-only views still work.",
                    json!({}),
                );
            }
            return;
        }
    };
    shared.meta(|t| {
        t.model = agent.model().to_string();
        t.task_id = agent.task_id().to_string();
        t.turns = agent.turns();
    });
    for Job::Line(line) in rx {
        shared.meta(|t| {
            t.busy = true;
            t.step = "working".into();
        });
        if line.starts_with('/') && !line.contains('\n') {
            shared.push("command", line.clone(), json!({}));
            match execute_command(&mut agent, shared.catalog, &line) {
                Ok((Flow::Exit, _)) => {
                    shared.meta(|t| t.ended = true);
                    shared.push(
                        "note",
                        "Session closed. Journal entries are kept. Restart `jani ui` to begin another.",
                        json!({}),
                    );
                }
                Ok((_, text)) => shared.push("note", text, json!({})),
                Err(error) => shared.push("error", format!("jani: {error}"), json!({})),
            }
        } else {
            shared.push("you", line.clone(), json!({}));
            shared.cancel.store(false, Ordering::SeqCst);
            let mut reasoning = String::new();
            let result = agent.run_turn_events_with_cancel(
                &line,
                &mut |event| match event {
                    TurnEvent::Step { step, max_steps } => {
                        shared.meta(|t| t.step = format!("step {step} of {max_steps}"));
                    }
                    TurnEvent::ToolStarted { activity, .. } => {
                        shared.meta(|t| t.step = activity);
                    }
                    TurnEvent::ToolFinished {
                        name,
                        activity,
                        ok,
                        duration_ms,
                    } => shared.push(
                        "tool",
                        activity,
                        json!({"name": name, "ok": ok, "ms": duration_ms}),
                    ),
                    TurnEvent::Reasoning(text) => reasoning.push_str(&text),
                    TurnEvent::Text(_) => {}
                },
                &shared.cancel,
            );
            if !reasoning.trim().is_empty() {
                shared.push("think", reasoning.trim().to_string(), json!({}));
            }
            match result {
                Ok(answer) => shared.push("jani", answer, json!({})),
                Err(error) => {
                    shared.push("error", format!("jani: {error}"), json!({}));
                    shared.push(
                        "note",
                        "Conversation context was restored to the last successful turn. Prior file or external tool effects may remain; inspect the audit journal and worktree.",
                        json!({}),
                    );
                }
            }
            for request in agent.pending_approvals() {
                shared.push(
                    "note",
                    format!(
                        "approval required: {} ({} {}). A verified approver decides it outside this session; then attach it with /approval {}.",
                        request["approvalId"].as_str().unwrap_or("unknown"),
                        request["capability"].as_str().unwrap_or("unknown"),
                        request["resource"].as_str().unwrap_or("unknown"),
                        request["approvalId"].as_str().unwrap_or("ID"),
                    ),
                    json!({}),
                );
            }
        }
        shared.meta(|t| {
            t.busy = false;
            t.step.clear();
            t.model = agent.model().to_string();
            t.task_id = agent.task_id().to_string();
            t.turns = agent.turns();
        });
    }
}

// ------------------------------------------------------------------- http

struct Request {
    method: String,
    path: String,
    query: BTreeMap<String, String>,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
}

fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
                if let Ok(value) = u8::from_str_radix(hex, 16) {
                    out.push(value);
                    i += 3;
                    continue;
                }
                out.push(b'%');
                i += 1;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            other => {
                out.push(other);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn read_request(stream: &mut TcpStream) -> Result<Request, (u16, &'static str)> {
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .map_err(|_| (500, "timeout setup failed"))?;
    let mut reader = BufReader::new(stream.try_clone().map_err(|_| (500, "clone failed"))?);
    let mut line = String::new();
    let n = (&mut reader)
        .take(MAX_LINE as u64)
        .read_line(&mut line)
        .map_err(|_| (400, "unreadable request"))?;
    if n == 0 || !line.ends_with('\n') {
        return Err((400, "malformed request line"));
    }
    let mut parts = line.split_whitespace();
    let method = parts.next().ok_or((400, "missing method"))?.to_string();
    let target = parts.next().ok_or((400, "missing target"))?.to_string();
    let (path, raw_query) = match target.split_once('?') {
        Some((path, query)) => (path.to_string(), query.to_string()),
        None => (target, String::new()),
    };
    let mut query = BTreeMap::new();
    for pair in raw_query.split('&').filter(|p| !p.is_empty()) {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        query.insert(percent_decode(key), percent_decode(value));
    }
    let mut headers = BTreeMap::new();
    loop {
        let mut header = String::new();
        let n = (&mut reader)
            .take(MAX_LINE as u64)
            .read_line(&mut header)
            .map_err(|_| (400, "unreadable header"))?;
        if n == 0 {
            return Err((400, "truncated headers"));
        }
        let header = header.trim_end_matches(['\r', '\n']);
        if header.is_empty() {
            break;
        }
        if headers.len() >= MAX_HEADERS {
            return Err((431, "too many headers"));
        }
        if let Some((name, value)) = header.split_once(':') {
            headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_string());
        }
    }
    let length: usize = headers
        .get("content-length")
        .map(|v| v.parse().map_err(|_| (400, "bad content-length")))
        .transpose()?
        .unwrap_or(0);
    if length > MAX_BODY {
        return Err((413, "body too large"));
    }
    let mut body = vec![0u8; length];
    reader
        .read_exact(&mut body)
        .map_err(|_| (400, "truncated body"))?;
    Ok(Request {
        method,
        path,
        query,
        headers,
        body,
    })
}

fn respond(
    stream: &mut TcpStream,
    status: u16,
    content_type: &str,
    body: &[u8],
    extra: &[(&str, String)],
) -> std::io::Result<()> {
    let reason = match status {
        200 => "OK",
        202 => "Accepted",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        409 => "Conflict",
        413 => "Payload Too Large",
        415 => "Unsupported Media Type",
        431 => "Request Header Fields Too Large",
        500 => "Internal Server Error",
        _ => "Service Unavailable",
    };
    let mut head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nReferrer-Policy: no-referrer\r\nCross-Origin-Resource-Policy: same-origin\r\nConnection: close\r\n",
        body.len()
    );
    for (name, value) in extra {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes())?;
    stream.write_all(body)?;
    stream.flush()
}

fn json_response(stream: &mut TcpStream, status: u16, value: &Value) {
    let body = serde_json::to_vec(value).unwrap_or_else(|_| b"{}".to_vec());
    let _ = respond(
        stream,
        status,
        "application/json; charset=utf-8",
        &body,
        &[],
    );
}

fn fail(stream: &mut TcpStream, status: u16, message: &str) {
    json_response(stream, status, &json!({ "error": message }));
}

fn handle(shared: &Shared<'_>, mut stream: TcpStream) {
    serve(shared, &mut stream);
    // Closing with unread request bytes makes the kernel send a reset, which
    // can discard the response the client has not read yet. Half-close, then
    // drain a bounded amount of whatever the peer was still sending.
    let _ = stream.shutdown(std::net::Shutdown::Write);
    let _ = stream.set_read_timeout(Some(Duration::from_millis(200)));
    let _ = std::io::copy(&mut (&mut stream).take(1 << 20), &mut std::io::sink());
}

fn serve(shared: &Shared<'_>, stream: &mut TcpStream) {
    let request = match read_request(stream) {
        Ok(request) => request,
        Err((status, message)) => return fail(stream, status, message),
    };
    let host = request
        .headers
        .get("host")
        .map(String::as_str)
        .unwrap_or("");
    if !shared.hosts.iter().any(|allowed| allowed == host) {
        return fail(stream, 403, "unexpected Host header");
    }
    if request.method != "GET" && request.method != "POST" {
        return fail(stream, 405, "method not allowed");
    }
    if request.method == "GET" && request.path == "/" {
        let csp = format!(
            "default-src 'none'; script-src 'nonce-{n}'; style-src 'nonce-{n}'; style-src-attr 'unsafe-inline'; connect-src 'self'; img-src data:; base-uri 'none'; form-action 'none'; frame-ancestors 'none'",
            n = shared.nonce
        );
        let page = INDEX_HTML.replace("__NONCE__", &shared.nonce);
        let _ = respond(
            stream,
            200,
            "text/html; charset=utf-8",
            page.as_bytes(),
            &[
                ("Content-Security-Policy", csp),
                ("X-Frame-Options", "DENY".into()),
            ],
        );
        return;
    }
    if !request.path.starts_with("/api/") {
        return fail(stream, 404, "not found");
    }
    let presented = request
        .headers
        .get("x-jani-token")
        .map(String::as_str)
        .unwrap_or("");
    if !constant_time_eq(presented, &shared.token) {
        return fail(stream, 401, "missing or invalid token");
    }
    if request.method == "POST" {
        let json_type = request
            .headers
            .get("content-type")
            .map(|v| v.starts_with("application/json"))
            .unwrap_or(false);
        if !json_type {
            return fail(stream, 415, "POST requires application/json");
        }
        if let Some(origin) = request.headers.get("origin") {
            if !shared
                .hosts
                .iter()
                .any(|h| *origin == format!("http://{h}"))
            {
                return fail(stream, 403, "cross-origin request refused");
            }
        }
    }
    match route(shared, &request) {
        Ok((status, value)) => json_response(stream, status, &value),
        Err((status, message)) => fail(stream, status, &message),
    }
}

type Reply = Result<(u16, Value), (u16, String)>;

fn server_error(message: String) -> (u16, String) {
    (500, message)
}

fn route(shared: &Shared<'_>, request: &Request) -> Reply {
    let cfg = shared.cfg;
    match (request.method.as_str(), request.path.as_str()) {
        ("GET", "/api/overview") => overview(shared).map(|v| (200, v)).map_err(server_error),
        ("GET", "/api/runs") => runs(cfg).map(|v| (200, v)).map_err(server_error),
        ("GET", "/api/run") => {
            let id = request
                .query
                .get("id")
                .ok_or((400, "id is required".to_string()))?;
            run_detail(cfg, id)
        }
        ("GET", "/api/journal") => journal_page(cfg).map(|v| (200, v)).map_err(server_error),
        ("GET", "/api/approvals") => {
            let store = approvals::ApprovalStore::open(&cfg.state_dir).map_err(server_error)?;
            Ok((200, store.to_json()))
        }
        ("POST", "/api/approvals/decide") => decide(shared, request),
        ("GET", "/api/budgets") => Ok((200, budgets(cfg))),
        ("GET", "/api/recovery") => recovery::status(&cfg.state_dir)
            .map(|v| (200, v))
            .map_err(server_error),
        ("GET", "/api/session") => Ok((200, session(shared, request))),
        ("POST", "/api/session/send") => send(shared, request),
        ("POST", "/api/session/cancel") => {
            shared.cancel.store(true, Ordering::SeqCst);
            Ok((202, json!({"ok": true})))
        }
        _ => Err((404, "not found".into())),
    }
}

// ------------------------------------------------------------------ views

fn overview(shared: &Shared<'_>) -> JResult<Value> {
    let cfg = shared.cfg;
    let identity = identity::load(cfg);
    let policy = PolicyEngine::load(cfg);
    let journal_path = cfg.state_dir.join(journal::JOURNAL_FILE);
    let integrity = if journal_path.is_file() {
        journal::verify(&cfg.state_dir)?
    } else {
        json!({"ok": true, "events": 0, "created": false})
    };
    let host = cfg
        .api_base
        .split("://")
        .nth(1)
        .unwrap_or(&cfg.api_base)
        .split('/')
        .next()
        .unwrap_or("")
        .to_string();
    Ok(json!({
        "version": env!("CARGO_PKG_VERSION"),
        "worktree": cfg.root.display().to_string(),
        "stateDir": cfg.state_dir.display().to_string(),
        "mode": cfg.mode.as_str(),
        "tenant": cfg.tenant,
        "principal": cfg.principal,
        "identity": match &identity {
            Ok(id) => json!({"principal": id.principal, "verified": id.verified, "issuer": id.issuer}),
            Err(error) => json!({"error": error}),
        },
        "policy": match &policy {
            Ok(engine) => json!({
                "summary": engine.describe(),
                "revision": engine.effective.revision,
                "data": engine.data_policy(),
                "budgets": engine.budget_limits(),
            }),
            Err(error) => json!({"error": error}),
        },
        "authority": {
            "allowWrite": cfg.allow_write,
            "writePaths": cfg.write_paths.iter().map(|p| p.display().to_string()).collect::<Vec<_>>(),
            "allowExec": cfg.allow_exec,
            "execPrograms": cfg.exec_programs,
            "sandbox": cfg.sandbox_mode.as_str(),
            "network": cfg.sandbox_net.as_str(),
            "permissions": cfg.permissions.as_ref().map(|p| p.describe()),
        },
        "model": {
            "name": cfg.model,
            "endpointHost": host,
            "protocol": cfg.protocol.as_str(),
            "keyConfigured": cfg.api_key.is_some(),
            "profile": cfg.model_profile,
            "profiles": cfg.model_choices.iter().map(|c| json!({"label": c.label, "model": c.model})).collect::<Vec<_>>(),
        },
        "skills": shared.catalog.skills.values().map(|s| json!({"name": s.name, "description": s.description})).collect::<Vec<_>>(),
        "journal": integrity,
        "warnings": cfg.settings_warnings,
    }))
}

fn load_events(cfg: &Config) -> Result<Vec<Value>, (u16, String)> {
    let path = cfg.state_dir.join(journal::JOURNAL_FILE);
    if !path.is_file() {
        return Ok(Vec::new());
    }
    let integrity = journal::verify(&cfg.state_dir).map_err(server_error)?;
    if integrity["ok"] != true {
        return Err((
            409,
            "audit journal integrity failed: refusing to show unverified events".into(),
        ));
    }
    let events = journal::read_events(&path).map_err(server_error)?;
    Ok(events
        .into_iter()
        .map(|e| serde_json::to_value(e).unwrap_or(Value::Null))
        .collect())
}

fn runs(cfg: &Config) -> Result<Value, String> {
    let events = load_events(cfg).map_err(|e| e.1)?;
    struct Run {
        task: String,
        first: u64,
        last: u64,
        count: usize,
        denials: usize,
        status: &'static str,
    }
    let mut map: BTreeMap<String, Run> = BTreeMap::new();
    for event in &events {
        let id = event["run_id"].as_str().unwrap_or("").to_string();
        let ts = event["timestamp"].as_u64().unwrap_or(0);
        let entry = map.entry(id).or_insert(Run {
            task: event["task_id"].as_str().unwrap_or("").to_string(),
            first: ts,
            last: ts,
            count: 0,
            denials: 0,
            status: "unreconciled",
        });
        entry.last = ts;
        entry.count += 1;
        if event["decision"] == "deny" {
            entry.denials += 1;
        }
        if event["action"] == "agent.run" {
            entry.status = match event["decision"].as_str() {
                Some("completed") => "completed",
                Some("failed") => "failed",
                _ => entry.status,
            };
        }
    }
    let mut list: Vec<Value> = map
        .into_iter()
        .map(|(id, r)| {
            json!({
                "runId": id, "taskId": r.task, "firstAt": r.first, "lastAt": r.last,
                "events": r.count, "denials": r.denials, "status": r.status,
            })
        })
        .collect();
    list.sort_by(|a, b| b["firstAt"].as_u64().cmp(&a["firstAt"].as_u64()));
    Ok(json!(list))
}

fn run_detail(cfg: &Config, id: &str) -> Reply {
    let events: Vec<Value> = load_events(cfg)?
        .into_iter()
        .filter(|e| e["run_id"] == id)
        .collect();
    if events.is_empty() {
        return Err((404, format!("no journal events for run '{id}'")));
    }
    let receipt = evidence::receipt(&cfg.state_dir, id).ok();
    Ok((
        200,
        json!({"runId": id, "events": events, "evidence": receipt}),
    ))
}

fn journal_page(cfg: &Config) -> Result<Value, String> {
    let all = load_events(cfg).map_err(|e| e.1)?;
    let total = all.len();
    let skip = total.saturating_sub(JOURNAL_PAGE);
    Ok(json!({
        "total": total,
        "firstShown": skip + 1,
        "events": all.into_iter().skip(skip).collect::<Vec<_>>(),
    }))
}

fn budgets(cfg: &Config) -> Value {
    let dir = cfg.state_dir.join(budget::BUDGETS_DIR);
    let mut out = Vec::new();
    if let Ok(read) = std::fs::read_dir(&dir) {
        for entry in read.flatten() {
            let Some(name) = entry
                .path()
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
            else {
                continue;
            };
            if let Ok(text) = std::fs::read_to_string(entry.path()) {
                if let Ok(value) = serde_json::from_str::<Value>(&text) {
                    out.push(json!({"file": name, "ledger": value}));
                }
            }
        }
    }
    out.sort_by(|a, b| {
        b["ledger"]["updatedAt"]
            .as_u64()
            .cmp(&a["ledger"]["updatedAt"].as_u64())
    });
    json!(out)
}

fn decide(shared: &Shared<'_>, request: &Request) -> Reply {
    let cfg = shared.cfg;
    let body: Value = serde_json::from_slice(&request.body)
        .map_err(|_| (400, "body must be JSON".to_string()))?;
    let id = body["id"]
        .as_str()
        .ok_or((400, "id is required".to_string()))?;
    let approve = body["approve"]
        .as_bool()
        .ok_or((400, "approve must be true or false".to_string()))?;
    let role = body["role"].as_str().filter(|r| !r.is_empty()).ok_or((
        400,
        "role is required to record an approval decision".to_string(),
    ))?;
    let who = identity::load(cfg).map_err(|e| (403, e))?;
    if !who.verified {
        return Err((
            403,
            "approval decisions require a verified identity assertion; start `jani ui` with --identity".into(),
        ));
    }
    let mut store = approvals::ApprovalStore::open(&cfg.state_dir).map_err(server_error)?;
    let decided = store
        .decide(id, &who.principal, role, approve)
        .map_err(|e| (409, e))?;
    Ok((200, serde_json::to_value(decided).unwrap_or(Value::Null)))
}

fn session(shared: &Shared<'_>, request: &Request) -> Value {
    let after: i64 = request
        .query
        .get("after")
        .and_then(|v| v.parse().ok())
        .unwrap_or(-1);
    let Ok(t) = shared.transcript.lock() else {
        return json!({"error": "session state unavailable"});
    };
    let entries: Vec<&Value> = t
        .entries
        .iter()
        .filter(|e| e["n"].as_i64().unwrap_or(0) > after)
        .collect();
    json!({
        "entries": entries,
        "busy": t.busy,
        "step": t.step,
        "ended": t.ended,
        "turns": t.turns,
        "model": t.model,
        "taskId": t.task_id,
        "fault": t.fault,
    })
}

fn send(shared: &Shared<'_>, request: &Request) -> Reply {
    let body: Value = serde_json::from_slice(&request.body)
        .map_err(|_| (400, "body must be JSON".to_string()))?;
    let text = body["text"].as_str().unwrap_or("").trim_end().to_string();
    if text.is_empty() || text.len() > MAX_BODY {
        return Err((400, "text must be 1..65536 bytes".into()));
    }
    {
        let mut t = shared
            .transcript
            .lock()
            .map_err(|_| server_error("session state unavailable".into()))?;
        if t.ended {
            return Err((409, "the session has ended".into()));
        }
        if t.busy {
            return Err((409, "a turn is already running".into()));
        }
        t.busy = true;
    }
    let sender = shared
        .jobs
        .lock()
        .map_err(|_| server_error("session state unavailable".into()))?
        .clone();
    match sender {
        Some(tx) if tx.send(Job::Line(text)).is_ok() => Ok((202, json!({"ok": true}))),
        _ => {
            shared.meta(|t| t.busy = false);
            Err((409, "the session is not running".into()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_distinct_and_compare_safely() {
        let a = random_hex(24);
        let b = random_hex(24);
        assert_eq!(a.len(), 48);
        assert_ne!(a, b);
        assert!(constant_time_eq(&a, &a));
        assert!(!constant_time_eq(&a, &b));
        assert!(!constant_time_eq("abc", "abcd"));
    }

    #[test]
    fn percent_decoding_handles_edges() {
        assert_eq!(percent_decode("run%5F1+x"), "run_1 x");
        assert_eq!(percent_decode("100%"), "100%");
        assert_eq!(percent_decode("%zz"), "%zz");
    }
}
