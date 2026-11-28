use crate::data::{redact_json, redact_text, Governance};
use crate::types::Effect;
use jani_core::authority::AuthorityWindow;
use reqwest::blocking::Client;
use reqwest::{Method, Url};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fs;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{IpAddr, Shutdown};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const MAX_HEADERS: usize = 16 * 1024;
const MAX_BODY: usize = 2 * 1024 * 1024;
const LOCAL_TIMEOUT: Duration = Duration::from_secs(3);
const UPSTREAM_TIMEOUT: Duration = Duration::from_secs(10);

fn upstream_client() -> Result<&'static Client, String> {
    // Request deadlines bound callers even if the system resolver is still
    // working. Keep the HTTP runtime alive instead of joining DNS at revocation.
    static CLIENT: OnceLock<Result<Client, String>> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            Client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .retry(reqwest::retry::never())
                .pool_max_idle_per_host(0)
                .build()
                .map_err(|error| format!("cannot initialize credential broker client: {error}"))
        })
        .as_ref()
        .map_err(Clone::clone)
}

pub(super) struct CredentialBroker {
    socket: PathBuf,
    lease: String,
    stop_flag: Arc<AtomicBool>,
    active: Arc<Mutex<Option<UnixStream>>>,
    worker: Option<JoinHandle<()>>,
}

impl CredentialBroker {
    pub(super) fn start(
        workspace: &Path,
        allowed_hosts: Vec<String>,
        secrets: BTreeMap<String, String>,
        governance: Governance,
        effect: Effect,
        deadline: Instant,
        authority: AuthorityWindow,
    ) -> Result<Self, String> {
        let mut random = [0; 24];
        getrandom::getrandom(&mut random)
            .map_err(|error| format!("cannot create broker lease: {error}"))?;
        let lease = hex::encode(random);
        let redactor = redaction_pattern(
            secrets
                .values()
                .chain(std::iter::once(&lease))
                .map(String::as_str),
        )?;
        let socket = workspace.join("broker.sock");
        let client = upstream_client()?;
        let listener = UnixListener::bind(&socket)
            .map_err(|error| format!("cannot bind credential broker: {error}"))?;
        let mut broker = Self {
            socket,
            lease,
            stop_flag: Arc::new(AtomicBool::new(false)),
            active: Arc::new(Mutex::new(None)),
            worker: None,
        };
        listener
            .set_nonblocking(true)
            .map_err(|error| error.to_string())?;
        let policy = BrokerPolicy {
            allowed_hosts,
            secrets,
            governance,
            effect,
            redactor,
            client,
            lease: broker.lease.clone(),
            deadline,
            authority,
            stop: broker.stop_flag.clone(),
        };
        let active = broker.active.clone();
        broker.worker = Some(
            thread::Builder::new()
                .name("jani-credential-broker".into())
                .spawn(move || {
                    while !policy.stopped() {
                        match listener.accept() {
                            Ok((stream, _)) => {
                                {
                                    let Ok(mut current) = active.lock() else {
                                        return;
                                    };
                                    if policy.stopped() {
                                        return;
                                    }
                                    let Ok(copy) = stream.try_clone() else {
                                        continue;
                                    };
                                    *current = Some(copy);
                                }
                                let _ = serve(stream, &policy);
                                if let Ok(mut current) = active.lock() {
                                    current.take();
                                }
                            }
                            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                                thread::sleep(Duration::from_millis(10))
                            }
                            Err(_) => return,
                        }
                    }
                })
                .map_err(|error| format!("cannot start credential broker: {error}"))?,
        );
        Ok(broker)
    }

    pub(super) fn context(&self) -> Value {
        json!({"transport": "http+unix", "socket": self.socket, "lease": self.lease})
    }

    pub(super) fn scrub_lease(&self, text: &str) -> String {
        text.replace(&self.lease, "[REDACTED]")
    }

    pub(super) fn scrub_result(&self, result: &mut Value) {
        scrub_exact_values(result, &|text| self.scrub_lease(text));
    }

    pub(super) fn stop(&mut self) {
        self.stop_flag.store(true, Ordering::SeqCst);
        let _ = fs::remove_file(&self.socket);
        if let Ok(active) = self.active.lock() {
            if let Some(stream) = active.as_ref() {
                let _ = stream.shutdown(Shutdown::Both);
            }
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl Drop for CredentialBroker {
    fn drop(&mut self) {
        self.stop();
    }
}

struct BrokerPolicy {
    allowed_hosts: Vec<String>,
    secrets: BTreeMap<String, String>,
    governance: Governance,
    effect: Effect,
    redactor: regex::Regex,
    client: &'static Client,
    lease: String,
    deadline: Instant,
    authority: AuthorityWindow,
    stop: Arc<AtomicBool>,
}

impl BrokerPolicy {
    fn stopped(&self) -> bool {
        self.stop.load(Ordering::SeqCst)
            || Instant::now() >= self.deadline
            || self.authority.require_current().is_err()
    }
}

struct TimedStream {
    stream: UnixStream,
    deadline: Instant,
}

impl TimedStream {
    fn remaining(&self) -> io::Result<Duration> {
        let remaining = self.deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(io::ErrorKind::TimedOut.into());
        }
        Ok(remaining)
    }
}

impl Read for TimedStream {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.stream.set_read_timeout(Some(self.remaining()?))?;
        self.stream.read(buffer)
    }
}

impl Write for TimedStream {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.stream.set_write_timeout(Some(self.remaining()?))?;
        self.stream.write(buffer)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.stream.flush()
    }
}

struct Request {
    method: Method,
    headers: BTreeMap<String, String>,
    body_length: usize,
}

fn header_line(reader: &mut impl BufRead, remaining: &mut usize) -> Option<String> {
    let mut line = String::new();
    let count = reader.take(*remaining as u64).read_line(&mut line).ok()?;
    *remaining -= count;
    line.strip_suffix("\r\n").map(str::to_owned)
}

fn read_headers(reader: &mut impl BufRead) -> Option<Request> {
    let mut remaining = MAX_HEADERS;
    let line = header_line(reader, &mut remaining)?;
    let mut parts = line.split(' ');
    let method = Method::from_bytes(parts.next()?.as_bytes()).ok()?;
    if matches!(method, Method::CONNECT | Method::TRACE)
        || parts.next()? != "/proxy"
        || parts.next()? != "HTTP/1.1"
        || parts.next().is_some()
    {
        return None;
    }
    let mut headers = BTreeMap::new();
    loop {
        let line = header_line(reader, &mut remaining)?;
        if line.is_empty() {
            break;
        }
        let (name, value) = line.split_once(':')?;
        let name = reqwest::header::HeaderName::from_bytes(name.as_bytes()).ok()?;
        let value = value.trim();
        reqwest::header::HeaderValue::from_str(value).ok()?;
        if name == "transfer-encoding"
            || headers.insert(name.to_string(), value.to_owned()).is_some()
        {
            return None;
        }
    }
    let body_length = match headers.get("content-length") {
        Some(value) if value.bytes().all(|byte| byte.is_ascii_digit()) => value.parse().ok()?,
        Some(_) => return None,
        None => 0,
    };
    Some(Request {
        method,
        headers,
        body_length,
    })
}

fn lease_matches(actual: &str, expected: &str) -> bool {
    let actual = actual.as_bytes();
    let expected = expected.as_bytes();
    let mut difference = actual.len() ^ expected.len();
    for (index, expected) in expected.iter().enumerate() {
        difference |= usize::from(actual.get(index).copied().unwrap_or(0) ^ expected);
    }
    difference == 0
}

fn target_allowed(raw: &str, allowed: &[String]) -> Option<Url> {
    if raw.chars().any(char::is_control) || raw.contains('\\') {
        return None;
    }
    let target = Url::parse(raw).ok()?;
    if !target.username().is_empty() || target.password().is_some() || target.fragment().is_some() {
        return None;
    }
    let host = target.host_str()?;
    let address = host
        .trim_matches(['[', ']'])
        .parse::<IpAddr>()
        .ok()
        .map(|address| address.to_canonical());
    if address.is_some_and(|address| match address {
        IpAddr::V4(address) => address.is_link_local(),
        IpAddr::V6(address) => address.is_unicast_link_local(),
    }) {
        return None;
    }
    let local_http = target.scheme() == "http"
        && target.port().is_some()
        && address.is_some_and(|address| address.is_loopback());
    if target.scheme() != "https" && !local_http {
        return None;
    }
    allowed
        .iter()
        .any(|entry| {
            let parsed = if entry.contains("://") {
                Url::parse(entry)
            } else {
                Url::parse(&format!("{}://{entry}", target.scheme()))
            };
            parsed.is_ok_and(|entry| {
                entry.host_str() == Some(host)
                    && entry.scheme() == target.scheme()
                    && entry.port_or_known_default() == target.port_or_known_default()
                    && entry.username().is_empty()
                    && entry.password().is_none()
                    && entry.path() == "/"
                    && entry.query().is_none()
                    && entry.fragment().is_none()
            })
        })
        .then_some(target)
}

fn reply(stream: &mut TimedStream, status: u16, value: &Value) -> io::Result<()> {
    let mut body = value.to_string();
    let mut status = status;
    if body.len() > MAX_BODY {
        status = 502;
        body = "{\"error\":\"broker response exceeds its limit\"}".into();
    }
    let reason = reqwest::StatusCode::from_u16(status)
        .ok()
        .and_then(|code| code.canonical_reason())
        .unwrap_or("Response");
    write!(stream, "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len())?;
    stream.write_all(body.as_bytes())
}

fn error(stream: &mut TimedStream, status: u16, message: &str) -> io::Result<()> {
    reply(stream, status, &json!({"error": message}))
}

fn serve(stream: UnixStream, policy: &BrokerPolicy) -> io::Result<()> {
    let mut stream = TimedStream {
        stream,
        deadline: policy.deadline.min(Instant::now() + LOCAL_TIMEOUT),
    };
    let mut reader = BufReader::new(&mut stream);
    let Some(request) = read_headers(&mut reader) else {
        return error(&mut stream, 400, "invalid broker request");
    };
    let header = |name: &str| request.headers.get(name).map(String::as_str);
    if !lease_matches(header("x-jani-lease").unwrap_or(""), &policy.lease) {
        return error(&mut stream, 401, "invalid broker lease");
    }
    if !matches!(policy.effect, Effect::ExternalWrite | Effect::Irreversible)
        && !matches!(request.method, Method::GET | Method::HEAD)
    {
        return error(
            &mut stream,
            403,
            "broker method exceeds the capability effect",
        );
    }
    if request.body_length > MAX_BODY {
        return error(&mut stream, 413, "broker request body exceeds its limit");
    }
    let Some(target) = target_allowed(
        header("x-jani-target-url").unwrap_or(""),
        &policy.allowed_hosts,
    ) else {
        return error(
            &mut stream,
            403,
            "broker target is outside the declared origins",
        );
    };
    let secret = match header("x-jani-secret-name") {
        Some(name) => match policy.secrets.get(name) {
            Some(secret) => Some(secret),
            None => return error(&mut stream, 403, "broker credential is unavailable"),
        },
        None if policy.secrets.len() > 1 => {
            return error(&mut stream, 400, "broker credential must be selected")
        }
        None => policy.secrets.values().next(),
    };
    let auth_header = header("x-jani-auth-header").unwrap_or("Authorization");
    if ![
        "Authorization",
        "X-API-Key",
        "X-Auth-Token",
        "Private-Token",
    ]
    .iter()
    .any(|header| header.eq_ignore_ascii_case(auth_header))
    {
        return error(&mut stream, 400, "unsupported broker authentication header");
    }
    let mut body = vec![0; request.body_length];
    reader.read_exact(&mut body)?;
    if policy.stopped() {
        return Ok(());
    }
    let remaining = policy.deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return Ok(());
    }
    let mut outgoing = policy
        .client
        .request(request.method.clone(), target.clone())
        .timeout(remaining.min(UPSTREAM_TIMEOUT));
    if !body.is_empty() {
        outgoing = outgoing.body(body);
    }
    if let Some(content_type) = header("content-type") {
        outgoing = outgoing.header("Content-Type", content_type);
    }
    if let Some(secret) = secret {
        outgoing = if auth_header.eq_ignore_ascii_case("authorization") {
            outgoing.bearer_auth(secret)
        } else {
            outgoing.header(auth_header, secret)
        };
    }
    if policy.stopped() {
        return Ok(());
    }
    let response = outgoing.send();
    stream.deadline = policy.deadline.min(Instant::now() + LOCAL_TIMEOUT);
    let Ok(response) = response else {
        return error(&mut stream, 502, "broker upstream request failed");
    };
    let status = response.status().as_u16();
    let mut bytes = Vec::new();
    if response
        .take((MAX_BODY + 1) as u64)
        .read_to_end(&mut bytes)
        .is_err()
        || bytes.len() > MAX_BODY
    {
        return error(
            &mut stream,
            502,
            "broker upstream body exceeds its limit or could not be read",
        );
    }
    if policy.stopped() {
        return Ok(());
    }
    let scrub = |text: &str| policy.redactor.replace_all(text, "[REDACTED]").into_owned();
    let mut result = match serde_json::from_slice::<Value>(&bytes) {
        Ok(mut value) => {
            scrub_exact_values(&mut value, &scrub);
            value
        }
        Err(_) => {
            let (clean, _) = redact_text(&scrub(&String::from_utf8_lossy(&bytes)));
            let classification = policy.governance.classify_tool_output(
                "connector-response",
                target.as_str(),
                &clean,
            );
            json!({"body": clean, "classification": classification.effective.as_str()})
        }
    };
    redact_json(&mut result);
    if policy.stopped() {
        return Ok(());
    }
    reply(&mut stream, status, &result)
}

fn redaction_pattern<'a>(words: impl Iterator<Item = &'a str>) -> Result<regex::Regex, String> {
    let mut words: Vec<_> = words.filter(|word| !word.is_empty()).collect();
    words.sort_unstable_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));
    words.dedup();
    let pattern = words
        .into_iter()
        .map(regex::escape)
        .collect::<Vec<_>>()
        .join("|");
    regex::Regex::new(&pattern).map_err(|_| "cannot configure broker credential redaction".into())
}

fn scrub_exact_values(value: &mut Value, scrub: &impl Fn(&str) -> String) {
    match value {
        Value::String(text) => *text = scrub(text),
        Value::Array(items) => items
            .iter_mut()
            .for_each(|item| scrub_exact_values(item, scrub)),
        Value::Object(fields) => {
            for (name, mut value) in std::mem::take(fields) {
                scrub_exact_values(&mut value, scrub);
                fields.insert(scrub(&name), value);
            }
        }
        scalar => {
            let text = scalar.to_string();
            if scrub(&text) != text {
                *scalar = Value::String("[REDACTED]".into());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::DirBuilderExt;

    #[test]
    fn request_parser_bounds_and_validates_framing() {
        let good = b"POST /proxy HTTP/1.1\r\nContent-Length: 1\r\n\r\nx";
        let mut reader = &good[..];
        let request = read_headers(&mut reader).unwrap();
        assert_eq!(request.body_length, 1);
        assert_eq!(reader, b"x");
        for bad in [
            "GET /other HTTP/1.1\r\n\r\n".to_owned(),
            "GET /proxy HTTP/1.1\r\nContent-Length: unknown\r\n\r\n".to_owned(),
            "GET /proxy HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n".to_owned(),
            format!(
                "GET /proxy HTTP/1.1\r\nX-Fixture: {}\r\n\r\n",
                "x".repeat(MAX_HEADERS)
            ),
        ] {
            assert!(read_headers(&mut bad.as_bytes()).is_none());
        }
    }

    #[test]
    fn targets_are_checked_as_the_same_normalized_origin_used_by_the_client() {
        let allowed = vec![
            "https://example.invalid:8443".into(),
            "127.0.0.1:9009".into(),
        ];
        for valid in [
            "https://EXAMPLE.invalid:8443/records",
            "http://127.0.0.1:9009/records",
        ] {
            assert!(target_allowed(valid, &allowed).is_some());
        }
        for denied in [
            "https://example.invalid/records",
            "http://127.0.0.1:9008/records",
        ] {
            assert!(target_allowed(denied, &allowed).is_none());
        }
    }

    #[test]
    fn broker_stop_unlinks_the_socket_and_releases_an_idle_connection() {
        let path = Path::new("/tmp").join(crate::types::new_id("jani-broker-test"));
        fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
        let mut broker = CredentialBroker::start(
            &path,
            vec![],
            BTreeMap::new(),
            Governance::new(Default::default(), "test", "test", "test"),
            Effect::Read,
            Instant::now() + Duration::from_secs(30),
            AuthorityWindow::default(),
        )
        .unwrap();
        let connection = UnixStream::connect(&broker.socket).unwrap();
        let wait = Instant::now() + Duration::from_secs(2);
        while broker.active.lock().unwrap().is_none() && Instant::now() < wait {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(broker.active.lock().unwrap().is_some());
        let start = Instant::now();
        broker.stop();
        assert!(start.elapsed() < LOCAL_TIMEOUT);
        assert!(!broker.socket.exists());
        drop(connection);
        fs::remove_dir(path).unwrap();
    }

    #[test]
    fn established_socket_cannot_start_credential_request_after_expiry() {
        let path = Path::new("/tmp").join(crate::types::new_id("jani-broker-expiry"));
        fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
        let origin = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        origin.set_nonblocking(true).unwrap();
        let url = format!("http://{}", origin.local_addr().unwrap());
        let expires = crate::types::now_unix() + 2;
        let mut broker = CredentialBroker::start(
            &path,
            vec![url.clone()],
            BTreeMap::from([("TOKEN".into(), "fixture-broker-token".into())]),
            Governance::new(Default::default(), "test", "test", "test"),
            Effect::Read,
            Instant::now() + Duration::from_secs(30),
            AuthorityWindow::new(Some(expires), None),
        )
        .unwrap();
        let mut connection = UnixStream::connect(&broker.socket).unwrap();
        connection
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        while broker.active.lock().unwrap().is_none() && crate::types::now_unix() < expires {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(broker.active.lock().unwrap().is_some());
        while crate::types::now_unix() < expires {
            thread::sleep(Duration::from_millis(10));
        }
        write!(connection, "GET /proxy HTTP/1.1\r\nHost: broker\r\nX-Jani-Lease: {}\r\nX-Jani-Target-Url: {url}/fixture\r\n\r\n", broker.lease).unwrap();
        let mut output = Vec::new();
        connection.read_to_end(&mut output).unwrap();
        assert!(output.is_empty());
        assert_eq!(
            origin.accept().unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        broker.stop();
        fs::remove_dir(path).unwrap();
    }

    #[test]
    fn redaction_does_not_rescan_its_own_replacements() {
        let redactor = redaction_pattern(["E", "E", "EXAMPLE"].into_iter()).unwrap();
        assert_eq!(
            redactor.replace_all("EXAMPLE E", "[REDACTED]"),
            "[REDACTED] [REDACTED]"
        );
    }
}
