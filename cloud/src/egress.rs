//! Outbound network control for workspaces that have no network of their own.
//!
//! A workspace in its own network namespace can reach nothing. The only way out
//! is a Unix socket the gateway listens on. Behind it is an HTTP proxy that
//! allows exactly the destinations the operator named (by default, the model
//! provider) and refuses everything else, including names that resolve to
//! loopback, private or link-local addresses, so it cannot be used to reach
//! the host or the internal network.
//!
//! Inside the sandbox, `jani-cloud sandbox-init` runs a tiny TCP-to-socket relay
//! on 127.0.0.1 so ordinary `HTTPS_PROXY`-aware clients work unchanged.

use anyhow::{bail, Context, Result};
use std::{
    net::{IpAddr, SocketAddr},
    path::Path,
    sync::Arc,
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{lookup_host, TcpListener, TcpStream, UnixListener, UnixStream},
    sync::Semaphore,
};

const MAX_EGRESS_CONNECTIONS: usize = 64;
const MAX_HEAD: usize = 16 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rule {
    host: String,
    /// None means any port.
    port: Option<u16>,
}

#[derive(Clone, Debug, Default)]
pub struct Allow {
    rules: Vec<Rule>,
}

impl Rule {
    pub fn parse(text: &str) -> Result<Rule> {
        let text = text.trim().to_ascii_lowercase();
        let (host, port) = text
            .rsplit_once(':')
            .with_context(|| format!("egress rule {text:?} needs host:port or host:*"))?;
        if host.is_empty() || host.contains('/') || host.contains('*') {
            bail!("egress rule {text:?}: the host must be an exact name or address");
        }
        let port = if port == "*" {
            None
        } else {
            Some(
                port.parse()
                    .with_context(|| format!("egress rule {text:?}: bad port"))?,
            )
        };
        Ok(Rule {
            host: host.trim_matches(['[', ']']).to_string(),
            port,
        })
    }

    fn matches(&self, host: &str, port: u16) -> bool {
        self.host == host && self.port.is_none_or(|p| p == port)
    }

    fn host_is_literal_ip(&self) -> bool {
        self.host.parse::<IpAddr>().is_ok()
    }
}

impl Allow {
    pub fn new(rules: Vec<Rule>) -> Self {
        Self { rules }
    }

    /// The destination a model `--api-base` implies, e.g. `https://api.x.test/v1` → `api.x.test:443`.
    pub fn from_api_base(url: &str) -> Option<Rule> {
        let (scheme, rest) = url.split_once("://")?;
        let authority = rest.split(['/', '?', '#']).next()?;
        let authority = authority.rsplit('@').next()?;
        let default = if scheme == "https" { 443 } else { 80 };
        let (host, port) = match authority.rsplit_once(':') {
            Some((h, p)) if p.chars().all(|c| c.is_ascii_digit()) && !p.is_empty() => {
                (h, p.parse().ok()?)
            }
            _ => (authority, default),
        };
        Some(Rule {
            host: host.trim_matches(['[', ']']).to_ascii_lowercase(),
            port: Some(port),
        })
    }

    pub fn rule_for(&self, host: &str, port: u16) -> Option<&Rule> {
        let host = host.to_ascii_lowercase();
        self.rules.iter().find(|r| r.matches(&host, port))
    }

    pub fn describe(&self) -> String {
        self.rules
            .iter()
            .map(|r| {
                format!(
                    "{}:{}",
                    r.host,
                    r.port.map_or("*".to_string(), |p| p.to_string())
                )
            })
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// Addresses a name must never resolve to unless the operator allowed that exact literal.
pub fn is_internal(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.octets()[0] == 0
                || (v4.octets()[0] == 100 && (64..128).contains(&v4.octets()[1]))
            // CGNAT
        }
        IpAddr::V6(v6) => {
            v6.is_loopback()
                || v6.is_unspecified()
                || (v6.segments()[0] & 0xfe00) == 0xfc00 // unique local
                || (v6.segments()[0] & 0xffc0) == 0xfe80 // link local
                || v6.to_ipv4_mapped().is_some_and(|v4| is_internal(&IpAddr::V4(v4)))
        }
    }
}

async fn read_head(stream: &mut UnixStream) -> std::io::Result<Vec<u8>> {
    let mut head = Vec::with_capacity(1024);
    let mut buf = [0u8; 2048];
    loop {
        let n = stream.read(&mut buf).await?;
        if n == 0 {
            return Err(std::io::ErrorKind::UnexpectedEof.into());
        }
        head.extend_from_slice(&buf[..n]);
        if head.windows(4).any(|w| w == b"\r\n\r\n") {
            return Ok(head);
        }
        if head.len() > MAX_HEAD {
            return Err(std::io::Error::other("request head too large"));
        }
    }
}

async fn refuse(stream: &mut UnixStream, status: &str, why: &str) {
    let body = format!("egress denied: {why}\n");
    let _ = stream
        .write_all(format!("HTTP/1.1 {status}\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes())
        .await;
}

/// Resolve and connect, refusing internal addresses unless the rule is that literal address.
async fn connect(rule: &Rule, host: &str, port: u16) -> Result<TcpStream, String> {
    let addrs: Vec<SocketAddr> = lookup_host((host, port))
        .await
        .map_err(|e| format!("cannot resolve {host}: {e}"))?
        .collect();
    let usable: Vec<SocketAddr> = addrs
        .into_iter()
        .filter(|a| {
            let link_local = match a.ip() {
                IpAddr::V4(v4) => v4.is_link_local(),
                IpAddr::V6(v6) => (v6.segments()[0] & 0xffc0) == 0xfe80,
            };
            !link_local && (rule.host_is_literal_ip() || !is_internal(&a.ip()))
        })
        .collect();
    if usable.is_empty() {
        return Err(format!("{host} resolves only to internal addresses"));
    }
    for addr in usable {
        if let Ok(Ok(stream)) =
            tokio::time::timeout(Duration::from_secs(10), TcpStream::connect(addr)).await
        {
            return Ok(stream);
        }
    }
    Err(format!("cannot connect to {host}:{port}"))
}

fn split_authority(authority: &str, default_port: u16) -> Option<(String, u16)> {
    let authority = authority.rsplit('@').next()?;
    if let Some(rest) = authority.strip_prefix('[') {
        let (host, tail) = rest.split_once(']')?;
        let port = tail
            .strip_prefix(':')
            .map_or(Some(default_port), |p| p.parse().ok())?;
        return Some((host.to_ascii_lowercase(), port));
    }
    match authority.rsplit_once(':') {
        Some((h, p)) if !h.contains(':') => Some((h.to_ascii_lowercase(), p.parse().ok()?)),
        _ => Some((authority.to_ascii_lowercase(), default_port)),
    }
}

async fn handle(mut client: UnixStream, allow: Arc<Allow>, who: Arc<String>) {
    let head = match tokio::time::timeout(Duration::from_secs(15), read_head(&mut client)).await {
        Ok(Ok(h)) => h,
        _ => return,
    };
    let split = head
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .unwrap_or(head.len());
    let text = String::from_utf8_lossy(&head[..split]).into_owned();
    let mut lines = text.split("\r\n");
    let request_line = lines.next().unwrap_or("");
    let mut parts = request_line.split_whitespace();
    let (method, target, version) = (
        parts.next().unwrap_or(""),
        parts.next().unwrap_or(""),
        parts.next().unwrap_or(""),
    );
    if parts.next().is_some() || !matches!(version, "HTTP/1.1" | "HTTP/1.0") {
        return refuse(&mut client, "400 Bad Request", "malformed request line").await;
    }
    let deny = |host: &str, port: u16| tracing::warn!(workspace = %who, destination = %format!("{host}:{port}"), "egress denied");
    if method.eq_ignore_ascii_case("CONNECT") {
        let Some((host, port)) = split_authority(target, 443) else {
            return refuse(&mut client, "400 Bad Request", "malformed CONNECT target").await;
        };
        let Some(rule) = allow.rule_for(&host, port) else {
            deny(&host, port);
            return refuse(
                &mut client,
                "403 Forbidden",
                &format!("{host}:{port} is not allowed"),
            )
            .await;
        };
        match connect(rule, &host, port).await {
            Ok(mut upstream) => {
                if client
                    .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                    .await
                    .is_err()
                {
                    return;
                }
                // Anything the client sent after the head belongs to the tunnel.
                let extra = &head[(split + 4).min(head.len())..];
                if !extra.is_empty() && upstream.write_all(extra).await.is_err() {
                    return;
                }
                let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
            }
            Err(why) => {
                tracing::warn!(workspace = %who, %why, "egress connect failed");
                refuse(&mut client, "502 Bad Gateway", &why).await
            }
        }
        return;
    }
    // Plain HTTP in absolute form: METHOD http://host[:port]/path HTTP/1.1
    let Some(rest) = target.strip_prefix("http://") else {
        return refuse(
            &mut client,
            "405 Method Not Allowed",
            "only CONNECT and http:// requests are proxied",
        )
        .await;
    };
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let Some((host, port)) = split_authority(authority, 80) else {
        return refuse(&mut client, "400 Bad Request", "malformed URL").await;
    };
    let raw_headers: Vec<&str> = lines.collect();
    let mut seen_cl = false;
    for line in &raw_headers {
        let lower = line.to_ascii_lowercase();
        if lower.starts_with("transfer-encoding:") {
            return refuse(
                &mut client,
                "501 Not Implemented",
                "transfer-encoding not supported on egress proxy",
            )
            .await;
        }
        if lower.starts_with("content-length:") {
            if seen_cl {
                return refuse(&mut client, "400 Bad Request", "duplicate content-length").await;
            }
            seen_cl = true;
        }
    }
    let Some(rule) = allow.rule_for(&host, port) else {
        deny(&host, port);
        return refuse(
            &mut client,
            "403 Forbidden",
            &format!("{host}:{port} is not allowed"),
        )
        .await;
    };
    match connect(rule, &host, port).await {
        Ok(mut upstream) => {
            let host_header = if port == 80 {
                host.clone()
            } else {
                format!("{host}:{port}")
            };
            let mut out = format!("{method} {path} HTTP/1.1\r\nHost: {host_header}\r\n");
            for line in raw_headers {
                let lower = line.to_ascii_lowercase();
                if lower.starts_with("proxy-")
                    || lower.starts_with("connection:")
                    || lower.starts_with("host:")
                {
                    continue;
                }
                out.push_str(line);
                out.push_str("\r\n");
            }
            out.push_str("Connection: close\r\n\r\n");
            if upstream.write_all(out.as_bytes()).await.is_err() {
                return;
            }
            let extra = &head[(split + 4).min(head.len())..];
            if !extra.is_empty() && upstream.write_all(extra).await.is_err() {
                return;
            }
            let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
        }
        Err(why) => {
            tracing::warn!(workspace = %who, %why, "egress connect failed");
            refuse(&mut client, "502 Bad Gateway", &why).await
        }
    }
}

/// Serve one workspace's egress socket until the task is aborted.
pub fn spawn(socket: &Path, allow: Arc<Allow>, who: String) -> Result<tokio::task::JoinHandle<()>> {
    let _ = std::fs::remove_file(socket);
    let listener =
        UnixListener::bind(socket).with_context(|| format!("cannot bind {}", socket.display()))?;
    // Reachable only through the directory it lives in (root-only) and the one
    // sandbox it is bind-mounted into, so the socket itself can be open.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o666))?;
    }
    let who = Arc::new(who);
    let permits = Arc::new(Semaphore::new(MAX_EGRESS_CONNECTIONS));
    Ok(tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((stream, _)) => {
                    if let Ok(permit) = permits.clone().try_acquire_owned() {
                        let allow = allow.clone();
                        let who = who.clone();
                        tokio::spawn(async move {
                            let _permit = permit;
                            handle(stream, allow, who).await;
                        });
                    }
                }
                Err(_) => tokio::time::sleep(Duration::from_millis(50)).await,
            }
        }
    }))
}

/// Runs inside the sandbox: relay 127.0.0.1:PORT to the egress socket, run the
/// command with proxy settings, and exit with its status.
pub fn sandbox_init(socket: &str, listen: &str, command: &[String]) -> Result<i32> {
    use std::os::unix::process::CommandExt;
    if command.is_empty() {
        bail!("sandbox-init needs a command");
    }
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    let listener = rt
        .block_on(TcpListener::bind(listen))
        .with_context(|| format!("cannot listen on {listen}"))?;
    let socket = socket.to_string();
    rt.spawn(async move {
        loop {
            let Ok((mut tcp, _)) = listener.accept().await else {
                continue;
            };
            let socket = socket.clone();
            tokio::spawn(async move {
                if let Ok(mut unix) = UnixStream::connect(&socket).await {
                    let _ = tokio::io::copy_bidirectional(&mut tcp, &mut unix).await;
                }
            });
        }
    });
    let proxy = format!("http://{listen}");
    let mut cmd = std::process::Command::new(&command[0]);
    cmd.args(&command[1..])
        .env("HTTP_PROXY", &proxy)
        .env("HTTPS_PROXY", &proxy)
        .env("http_proxy", &proxy)
        .env("https_proxy", &proxy)
        .env_remove("NO_PROXY")
        .env_remove("no_proxy")
        .env_remove("ALL_PROXY");
    #[cfg(target_os = "linux")]
    unsafe {
        cmd.pre_exec(|| {
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = cmd
        .spawn()
        .with_context(|| format!("cannot run {}", command[0]))?;
    let status = child.wait()?;
    Ok(status.code().unwrap_or(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rules_parse_exactly_and_refuse_wildcard_hosts() {
        assert_eq!(
            Rule::parse("API.Example.test:443").unwrap(),
            Rule {
                host: "api.example.test".into(),
                port: Some(443)
            }
        );
        assert_eq!(Rule::parse("x.test:*").unwrap().port, None);
        assert!(Rule::parse("*.example.test:443").is_err());
        assert!(Rule::parse("noport").is_err());
        assert!(Rule::parse("a/b:1").is_err());
    }

    #[test]
    fn api_base_implies_the_provider_destination() {
        let r = Allow::from_api_base("https://api.model.test/v1").unwrap();
        assert_eq!((r.host.as_str(), r.port), ("api.model.test", Some(443)));
        let r = Allow::from_api_base("http://127.0.0.1:18081/v1").unwrap();
        assert_eq!((r.host.as_str(), r.port), ("127.0.0.1", Some(18081)));
        let r = Allow::from_api_base("https://user:pw@host.test:8443/x").unwrap();
        assert_eq!((r.host.as_str(), r.port), ("host.test", Some(8443)));
        assert!(Allow::from_api_base("not a url").is_none());
    }

    #[test]
    fn matching_is_exact_on_host_and_port() {
        let a = Allow::new(vec![
            Rule::parse("api.test:443").unwrap(),
            Rule::parse("other.test:*").unwrap(),
        ]);
        assert!(a.rule_for("API.test", 443).is_some());
        assert!(a.rule_for("api.test", 80).is_none());
        assert!(a.rule_for("evil.api.test", 443).is_none());
        assert!(a.rule_for("other.test", 9).is_some());
    }

    #[test]
    fn internal_addresses_are_recognised() {
        for ip in [
            "127.0.0.1",
            "10.1.2.3",
            "192.168.0.1",
            "172.16.5.5",
            "169.254.169.254",
            "0.0.0.0",
            "100.64.0.1",
            "::1",
            "fc00::1",
            "fe80::1",
            "::ffff:10.0.0.1",
        ] {
            assert!(is_internal(&ip.parse().unwrap()), "{ip}");
        }
        for ip in ["8.8.8.8", "1.1.1.1", "2606:4700::1111"] {
            assert!(!is_internal(&ip.parse().unwrap()), "{ip}");
        }
    }

    #[test]
    fn authorities_split_with_defaults_and_brackets() {
        assert_eq!(
            split_authority("api.test:8443", 443),
            Some(("api.test".into(), 8443))
        );
        assert_eq!(
            split_authority("api.test", 443),
            Some(("api.test".into(), 443))
        );
        assert_eq!(split_authority("[::1]:80", 443), Some(("::1".into(), 80)));
        assert_eq!(
            split_authority("user@api.test:1", 443),
            Some(("api.test".into(), 1))
        );
        assert_eq!(split_authority("api.test:notaport", 443), None);
    }
}
