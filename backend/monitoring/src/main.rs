//! JSMT/1 telemetry-only gateway. No shell, no remote execution, no AI dependency.
//! This first implementation is single-node and requires manually provisioned mTLS.
use jani_jsmt::{identity_ok, validate, VERSION, MAX_BODY};
use rusqlite::{params, Connection, TransactionBehavior};
use rustls::pki_types::{pem::PemObject, CertificateDer, PrivateKeyDer};
use rustls::{RootCertStore, ServerConfig, ServerConnection, StreamOwned};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs;
mod store;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::Path;
use std::sync::{atomic::{AtomicI64, AtomicUsize, Ordering}, Arc};
use std::time::Duration;

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Binding {
    tenant_id: String,
    device_id: String,
    #[serde(default)]
    revoked: bool,
    #[serde(default)]
    expires_unix: Option<i64>,
}
type Registry = HashMap<String, Binding>;
type ErrBox = Box<dyn std::error::Error + Send + Sync>;
type Result<T> = std::result::Result<T, ErrBox>;

fn certs(path: &str) -> Result<Vec<CertificateDer<'static>>> {
    let found = CertificateDer::pem_file_iter(path)?.collect::<std::result::Result<Vec<_>, _>>()?;
    if found.is_empty() { return Err("no certificate in PEM".into()); }
    Ok(found)
}
fn server_tls(ca: &str, cert: &str, key: &str) -> Result<Arc<ServerConfig>> {
    let mut roots = RootCertStore::empty();
    for c in certs(ca)? { roots.add(c)?; }
    let verifier = rustls::server::WebPkiClientVerifier::builder(Arc::new(roots)).build()?;
    let private = PrivateKeyDer::from_pem_file(key)?;
    let config = ServerConfig::builder_with_protocol_versions(&[&rustls::version::TLS13])
        .with_client_cert_verifier(verifier)
        .with_single_cert(certs(cert)?, private)?;
    Ok(Arc::new(config))
}
fn init_db(path: &str) -> Result<()> {
    let conn = Connection::open(path)?;
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA secure_delete=ON;
        CREATE TABLE IF NOT EXISTS accepted(
          tenant TEXT NOT NULL, device TEXT NOT NULL, sequence INTEGER NOT NULL,
          payload_hash TEXT NOT NULL, json TEXT NOT NULL,
          received_unix INTEGER NOT NULL DEFAULT 0,
          PRIMARY KEY(tenant,device,sequence)
        );
        CREATE TABLE IF NOT EXISTS device_cursors(
          tenant TEXT NOT NULL, device TEXT NOT NULL, last_seq INTEGER NOT NULL,
          PRIMARY KEY(tenant,device)
        );")?;
    // Upgrade any existing v1 pilot database without resetting its device cursors.
    let mut has_received=false;
    {
        let mut stmt=conn.prepare("PRAGMA table_info(accepted)")?;
        let rows=stmt.query_map([],|r|r.get::<_,String>(1))?;
        for name in rows { if name? == "received_unix" {has_received=true;} }
    }
    if !has_received {
        conn.execute("ALTER TABLE accepted ADD COLUMN received_unix INTEGER NOT NULL DEFAULT 0",[])?;
        conn.execute("UPDATE accepted SET received_unix=?1 WHERE received_unix=0",params![store::epoch()])?;
    }
    store::schema(&conn)?;
    Ok(())
}
fn persist(db: &str, env: &jani_jsmt::Envelope, canonical: &[u8], hash: &str, rate_per_min: i64) -> Result<(u16, Value)> {
    let mut conn = Connection::open(db)?;
    conn.busy_timeout(Duration::from_secs(5))?;
    conn.execute_batch("PRAGMA synchronous=FULL")?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let old: Option<String> = tx.query_row(
        "SELECT payload_hash FROM accepted WHERE tenant=?1 AND device=?2 AND sequence=?3",
        params![env.tenant_id, env.device_id, env.sequence], |r| r.get(0),
    ).optional()?;
    if let Some(existing) = old {
        tx.commit()?;
        if existing != hash { return Ok((409, json!({"error":"sequence_conflict"}))); }
        return Ok((200, json!({"version":VERSION,"ack_sequence":env.sequence,
            "payload_sha256":hash,"duplicate":true})));
    }
    let previous: Option<i64> = tx.query_row(
        "SELECT last_seq FROM device_cursors WHERE tenant=?1 AND device=?2",
        params![env.tenant_id, env.device_id], |r| r.get(0),
    ).optional()?;
    let expected = previous.unwrap_or(0) + 1;
    if env.sequence != expected {
        tx.rollback()?;
        return Ok((409, json!({"error":"out_of_order","expected_sequence":expected})));
    }
    if !store::rate_limit(&tx,&env.tenant_id,&env.device_id,rate_per_min)? {
        tx.rollback()?;
        return Ok((429,json!({"error":"ingest_rate_limited"})));
    }
    tx.execute("INSERT INTO accepted(tenant,device,sequence,payload_hash,json,received_unix)
        VALUES(?1,?2,?3,?4,?5,?6)",
        params![env.tenant_id,env.device_id,env.sequence,hash,
            std::str::from_utf8(canonical)?,store::epoch()])?;
    tx.execute("INSERT INTO device_cursors VALUES(?1,?2,?3)
        ON CONFLICT(tenant,device) DO UPDATE SET last_seq=excluded.last_seq",
        params![env.tenant_id,env.device_id,env.sequence])?;
    store::project(&tx,env)?;
    tx.commit()?; // Only after durable transaction commit may we ACK.
    Ok((200, json!({"version":VERSION,"ack_sequence":env.sequence,
        "payload_sha256":hash,"duplicate":false})))
}
use rusqlite::OptionalExtension;

fn bounded_line<R: BufRead>(r: &mut R, max: usize) -> std::io::Result<String> {
    let mut bytes = Vec::new();
    loop {
        let slice = r.fill_buf()?;
        if slice.is_empty() { return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof,"header EOF")); }
        let take = slice.iter().position(|b| *b == b'\n').map_or(slice.len(), |v| v + 1);
        let n = take.min(max.saturating_add(1).saturating_sub(bytes.len()));
        if n == 0 { return Err(std::io::Error::new(std::io::ErrorKind::InvalidData,"headers too long")); }
        bytes.extend_from_slice(&slice[..n]);
        r.consume(n);
        if bytes.len() > max { return Err(std::io::Error::new(std::io::ErrorKind::InvalidData,"headers too long")); }
        if bytes.last() == Some(&b'\n') { break; }
    }
    let line = std::str::from_utf8(&bytes)
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidData,"not UTF8"))?;
    Ok(line.trim_end_matches(&['\r','\n'][..]).to_owned())
}
fn write_json<T: Write>(to: &mut T, status: u16, body: &Value) -> Result<()> {
    let bytes = serde_json::to_vec(body)?;
    let reason = match status { 200=>"OK",400=>"Bad Request",403=>"Forbidden",429=>"Too Many Requests",
        404=>"Not Found",409=>"Conflict",413=>"Payload Too Large",
        415=>"Unsupported Media Type",503=>"Service Unavailable",_=>"Error" };
    write!(to, "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n", bytes.len())?;
    to.write_all(&bytes)?;
    to.flush()?;
    Ok(())
}
fn handle(mut socket: StreamOwned<ServerConnection, TcpStream>, registry_file: &str, db: &str, rate_per_min: i64) -> Result<()> {
    let fingerprint = socket.conn.peer_certificates()
        .and_then(|cs| cs.first())
        .map(|cert| hex::encode(Sha256::digest(cert.as_ref())));
    // Re-read registry per request. Revocations take effect on the next request,
    // including on connections established before a registry update.
    let registry=read_registry(registry_file).unwrap_or_default(); // malformed registry fails closed
    let binding=fingerprint.as_ref().and_then(|fp|registry.get(fp))
        .filter(|b| !b.revoked && b.expires_unix.is_none_or(|t| t>store::epoch()));
    let mut reader = BufReader::new(&mut socket);
    let request = bounded_line(&mut reader, 8192)?;
    let parts: Vec<&str> = request.split_whitespace().collect();
    if parts.len() != 3 || parts[2] != "HTTP/1.1" { return Ok(()); }
    let (mut length, mut content_type, mut transfer_encoding) = (None, None, false);
    let mut remaining_header = 16_384usize;
    loop {
        let line = bounded_line(&mut reader, remaining_header.min(8192))?;
        remaining_header = remaining_header.saturating_sub(line.len()+2);
        if remaining_header == 0 { return Ok(()); }
        if line.is_empty() { break; }
        if let Some((k,v)) = line.split_once(':') {
            let name=k.trim().to_ascii_lowercase(); let value=v.trim();
            match name.as_str() {
                "content-length" => {
                    if length.is_some() { return Ok(()); }
                    length = value.parse::<usize>().ok();
                }
                "content-type" => content_type = Some(value.to_owned()),
                "transfer-encoding" => transfer_encoding = true,
                _ => {}
            }
        }
    }
    let (status, response) = if binding.is_none() {
        (403, json!({"error":"unregistered_certificate"}))
    } else if parts[0] == "GET" && parts[1] == "/health" {
        (200, json!({"status":"ready","transport":"mtls-tls13"}))
    } else if parts[0] != "POST" || parts[1] != "/v1/ingest" {
        (404, json!({"error":"not_found"}))
    } else if transfer_encoding || length.is_none_or(|v| v == 0 || v > MAX_BODY) {
        (413, json!({"error":"payload_bounds"}))
    } else if content_type.as_deref() != Some("application/json") {
        (415, json!({"error":"json_required"}))
    } else {
        let mut body = vec![0u8; length.unwrap_or(0)];
        if reader.read_exact(&mut body).is_err() {
            (400, json!({"error":"short_body"}))
        } else {
            match validate(&body) {
                Err(reason) => (400, json!({"error":reason})),
                Ok((env, bytes, digest)) => {
                    let device = binding.expect("binding checked");
                    if env.tenant_id != device.tenant_id || env.device_id != device.device_id {
                        (403, json!({"error":"client_identity_mismatch"}))
                    } else {
                        match persist(db, &env, &bytes, &digest, rate_per_min) {
                            Ok(response) => response,
                            Err(_) => (503, json!({"error":"store_unavailable"})),
                        }
                    }
                }
            }
        }
    };
    drop(reader);
    write_json(&mut socket, status, &response)?;
    Ok(())
}
fn read_registry(path: &str) -> Result<Registry> {
    let metadata=fs::metadata(path)?;
    if metadata.len() > 1_048_576 {return Err("registry exceeds 1 MiB".into());}
    let registry:Registry=serde_json::from_str(&fs::read_to_string(path)?)?;
    if registry.is_empty() || registry.len()>4096 {return Err("invalid registry size".into());}
    for (hash,binding) in &registry {
        if hash.len()!=64 || !hash.bytes().all(|c|c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
            || !identity_ok(&binding.tenant_id) || !identity_ok(&binding.device_id) {
            return Err("invalid registry entry".into());
        }
    }
    Ok(registry)
}
fn required(args: &[String], name: &str) -> Result<String> {
    args.windows(2).find(|pair| pair[0]==name).map(|pair| pair[1].clone())
        .ok_or_else(|| format!("missing {name}").into())
}
fn run() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a=="--version") { println!("jani-gateway {}",env!("CARGO_PKG_VERSION"));return Ok(()); }
    if args.get(1).is_some_and(|a| a=="inspect") {
        let db=required(&args,"--db")?;
        let tenant=required(&args,"--tenant")?;
        println!("{}",store::inspect(&db,&tenant)?);
        return Ok(());
    }
    if args.iter().any(|a| a=="--help") {
        println!("jani-gateway --ca CA.pem --cert gateway.crt --key gateway.key --registry devices.json --db gateway.sqlite [--bind 127.0.0.1:8443] [--allow-remote] [--rate-per-minute 120] [--retain-days 7]\njani-gateway inspect --db gateway.sqlite --tenant TENANT");
        return Ok(());
    }
    let bind: SocketAddr = args.windows(2).find(|a|a[0]=="--bind")
        .map(|a|a[1].clone()).unwrap_or("127.0.0.1:8443".into()).parse()?;
    if !bind.ip().is_loopback() && !args.iter().any(|a|a=="--allow-remote") {
        return Err("non-loopback listening requires --allow-remote".into());
    }
    let rate_per_min:i64=args.windows(2).find(|a|a[0]=="--rate-per-minute")
        .map(|a|a[1].parse()).transpose()?.unwrap_or(120);
    if !(1..=10_000).contains(&rate_per_min) {return Err("rate-per-minute out of range".into());}
    let retain_days:i64=args.windows(2).find(|a|a[0]=="--retain-days")
        .map(|a|a[1].parse()).transpose()?.unwrap_or(7);
    if !(1..=90).contains(&retain_days) {return Err("retain-days out of range".into());}
    let db=required(&args,"--db")?;
    let config=server_tls(&required(&args,"--ca")?,&required(&args,"--cert")?,&required(&args,"--key")?)?;
    let registry_file=required(&args,"--registry")?;
    let _=read_registry(&registry_file)?;
    if let Some(parent) = Path::new(&db).parent() {
        if !parent.as_os_str().is_empty() { fs::create_dir_all(parent)?; }
    }
    init_db(&db)?;
    store::purge(&Connection::open(&db)?,retain_days)?;
    let listener=TcpListener::bind(bind)?;
    eprintln!("jani-gateway listening on {} (mTLS, JSMT/1)",listener.local_addr()?);
    let active=Arc::new(AtomicUsize::new(0));
    let last_purge=Arc::new(AtomicI64::new(store::epoch()));
    for incoming in listener.incoming() {
        let Ok(mut tcp) = incoming else { continue };
        if active.fetch_add(1,Ordering::SeqCst) >= 64 {
            active.fetch_sub(1,Ordering::SeqCst);
            continue;
        }
        let tls=config.clone();let registry_file=registry_file.clone();let db=db.clone();let active=active.clone();
        let last_purge=last_purge.clone();
        std::thread::spawn(move || {
            let _guard = Guard(active);
            let _=tcp.set_read_timeout(Some(Duration::from_secs(10)));
            let _=tcp.set_write_timeout(Some(Duration::from_secs(10)));
            let Ok(mut conn)=ServerConnection::new(tls) else { return };
            while conn.is_handshaking() {
                if conn.complete_io(&mut tcp).is_err() { return; }
            }
            let socket=StreamOwned::new(conn,tcp);
            let _=handle(socket,&registry_file,&db,rate_per_min);
            // Retention cleanup is best-effort and never blocks ACK commit.
            let now=store::epoch();
            let last=last_purge.load(Ordering::Relaxed);
            if now.saturating_sub(last)>=3600 &&
               last_purge.compare_exchange(last,now,Ordering::SeqCst,Ordering::Relaxed).is_ok() {
                if let Ok(conn)=Connection::open(&db) {
                    let _=conn.busy_timeout(Duration::from_millis(100));
                    let _=store::purge(&conn,retain_days);
                }
            }
        });
    }
    Ok(())
}
struct Guard(Arc<AtomicUsize>);
impl Drop for Guard {
    fn drop(&mut self) { self.0.fetch_sub(1,Ordering::SeqCst); }
}
fn main() {
    if let Err(e)=run() { eprintln!("jani-gateway: {e}"); std::process::exit(1); }
}
