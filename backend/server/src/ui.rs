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
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, Shutdown, SocketAddrV4, TcpListener, TcpStream};
#[cfg(unix)]
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Mutex};
use std::time::Duration;

mod views;

const INDEX_HTML: &str = include_str!("ui/index.html");
const MAX_BODY: usize = 64 * 1024;
const MAX_HEADERS: usize = 64;
const MAX_LINE: usize = 8 * 1024;
const MAX_CONNECTIONS: usize = 32;
const TRANSCRIPT_CAP: usize = 2000;

/// One client connection: loopback TCP, or a Unix socket.
enum Conn {
    Tcp(TcpStream),
    #[cfg(unix)]
    Unix(UnixStream),
}

impl Conn {
    fn try_clone(&self) -> std::io::Result<Conn> {
        Ok(match self {
            Conn::Tcp(s) => Conn::Tcp(s.try_clone()?),
            #[cfg(unix)]
            Conn::Unix(s) => Conn::Unix(s.try_clone()?),
        })
    }
    fn set_read_timeout(&self, d: Option<Duration>) -> std::io::Result<()> {
        match self {
            Conn::Tcp(s) => s.set_read_timeout(d),
            #[cfg(unix)]
            Conn::Unix(s) => s.set_read_timeout(d),
        }
    }
    fn shutdown_write(&self) -> std::io::Result<()> {
        match self {
            Conn::Tcp(s) => s.shutdown(Shutdown::Write),
            #[cfg(unix)]
            Conn::Unix(s) => s.shutdown(Shutdown::Write),
        }
    }
}

impl Read for Conn {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Conn::Tcp(s) => s.read(buf),
            #[cfg(unix)]
            Conn::Unix(s) => s.read(buf),
        }
    }
}

impl Write for Conn {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            Conn::Tcp(s) => s.write(buf),
            #[cfg(unix)]
            Conn::Unix(s) => s.write(buf),
        }
    }
    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Conn::Tcp(s) => s.flush(),
            #[cfg(unix)]
            Conn::Unix(s) => s.flush(),
        }
    }
}

enum Listener {
    Tcp(TcpListener),
    #[cfg(unix)]
    Unix(UnixListener),
}

impl Listener {
    fn accept(&self) -> std::io::Result<Conn> {
        match self {
            Listener::Tcp(l) => l.accept().map(|(s, _)| Conn::Tcp(s)),
            #[cfg(unix)]
            Listener::Unix(l) => l.accept().map(|(s, _)| Conn::Unix(s)),
        }
    }
}

/// Bind the Unix socket owner-only. A stale socket from a crashed run is
/// replaced; anything else at that path is refused rather than deleted.
#[cfg(unix)]
fn bind_unix(path: &std::path::Path) -> JResult<UnixListener> {
    use std::os::unix::fs::{FileTypeExt, PermissionsExt};
    if let Ok(meta) = std::fs::symlink_metadata(path) {
        if !meta.file_type().is_socket() {
            return Err(format!("{} exists and is not a socket", path.display()));
        }
        std::fs::remove_file(path).map_err(|e| format!("cannot replace stale socket: {e}"))?;
    }
    let listener =
        UnixListener::bind(path).map_err(|e| format!("cannot bind {}: {e}", path.display()))?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .map_err(|e| format!("cannot restrict {}: {e}", path.display()))?;
    Ok(listener)
}

enum Job {
    Line(String),
    Resume { task_id: String, prompt: String },
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
    /// Where the agent asks about commands `--exec smart` cannot classify as safe.
    gate: std::sync::Arc<crate::consent::ConsentGate>,
}

fn random_hex(bytes: usize) -> JResult<String> {
    let mut random = vec![0; bytes];
    getrandom::getrandom(&mut random)
        .map_err(|error| format!("cannot obtain OS entropy: {error}"))?;
    Ok(hex::encode(random))
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
    let (listener, hosts, address) = match cfg.ui_unix_socket.as_deref() {
        #[cfg(unix)]
        Some(path) => (
            Listener::Unix(bind_unix(path)?),
            // Nothing is reachable over the network, so the only Host that makes
            // sense is a fixed name the front end sends.
            vec!["localhost".to_string()],
            format!("unix:{}", path.display()),
        ),
        #[cfg(not(unix))]
        Some(_) => return Err("--unix-socket is only supported on Unix".into()),
        None => {
            let tcp = TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, cfg.ui_port))
                .map_err(|e| format!("cannot bind 127.0.0.1:{}: {e}", cfg.ui_port))?;
            let port = tcp.local_addr().map_err(|e| e.to_string())?.port();
            (
                Listener::Tcp(tcp),
                vec![format!("127.0.0.1:{port}"), format!("localhost:{port}")],
                format!("http://127.0.0.1:{port}"),
            )
        }
    };
    let shared = Shared {
        cfg,
        catalog,
        token: random_hex(24)?,
        nonce: random_hex(12)?,
        hosts,
        transcript: Mutex::new(Transcript::default()),
        jobs: Mutex::new(None),
        cancel: AtomicBool::new(false),
        live: AtomicUsize::new(0),
        gate: std::sync::Arc::new(crate::consent::ConsentGate::default()),
    };
    {
        // A closed stdout must not take the server down.
        let mut out = std::io::stdout();
        let _ = writeln!(out, "Jani workspace: {address}/#token={}", shared.token);
        let _ = writeln!(
            out,
            "Local only. Authority is frozen at startup (writes: {}, commands: {}). Press Ctrl+C to stop.",
            cfg.allow_write,
            cfg.exec_mode.as_str()
        );
    }
    let (tx, rx) = mpsc::channel();
    *shared.jobs.lock().map_err(|_| "ui state poisoned")? = Some(tx);
    std::thread::scope(|scope| {
        scope.spawn(|| worker(&shared, rx));
        loop {
            let Ok(stream) = listener.accept() else {
                continue;
            };
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
    agent.set_consent(shared.gate.clone());
    shared.meta(|t| {
        t.model = agent.model().to_string();
        t.task_id = agent.task_id().to_string();
        t.turns = agent.turns();
    });
    for job in rx {
        shared.meta(|t| {
            t.busy = true;
            t.step = "working".into();
        });
        match job {
            Job::Line(line) => {
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
                    run_prompt(shared, &mut agent, line);
                }
            }
            Job::Resume { task_id, prompt } => {
                match Agent::resume(shared.cfg, shared.catalog, &task_id) {
                    Ok(mut resumed) => {
                        resumed.set_consent(shared.gate.clone());
                        agent = resumed;
                        shared.push(
                            "note",
                            format!("Resumed task {task_id} from persisted checkpoint."),
                            json!({}),
                        );
                        if !prompt.is_empty() {
                            run_prompt(shared, &mut agent, prompt);
                        }
                    }
                    Err(error) => {
                        shared.push(
                            "error",
                            format!("jani: cannot resume task {task_id}: {error}"),
                            json!({}),
                        );
                    }
                }
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

fn run_prompt(shared: &Shared<'_>, agent: &mut Agent<'_>, line: String) {
    shared.push("you", line.clone(), json!({}));
    shared.cancel.store(false, Ordering::SeqCst);
    shared.gate.rearm();
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

fn read_request(stream: &mut Conn) -> Result<Request, (u16, &'static str)> {
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
    let mut seen_host = false;
    loop {
        let mut header = String::new();
        let n = (&mut reader)
            .take(MAX_LINE as u64)
            .read_line(&mut header)
            .map_err(|_| (400, "unreadable header"))?;
        if n == 0 {
            return Err((400, "truncated headers"));
        }
        if !header.ends_with('\n') {
            return Err((431, "header line too long"));
        }
        let header = header.trim_end_matches(['\r', '\n']);
        if header.is_empty() {
            break;
        }
        if headers.len() >= MAX_HEADERS {
            return Err((431, "too many headers"));
        }
        let Some((name, value)) = header.split_once(':') else {
            return Err((400, "malformed header"));
        };
        let key = name.trim().to_ascii_lowercase();
        let val = value.trim().to_string();
        if key == "transfer-encoding" {
            return Err((501, "transfer-encoding not supported"));
        }
        if key == "content-length" && headers.contains_key("content-length") {
            return Err((400, "duplicate content-length"));
        }
        if key == "host" {
            if seen_host {
                return Err((400, "duplicate host header"));
            }
            seen_host = true;
        }
        headers.insert(key, val);
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
    stream: &mut Conn,
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
        422 => "Unprocessable Entity",
        415 => "Unsupported Media Type",
        431 => "Request Header Fields Too Large",
        500 => "Internal Server Error",
        501 => "Not Implemented",
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

fn json_response(stream: &mut Conn, status: u16, value: &Value) {
    let body = serde_json::to_vec(value).unwrap_or_else(|_| b"{}".to_vec());
    let _ = respond(
        stream,
        status,
        "application/json; charset=utf-8",
        &body,
        &[],
    );
}

fn fail(stream: &mut Conn, status: u16, message: &str) {
    json_response(stream, status, &json!({ "error": message }));
}

fn handle(shared: &Shared<'_>, mut stream: Conn) {
    serve(shared, &mut stream);
    // Closing with unread request bytes makes the kernel send a reset, which
    // can discard the response the client has not read yet. Half-close, then
    // drain a bounded amount of whatever the peer was still sending.
    let _ = stream.shutdown_write();
    let _ = stream.set_read_timeout(Some(Duration::from_millis(200)));
    let _ = std::io::copy(&mut (&mut stream).take(1 << 20), &mut std::io::sink());
}

fn serve(shared: &Shared<'_>, stream: &mut Conn) {
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

/// A subsystem refusal (bad input, denied by policy, nothing to show) is a 422
/// with the subsystem's own words, not a server fault.
fn view(result: JResult<Value>) -> Reply {
    result.map(|v| (200, v)).map_err(|e| (422, e))
}

fn json_body(request: &Request) -> Result<Value, (u16, String)> {
    serde_json::from_slice(&request.body).map_err(|_| (400, "body must be JSON".to_string()))
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
        ("GET", "/api/journal") => journal_page(cfg, &request.query),
        ("GET", "/api/approval") => approval_detail(cfg, &request.query),
        ("GET", "/api/policy") => view(views::policy(cfg)),
        ("POST", "/api/policy/check") => view(views::policy_check(cfg, &json_body(request)?)),
        ("GET", "/api/audit") => view(views::audit(cfg)),
        ("GET", "/api/evidence") => {
            let id = request
                .query
                .get("id")
                .ok_or((400, "id is required".to_string()))?;
            view(views::evidence_export(cfg, id))
        }
        ("GET", "/api/evidence/list") => {
            let events = load_verified_events(cfg)?;
            let values: Vec<Value> = events
                .iter()
                .map(|event| serde_json::to_value(event).unwrap_or(Value::Null))
                .collect();
            let ids: Vec<String> = run_list(&values)
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|r| r["runId"].as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            Ok((200, views::evidence_list(&events, &ids)))
        }
        ("GET", "/api/mirror") => {
            let sub = request
                .query
                .get("sub")
                .map(String::as_str)
                .unwrap_or("workflows");
            view(views::mirror(
                cfg,
                sub,
                request.query.get("run").map(String::as_str),
                request.query.get("other").map(String::as_str),
            ))
        }
        ("GET", "/api/lab") => view(views::lab(
            cfg,
            request.query.get("run").map(String::as_str),
        )),
        ("GET", "/api/attention") => view(crate::attention::overview(cfg)),
        ("GET", "/api/system") => Ok((200, views::system(cfg))),
        ("GET", "/api/plugins") => Ok((200, views::plugins(cfg))),
        ("GET", "/api/plugin") => {
            let id = request
                .query
                .get("id")
                .ok_or((400, "id is required".to_string()))?;
            view(views::plugin_detail(cfg, id))
        }
        ("GET", "/api/native") => Ok((200, views::native_catalog())),
        ("POST", "/api/native/run") => view(views::native_run(cfg, &json_body(request)?)),
        ("GET", "/api/updates") => Ok((200, views::updates_info(cfg))),
        ("POST", "/api/updates/check") => view(views::updates_check(cfg)),
        ("GET", "/api/approvals") => {
            let store = approvals::ApprovalStore::open(&cfg.state_dir).map_err(server_error)?;
            Ok((200, store.to_json()))
        }
        ("POST", "/api/approvals/decide") => decide(shared, request),
        ("GET", "/api/budgets") => Ok((200, budgets(cfg))),
        ("GET", "/api/recovery") => {
            if !cfg.state_dir.join(journal::JOURNAL_FILE).is_file() {
                return Ok((
                    200,
                    json!({
                        "schemaVersion": "jani.recovery-status/v1",
                        "journalChainVerified": true,
                        "unreconciledRuns": 0,
                        "runs": [],
                        "note": "No audit journal exists yet, so no run has been recorded.",
                    }),
                ));
            }
            recovery::status(&cfg.state_dir)
                .map(|v| (200, v))
                .map_err(server_error)
        }
        ("GET", "/api/session") => Ok((200, session(shared, request))),
        ("GET", "/api/tasks") => list_tasks(cfg, &request.query),
        ("POST", "/api/session/send") => send(shared, request),
        ("POST", "/api/session/resume") => resume_session(shared, request),
        ("POST", "/api/consent") => answer_consent(shared, request),
        ("POST", "/api/answer") => answer_question(shared, request),
        ("POST", "/api/session/cancel") => {
            shared.cancel.store(true, Ordering::SeqCst);
            shared.gate.cancel();
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
        // Set by a front end (jani-cloud) that signs people in and proxies to this process.
        "proxied": std::env::var("JANI_UI_PROXIED").as_deref() == Ok("1"),
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
            "execMode": cfg.exec_mode.as_str(),
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

#[derive(Default)]
struct JournalCache {
    path: std::path::PathBuf,
    offset: u64,
    last_digest: String,
    events: Vec<crate::types::AuditEvent>,
}

fn load_verified_events(cfg: &Config) -> Result<Vec<crate::types::AuditEvent>, (u16, String)> {
    use std::io::{BufRead, Seek, SeekFrom};
    use std::sync::OnceLock;
    static CACHE: OnceLock<Mutex<JournalCache>> = OnceLock::new();
    let path = cfg.state_dir.join(journal::JOURNAL_FILE);
    if !path.is_file() {
        return Ok(Vec::new());
    }
    let meta = std::fs::metadata(&path).map_err(|e| (409, e.to_string()))?;
    let len = meta.len();
    let mut cache = CACHE
        .get_or_init(|| Mutex::new(JournalCache::default()))
        .lock()
        .map_err(|_| (500, "journal cache lock poisoned".into()))?;
    if cache.path == path && len == cache.offset {
        return Ok(cache.events.clone());
    }
    if cache.path == path && len > cache.offset && cache.offset > 0 {
        let mut file = std::fs::File::open(&path).map_err(|e| (409, e.to_string()))?;
        if file.seek(SeekFrom::Start(cache.offset)).is_ok() {
            let mut reader = BufReader::new(&mut file);
            let mut line = String::new();
            let mut pos = cache.offset;
            let mut prev = cache.last_digest.clone();
            let mut appended = Vec::new();
            let mut ok = true;
            loop {
                line.clear();
                let n = reader
                    .read_line(&mut line)
                    .map_err(|e| (409, e.to_string()))?;
                if n == 0 {
                    break;
                }
                if !line.ends_with('\n') {
                    ok = false;
                    break;
                }
                let trimmed = line.trim();
                if !trimmed.is_empty() {
                    match serde_json::from_str::<crate::types::AuditEvent>(trimmed)
                        .map_err(|e| e.to_string())
                        .and_then(|ev| journal::verify_next(&ev, &prev).map(|d| (ev, d)))
                    {
                        Ok((ev, next_digest)) => {
                            prev = next_digest;
                            appended.push(ev);
                        }
                        Err(_) => {
                            ok = false;
                            break;
                        }
                    }
                }
                pos += n as u64;
            }
            if ok {
                cache.events.extend(appended);
                cache.offset = pos;
                cache.last_digest = prev;
                return Ok(cache.events.clone());
            }
        }
    }
    let events = journal::read_verified_events(&path).map_err(|error| {
        (
            409,
            format!(
                "audit journal could not be verified: refusing to show unverified events: {error}"
            ),
        )
    })?;
    cache.path = path;
    cache.offset = len;
    cache.last_digest = events
        .last()
        .map(|e| e.event_digest.clone())
        .unwrap_or_default();
    cache.events = events.clone();
    Ok(events)
}

fn load_events(cfg: &Config) -> Result<Vec<Value>, (u16, String)> {
    Ok(load_verified_events(cfg)?
        .into_iter()
        .map(|event| serde_json::to_value(event).unwrap_or(Value::Null))
        .collect())
}

fn run_list(events: &[Value]) -> Value {
    struct Run {
        task: String,
        first: u64,
        last: u64,
        count: usize,
        denials: usize,
        status: &'static str,
    }
    let mut map: BTreeMap<String, Run> = BTreeMap::new();
    for event in events {
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
    // Only agent runs are runs. Governed operations that share the journal
    // (native tools, for example) have no completion record and would
    // otherwise be shown as unfinished work.
    let agent_runs: std::collections::BTreeSet<String> = events
        .iter()
        .filter(|e| e["action"] == "agent.run")
        .filter_map(|e| e["run_id"].as_str().map(str::to_string))
        .collect();
    let mut list: Vec<Value> = map
        .into_iter()
        .filter(|(id, _)| agent_runs.contains(id))
        .map(|(id, r)| {
            json!({
                "runId": id, "taskId": r.task, "firstAt": r.first, "lastAt": r.last,
                "events": r.count, "denials": r.denials, "status": r.status,
            })
        })
        .collect();
    list.sort_by(|a, b| b["firstAt"].as_u64().cmp(&a["firstAt"].as_u64()));
    json!(list)
}

fn runs(cfg: &Config) -> Result<Value, String> {
    let events = load_events(cfg).map_err(|e| e.1)?;
    Ok(run_list(&events))
}

fn run_detail(cfg: &Config, id: &str) -> Reply {
    let events: Vec<_> = load_verified_events(cfg)?
        .into_iter()
        .filter(|event| event.run_id == id)
        .collect();
    if events.is_empty() {
        return Err((404, format!("no journal events for run '{id}'")));
    }
    let receipt = evidence::receipt_from_verified_events(&events, id).ok();
    let events: Vec<Value> = events
        .into_iter()
        .map(|event| serde_json::to_value(event).unwrap_or(Value::Null))
        .collect();
    Ok((
        200,
        json!({"runId": id, "events": events, "evidence": receipt}),
    ))
}

fn journal_page(cfg: &Config, query: &BTreeMap<String, String>) -> Reply {
    let all = load_events(cfg)?;
    let total = all.len();
    let limit = query
        .get("limit")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(200)
        .clamp(1, 1000);
    let before = query
        .get("before")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(usize::MAX);
    let needle = query
        .get("q")
        .map(|q| q.to_lowercase())
        .filter(|q| !q.is_empty());
    let field = |event: &Value, key: &str| event[key].as_str().unwrap_or("").to_string();
    let mut matched: Vec<Value> = Vec::new();
    for (index, mut event) in all.into_iter().enumerate() {
        let seq = index + 1;
        if seq >= before {
            break;
        }
        let keep = ["run", "decision", "action"].iter().all(|k| {
            let want = query.get(*k).map(String::as_str).unwrap_or("");
            let key = if *k == "run" { "run_id" } else { k };
            want.is_empty() || field(&event, key) == want
        }) && needle.as_ref().is_none_or(|n| {
            [
                "run_id",
                "action",
                "decision",
                "reason_code",
                "actor_id",
                "invocation_id",
                "event_digest",
            ]
            .iter()
            .any(|k| field(&event, k).to_lowercase().contains(n.as_str()))
        });
        if keep {
            event["seq"] = json!(seq);
            matched.push(event);
        }
    }
    let matched_count = matched.len();
    let skip = matched_count.saturating_sub(limit);
    let events: Vec<Value> = matched.into_iter().skip(skip).collect();
    Ok((
        200,
        json!({
            "total": total,
            "matched": matched_count,
            "hasMore": skip > 0,
            "oldestSeq": events.first().and_then(|e| e["seq"].as_u64()),
            "events": events,
        }),
    ))
}

fn approval_detail(cfg: &Config, query: &BTreeMap<String, String>) -> Reply {
    let id = query.get("id").ok_or((400, "id is required".to_string()))?;
    let store = approvals::ApprovalStore::open(&cfg.state_dir).map_err(server_error)?;
    let request = store
        .find(id)
        .ok_or((404, format!("unknown approval '{id}'")))?;
    let mut value = serde_json::to_value(request).unwrap_or(Value::Null);
    // The nonce is a replay guard, not something an operator needs on screen.
    if let Some(map) = value.as_object_mut() {
        map.remove("nonce");
    }
    let related: Vec<Value> = load_events(cfg)?
        .into_iter()
        .filter(|e| e["approval_id"] == id.as_str())
        .collect();
    Ok((200, json!({"approval": value, "events": related})))
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
        .decide_with_scopes(
            id,
            &who.tenant,
            &who.principal,
            &who.subject_type,
            role,
            &who.approval_scopes,
            approve,
        )
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
        "question": shared.gate.pending_question().map(|(id, q)| json!({
            "id": id,
            "text": q.text,
            "options": q.options,
        })),
        "consent": shared.gate.pending().map(|(id, q)| json!({
            "id": id,
            "argv": q.argv,
            "command": q.argv.join(" "),
            "reason": q.reason,
            "suggest": q.suggest.join(" "),
        })),
    })
}

/// The operator's reply to a question the agent asked: their words, or `text: null` to
/// leave it for later (the turn then ends with the question).
fn answer_question(shared: &Shared<'_>, request: &Request) -> Reply {
    let body: Value = serde_json::from_slice(&request.body)
        .map_err(|_| (400, "body must be JSON".to_string()))?;
    let id = body["id"]
        .as_u64()
        .ok_or((400, "id is required".to_string()))?;
    let text = match &body["text"] {
        Value::Null => None,
        Value::String(t) if t.chars().count() <= 4000 => Some(t.trim().to_string()),
        _ => {
            return Err((
                400,
                "text must be a string of at most 4000 characters, or null".into(),
            ))
        }
    };
    if shared.gate.answer_question(id, text) {
        Ok((200, json!({"ok": true})))
    } else {
        Err((409, "that question is no longer waiting".into()))
    }
}

/// The operator's answer to a "may I run this?" card.
fn answer_consent(shared: &Shared<'_>, request: &Request) -> Reply {
    let body: Value = serde_json::from_slice(&request.body)
        .map_err(|_| (400, "body must be JSON".to_string()))?;
    let id = body["id"]
        .as_u64()
        .ok_or((400, "id is required".to_string()))?;
    let answer = match body["answer"].as_str() {
        Some("once") => crate::consent::ConsentAnswer::Once,
        Some("session") => crate::consent::ConsentAnswer::Session,
        Some("always") => crate::consent::ConsentAnswer::Always,
        Some("deny") => crate::consent::ConsentAnswer::Deny,
        _ => return Err((400, "answer must be once, session, always or deny".into())),
    };
    if shared.gate.answer(id, answer) {
        Ok((200, json!({"ok": true})))
    } else {
        Err((409, "that question is no longer waiting".into()))
    }
}

fn list_tasks(cfg: &Config, query: &BTreeMap<String, String>) -> Reply {
    let store = jani_store::tasks::TaskStore::open(&cfg.state_dir).map_err(server_error)?;
    let tenant = identity::load(cfg)
        .map(|id| id.tenant)
        .unwrap_or_else(|_| cfg.tenant.clone());
    let status = query
        .get("status")
        .map(String::as_str)
        .filter(|s| !s.is_empty());
    let tasks = store.list(&tenant, status).map_err(server_error)?;
    Ok((200, json!({"tenant": tenant, "tasks": tasks})))
}

fn resume_session(shared: &Shared<'_>, request: &Request) -> Reply {
    let body: Value = json_body(request)?;
    let task_id = body["taskId"]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or((400, "taskId is required".to_string()))?
        .to_string();
    let prompt = body["prompt"].as_str().unwrap_or("").trim_end().to_string();
    dispatch_job(shared, Job::Resume { task_id, prompt })
}

fn send(shared: &Shared<'_>, request: &Request) -> Reply {
    let body: Value = json_body(request)?;
    let text = body["text"].as_str().unwrap_or("").trim_end().to_string();
    if text.is_empty() || text.len() > MAX_BODY {
        return Err((400, "text must be 1..65536 bytes".into()));
    }
    dispatch_job(shared, Job::Line(text))
}

fn dispatch_job(shared: &Shared<'_>, job: Job) -> Reply {
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
        Some(tx) if tx.send(job).is_ok() => Ok((202, json!({"ok": true}))),
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
        let a = random_hex(24).unwrap();
        let b = random_hex(24).unwrap();
        let nonce = random_hex(12).unwrap();
        assert_eq!(nonce.len(), 24);
        assert!(a.bytes().all(|byte| byte.is_ascii_hexdigit()));
        assert!(nonce.bytes().all(|byte| byte.is_ascii_hexdigit()));
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
