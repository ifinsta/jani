//! Jani Probe: read-only explicitly enrolled client, bounded metrics/log source collection,
//! durable local queue and validated TLS1.3/mTLS JSMT/1 delivery.
//! Intentionally NO command, policy-update, self-update or shell execution interface.
use jani_jsmt::{create_envelope, identity_ok, Ack, Coverage, Log, Metric, VERSION};
use regex::Regex;
use reqwest::blocking::Client;
use reqwest::{Certificate, Identity, Url};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::Deserialize;
use std::collections::{BTreeMap, HashMap};
use std::fs::{self, File};
#[cfg(unix)]
use std::fs::OpenOptions;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
const MAX_QUEUE: i64 = 1000;
const MAX_LINES: usize = 200;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Source { service: String, path: String }
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceFile { services: Vec<Source> }
#[derive(Default)]
struct Observations {
    metrics: Vec<Metric>,
    logs: Vec<Log>,
    coverage: Vec<Coverage>,
    offsets: Vec<(String, i64)>,
}
fn now_nanos() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos().min(i64::MAX as u128) as i64
}
fn observed_at() -> String {
    // For full timestamp semantic compatibility, use UTC in a future protocol revision.
    // v1 validator constrains length and identity only.
    let seconds=SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs() as i64;
    let days=seconds/86400;
    let secs=seconds%86400;
    let z=days+719468;
    let era=z/146097;
    let doe=z-era*146097;
    let yoe=(doe-doe/1460+doe/36524-doe/146096)/365;
    let y=yoe+era*400;
    let doy=doe-(365*yoe+yoe/4-yoe/100);
    let mp=(5*doy+2)/153;
    let d=doy-(153*mp+2)/5+1;
    let m=mp+if mp<10 {3}else{-9};
    let year=y+if m<=2 {1}else{0};
    format!("{year:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",secs/3600,(secs%3600)/60,secs%60)
}
fn metric(name: &str, value: f64, attrs: BTreeMap<String,String>) -> Metric {
    Metric { name:name.into(), unit:jani_jsmt::metric_unit(name).unwrap_or("1").into(), value, attributes:attrs }
}
fn linux_metrics(out: &mut Observations) {
    #[cfg(target_os="linux")] {
        if let Ok(raw)=fs::read_to_string("/proc/uptime") {
            if let Some(n)=raw.split_whitespace().next().and_then(|n|n.parse::<f64>().ok()) {
                out.metrics.push(metric("system.uptime",n,BTreeMap::new()));
            }
        }
        if let Ok(raw)=fs::read_to_string("/proc/meminfo") {
            let mut values=HashMap::new();
            for line in raw.lines() {
                if let Some((key, rest))=line.split_once(':') {
                    if let Some(value)=rest.split_whitespace().next().and_then(|v|v.parse::<u64>().ok()) {
                        values.insert(key.to_string(),value*1024);
                    }
                }
            }
            if let (Some(total),Some(available))=(values.get("MemTotal"),values.get("MemAvailable")) {
                let used=total.saturating_sub(*available);
                out.metrics.push(metric("system.memory.usage",used as f64,
                    BTreeMap::from([("system.memory.state".into(),"used".into())])));
                out.metrics.push(metric("system.memory.usage",*available as f64,
                    BTreeMap::from([("system.memory.state".into(),"free".into())])));
            }
        }
        if let Ok(iter)=fs::read_dir("/proc") {
            let count=iter.flatten().filter(|item|item.file_name().to_str()
                .is_some_and(|name|!name.is_empty() && name.bytes().all(|b|b.is_ascii_digit()))).count();
            out.metrics.push(metric("system.process.count",count as f64,BTreeMap::new()));
        }
    }
}
fn scrub(line: &str) -> String {
    // Minimal recognized-secret redaction, not sufficient to certify comprehensive DLP.
    let regex=Regex::new(r"(?i)(authorization\s*[:=]\s*bearer\s+|(?:password|api[_-]?key|token|secret)\s*[:=]\s*)([^\s,;]+)")
        .expect("constant regex");
    regex.replace_all(line,"1[REDACTED]").chars().take(4096).collect()
}
fn coverage(service: &str, source: &str, status: &str, reason: &str) -> Coverage {
    Coverage { service:service.into(), source:source.into(), status:status.into(),
        reason:reason.chars().take(160).collect() }
}
fn collect(sources: &[Source], cursors: &HashMap<String,i64>) -> Result<Observations> {
    let mut out=Observations::default();
    for src in sources {
        if src.service.is_empty() || src.service.len()>100 || !src.service.bytes().all(|b|
            b.is_ascii_alphanumeric() || matches!(b,b'.'|b'_'|b'-'|b'@')) {
            return Err("invalid source service ID".into());
        }
        if src.path.is_empty() || src.path.len()>140 || !Path::new(&src.path).is_absolute() {
            return Err("source paths must be absolute and <=140 bytes".into());
        }
        let source=format!("file:{}",src.path);
        if source.len()>160 { return Err("source identifier too long".into()); }
        let previous=*cursors.get(&source).unwrap_or(&0);
        let metadata=match fs::symlink_metadata(&src.path) {
            Ok(m)=>m,
            Err(e)=>{
                let status=if e.kind()==std::io::ErrorKind::PermissionDenied {"permission_denied"} else {"missing"};
                out.coverage.push(coverage(&src.service,&source,status,"source inaccessible"));
                out.metrics.push(metric("jani.service.log.covered",0.0,
                    BTreeMap::from([("service.name".into(),src.service.clone())])));
                continue;
            }
        };
        if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
            out.coverage.push(coverage(&src.service,&source,"unsupported","not a regular file or symlink"));
            out.metrics.push(metric("jani.service.log.covered",0.0,
                BTreeMap::from([("service.name".into(),src.service.clone())])));
            continue;
        }
        #[cfg(unix)]
        let open_attempt={
            use std::os::unix::fs::OpenOptionsExt;
            OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW).open(&src.path)
        };
        #[cfg(not(unix))]
        let open_attempt=File::open(&src.path);
        let mut file=match open_attempt {
            Ok(f)=>f,
            Err(_)=>{
                out.coverage.push(coverage(&src.service,&source,"permission_denied","file cannot be opened"));
                out.metrics.push(metric("jani.service.log.covered",0.0,
                    BTreeMap::from([("service.name".into(),src.service.clone())])));
                continue;
            }
        };
        let rotated_or_truncated=metadata.len() < previous.max(0) as u64;
        let begin=if rotated_or_truncated {0} else {previous.max(0) as u64};
        file.seek(SeekFrom::Start(begin))?;
        // Bounded read. Cursor advances only to the last complete newline.
        let mut chunk=Vec::new();
        file.take(128*1024).read_to_end(&mut chunk)?;
        let bytes_read=chunk.len();
        let finished=chunk.iter().rposition(|b|*b==b'\n').map(|p|p+1).unwrap_or(0);
        let complete=&chunk[..finished];
        let contents=String::from_utf8_lossy(complete);
        let mut count=0usize;let mut truncated=false;
        for line in contents.split_terminator('\n') {
            if out.logs.len() >= MAX_LINES { return Err("log batch exceeds 200; cursors unchanged".into()); }
            truncated |= line.chars().count()>4096;
            let mut attrs=BTreeMap::new();
            attrs.insert("jani.log.source".into(),source.chars().take(100).collect());
            out.logs.push(Log {time_unix_nano:now_nanos(),severity_text:"INFO".into(),
                body:scrub(line.trim_end_matches('\r')),resource:BTreeMap::from([("service.name".into(),src.service.clone())]),
                attributes:attrs});
            count+=1;
        }
        let invalid_utf8=std::str::from_utf8(complete).is_err();
        let partial=rotated_or_truncated
            || (bytes_read==128*1024 && (begin+bytes_read as u64)<metadata.len())
            || truncated || invalid_utf8 || (finished==0 && bytes_read>0);
        out.coverage.push(coverage(&src.service,&source,
            if partial {"partial"} else {"covered"},
            if rotated_or_truncated {"source truncated; earlier log loss possible"}
            else if truncated {"log line exceeded 4096 chars; truncated"}
            else if invalid_utf8 {"non-UTF8 input decoded with replacement"}
            else if partial {"backlog or incomplete line remains"} else {""}));
        out.metrics.push(metric("jani.service.log.covered",if partial {0.0} else {1.0},
            BTreeMap::from([("service.name".into(),src.service.clone())])));
        out.offsets.push((source,(begin+finished as u64) as i64));
        let _=count;
    }
    linux_metrics(&mut out);
    Ok(out)
}
#[cfg(target_os="linux")]
fn discover_gap(out: &mut Observations, sources: &[Source]) {
    // Enumerate systemd service names, but mark unconfigured services as unsupported.
    // Do not claim all services have logs merely because the journal exists.
    let result=std::process::Command::new("systemctl").args(
        ["list-units","--type=service","--all","--no-legend","--no-pager","--plain"])
        .output();
    let Ok(output)=result else {
        out.coverage.push(coverage("host-discovery","systemd","unsupported","service inventory unavailable"));
        return;
    };
    if !output.status.success() {
        out.coverage.push(coverage("host-discovery","systemd","unsupported","systemd inventory failed"));
        return;
    }
    let stdout=String::from_utf8_lossy(&output.stdout);
    let names: Vec<&str>=stdout.lines().collect();
    let max_available=256usize.saturating_sub(out.coverage.len()+1);
    if names.len()>max_available {
        out.coverage.push(coverage("host-discovery","systemd","partial","service inventory exceeds envelope limit"));
    }
    for line in names.into_iter().take(max_available) {
        if out.coverage.len()>=256 { break; }
        let name=line.split_whitespace().next().unwrap_or("").trim_end_matches(".service");
        if name.is_empty() || sources.iter().any(|s|s.service==name) {continue;}
        out.coverage.push(coverage(name,"systemd","unsupported","service log adapter not configured"));
    }
}
#[cfg(not(target_os="linux"))]
fn discover_gap(out: &mut Observations, _sources: &[Source]) {
    if out.coverage.len()<256 {
        out.coverage.push(coverage("host-discovery","os-adapter","unsupported","automatic OS service discovery not implemented"));
    }
}
fn db(path: &str) -> Result<Connection> {
    if let Some(parent)=Path::new(path).parent() {
        if !parent.as_os_str().is_empty() { fs::create_dir_all(parent)?; }
    }
    let new_file=!Path::new(path).exists();
    let conn=Connection::open(path)?;
    #[cfg(unix)] {
        use std::os::unix::fs::PermissionsExt;
        if new_file { fs::set_permissions(path,fs::Permissions::from_mode(0o600))?; }
    }
    #[cfg(not(unix))]
    let _=new_file;
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;
       CREATE TABLE IF NOT EXISTS pending(seq INTEGER PRIMARY KEY,payload BLOB NOT NULL,hash TEXT NOT NULL);
       CREATE TABLE IF NOT EXISTS meta(key TEXT PRIMARY KEY,value TEXT NOT NULL);
       CREATE TABLE IF NOT EXISTS offsets(source TEXT PRIMARY KEY,value INTEGER NOT NULL);")?;
    Ok(conn)
}
fn enqueue(path: &str,tenant: &str,device: &str,sources: &[Source]) -> Result<i64> {
    let mut conn=db(path)?;
    let tx=conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let count:i64=tx.query_row("SELECT COUNT(*) FROM pending",[],|r|r.get(0))?;
    if count>=MAX_QUEUE {return Err("spool full; refusing to lose logs".into());}
    let mut cursors=HashMap::new();
    { let mut stmt=tx.prepare("SELECT source,value FROM offsets")?;
      let rows=stmt.query_map([],|r|Ok((r.get::<_,String>(0)?,r.get::<_,i64>(1)?)))?;
      for pair in rows {let(k,v)=pair?;cursors.insert(k,v);}
    }
    let mut o=collect(sources,&cursors)?;
    discover_gap(&mut o,sources);
    let seq:Option<i64>=tx.query_row("SELECT value FROM meta WHERE key='sequence'",[],|r| {
        let s:String=r.get(0)?; Ok(s.parse::<i64>().unwrap_or(0))
    }).optional()?;
    let seq=seq.unwrap_or(0)+1;
    let (payload,hash)=create_envelope(tenant,device,seq,observed_at(),o.metrics,o.logs,o.coverage)
        .map_err(|e|format!("invalid normalized telemetry: {e}"))?;
    tx.execute("INSERT INTO pending VALUES(?1,?2,?3)",params![seq,payload,hash])?;
    tx.execute("INSERT INTO meta(key,value) VALUES('sequence',?1)
        ON CONFLICT(key) DO UPDATE SET value=excluded.value",params![seq.to_string()])?;
    for (source,pos) in o.offsets {
        tx.execute("INSERT INTO offsets VALUES(?1,?2)
           ON CONFLICT(source) DO UPDATE SET value=excluded.value",params![source,pos])?;
    }
    tx.commit()?;
    Ok(seq)
}
fn client(ca: &str,cert: &str,key: &str) -> Result<Client> {
    let root=Certificate::from_pem(&fs::read(ca)?)?;
    let mut identity=fs::read(cert)?;
    identity.extend_from_slice(b"\n");
    identity.extend_from_slice(&fs::read(key)?);
    let id=Identity::from_pem(&identity)?;
    Ok(Client::builder()
       .https_only(true).http1_only()
       .min_tls_version(reqwest::tls::Version::TLS_1_3)
       .redirect(reqwest::redirect::Policy::none())
       .no_proxy()
       .tls_built_in_root_certs(false)
       .add_root_certificate(root).identity(id)
       .timeout(Duration::from_secs(10)).build()?)
}
fn flush(conn: &mut Connection, http: &Client, origin: &str) -> Result<usize> {
    let mut stmt=conn.prepare("SELECT seq,payload,hash FROM pending ORDER BY seq LIMIT 32")?;
    let rows=stmt.query_map([],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,Vec<u8>>(1)?,r.get::<_,String>(2)?)))?;
    let pending=rows.collect::<rusqlite::Result<Vec<_>>>()?;
    drop(stmt);
    let mut sent=0;
    for (seq,payload,hash) in pending {
        let response=http.post(format!("{origin}/v1/ingest"))
            .header("Content-Type","application/json")
            .body(payload).send()?;
        if !response.status().is_success() {
            return Err(format!("gateway refused telemetry: HTTP {}",response.status()).into());
        }
        let ack:Ack=response.json()?;
        if ack.version!=VERSION || ack.ack_sequence!=seq || ack.payload_sha256!=hash {
            return Err("unauthenticated or mismatched application ACK".into());
        }
        conn.execute("DELETE FROM pending WHERE seq=?1 AND hash=?2",params![seq,hash])?;
        sent+=1;
    }
    Ok(sent)
}
fn required(args: &[String],flag: &str) -> Result<String> {
    args.windows(2).find(|w|w[0]==flag).map(|w|w[1].clone())
        .ok_or_else(||format!("missing {flag}").into())
}
fn run() -> Result<()> {
    let args:Vec<String>=std::env::args().collect();
    if args.iter().any(|a|a=="--version") { println!("jani-probe {}",env!("CARGO_PKG_VERSION"));return Ok(()); }
    if args.iter().any(|a|a=="--help") {
        println!("jani-probe --gateway https://host:port --tenant ID --device ID --spool state.sqlite --sources sources.json --ca CA.pem --cert device.crt --key device.key [--watch]");
        return Ok(());
    }
    let gateway=required(&args,"--gateway")?;
    let uri=Url::parse(&gateway)?;
    if uri.scheme()!="https" || uri.username()!="" || uri.password().is_some()
        || uri.path()!="/" || uri.query().is_some() || uri.fragment().is_some() {
        return Err("JSMT requires a clean HTTPS origin".into());
    }
    let tenant=required(&args,"--tenant")?;let device=required(&args,"--device")?;
    if !identity_ok(&tenant) || !identity_ok(&device) { return Err("invalid tenant/device".into()); }
    let spool=required(&args,"--spool")?;
    let config:SourceFile=serde_json::from_str(&fs::read_to_string(required(&args,"--sources")?)?)?;
    if config.services.len()>240 {return Err("too many explicitly configured services".into());}
    let http=client(&required(&args,"--ca")?,&required(&args,"--cert")?,&required(&args,"--key")?)?;
    let watch=args.iter().any(|a|a=="--watch");
    let mut conn=db(&spool)?;
    loop {
        // Drain old durable records *before* enqueueing. Saturated offline queues
        // must recover on reconnect instead of permanently failing collection.
        if let Err(e)=flush(&mut conn,&http,gateway.trim_end_matches('/')) {
            eprintln!("jani-probe: old queued telemetry not yet delivered: {e}");
        }
        match enqueue(&spool,&tenant,&device,&config.services) {
            Ok(seq)=> {
                match flush(&mut conn,&http,gateway.trim_end_matches('/')) {
                    Ok(sent)=>eprintln!("jani-probe: seq={seq}, delivered={sent}"),
                    Err(e)=>{
                        eprintln!("jani-probe: delivery unavailable; telemetry retained: {e}");
                        if !watch { return Err(e); }
                    }
                }
            }
            Err(e)=>{
                eprintln!("jani-probe: collection paused without dropping data: {e}");
                if !watch {return Err(e);}
            }
        }
        if !watch {break;}
        std::thread::sleep(Duration::from_secs(30));
    }
    Ok(())
}
fn main(){
    if let Err(e)=run(){eprintln!("jani-probe: {e}");std::process::exit(1);}
}
