//! jani-cloud: a prototype single-container front end for Jani.
//!
//! People sign in with ifinsta (`siwi`, against ifinsta accounts). Each signed-in
//! person is proxied to their own `jani ui` process. The browser never sees that
//! process's token and cannot reach any other person's.

mod approvals;
mod auth;
mod cgroup;
mod egress;
mod proxy;
mod workspace;

use anyhow::{Context, Result};
use auth::{MaybePerson, Person};
use axum::{
    extract::{FromRef, Request, State},
    http::{header, HeaderValue, StatusCode},
    response::{Html, IntoResponse, Redirect, Response},
    routing::{get, post},
    Json, Router,
};
use cgroup::{Limits, Mode};
use serde::Deserialize;
use serde_json::json;
use siwi::{Config, MemoryStore, Siwi};
use std::{net::SocketAddr, path::PathBuf, sync::Arc, time::Duration};
use workspace::{Failure, Manager, NetMode, Sandbox, Settings};

pub struct App {
    pub manager: Arc<Manager>,
    /// Exact origin of the registered redirect URI; the only Origin we accept.
    pub origin: String,
    pub tenant: String,
    pub allow_consent_proxy: bool,
    pub intake_token: Option<String>,
    pub approvers: approvals::Approvers,
    pub profiles: approvals::Profiles,
    pub data_dir: PathBuf,
    pub self_bin: PathBuf,
    pub uid_isolation: bool,
}

#[derive(Clone, FromRef)]
struct AppState {
    siwi: Arc<Siwi>,
    app: Arc<App>,
}

fn env_list(name: &str, default: &str) -> Vec<String> {
    std::env::var(name)
        .unwrap_or_else(|_| default.into())
        .split([',', ' '])
        .filter(|s| !s.trim().is_empty())
        .map(|s| s.trim().to_string())
        .collect()
}

fn settings_from_env() -> Result<Settings> {
    let sandbox = match std::env::var("JANI_CLOUD_SANDBOX").as_deref() {
        Ok("off") => Sandbox::Off,
        Ok("auto") => Sandbox::Auto,
        Ok("bwrap") | Err(_) => Sandbox::Bwrap,
        Ok(other) => anyhow::bail!("JANI_CLOUD_SANDBOX must be bwrap, auto or off, not {other:?}"),
    };
    let net = match std::env::var("JANI_CLOUD_NET").as_deref() {
        Ok("shared") => NetMode::Shared,
        Ok("isolated") => NetMode::Isolated,
        Ok("auto") | Err(_) => NetMode::Auto,
        Ok(other) => {
            anyhow::bail!("JANI_CLOUD_NET must be auto, isolated or shared, not {other:?}")
        }
    };
    let quotas = match std::env::var("JANI_CLOUD_QUOTAS").as_deref() {
        Ok("require") => Mode::Require,
        Ok("off") => Mode::Off,
        Ok("auto") | Err(_) => Mode::Auto,
        Ok(other) => anyhow::bail!("JANI_CLOUD_QUOTAS must be require, auto or off, not {other:?}"),
    };
    let number = |name: &str, default: u64| -> Result<u64> {
        match std::env::var(name) {
            Ok(v) => v
                .parse()
                .with_context(|| format!("{name} must be a number")),
            Err(_) => Ok(default),
        }
    };
    // 0 means "no limit".
    let limit = |name: &str, default: u64| -> Result<Option<u64>> {
        Ok(Some(number(name, default)?).filter(|v| *v > 0))
    };
    let cpus = match std::env::var("JANI_CLOUD_LIMIT_CPUS") {
        Ok(v) => Some(
            v.parse::<f64>()
                .with_context(|| "JANI_CLOUD_LIMIT_CPUS must be a number")?,
        )
        .filter(|v| *v > 0.0),
        Err(_) => None,
    };
    Ok(Settings {
        data_dir: PathBuf::from(
            std::env::var("JANI_CLOUD_DATA").unwrap_or_else(|_| "./data".into()),
        ),
        jani_bin: PathBuf::from(
            std::env::var("JANI_CLOUD_JANI_BIN").unwrap_or_else(|_| "jani".into()),
        ),
        self_bin: std::env::current_exe().context("cannot find this executable")?,
        jani_args: std::env::var("JANI_CLOUD_JANI_ARGS")
            .unwrap_or_default()
            .split_whitespace()
            .map(str::to_string)
            .collect(),
        passthrough_env: env_list(
            "JANI_CLOUD_PASSTHROUGH_ENV",
            "JANI_API_KEY,JANI_API_BASE,JANI_MODEL",
        ),
        template: std::env::var("JANI_CLOUD_TEMPLATE").ok().map(PathBuf::from),
        settings_file: std::env::var("JANI_CLOUD_SETTINGS").ok().map(PathBuf::from),
        max_active: number("JANI_CLOUD_MAX_ACTIVE", 8)? as usize,
        idle_ttl: Duration::from_secs(number("JANI_CLOUD_IDLE_SECONDS", 1800)?),
        sandbox,
        uid_base: match std::env::var("JANI_CLOUD_UID_BASE") {
            Ok(v) => Some(v.parse().context("JANI_CLOUD_UID_BASE must be a number")?),
            Err(_) => None,
        },
        net,
        egress_allow: env_list("JANI_CLOUD_EGRESS_ALLOW", ""),
        limits: Limits {
            memory_mb: limit("JANI_CLOUD_LIMIT_MEMORY_MB", 1024)?,
            pids: limit("JANI_CLOUD_LIMIT_PIDS", 256)?,
            cpus,
            open_files: limit("JANI_CLOUD_LIMIT_OPEN_FILES", 1024)?,
            disk_mb: limit("JANI_CLOUD_LIMIT_DISK_MB", 2048)?,
        },
        quotas,
        disk_check: Duration::from_secs(number("JANI_CLOUD_DISK_CHECK_SECONDS", 10)?.max(1)),
    })
}

#[cfg(unix)]
fn is_root() -> bool {
    // SAFETY-free check: root owns /proc/self.
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata("/proc/self")
        .map(|m| m.uid() == 0)
        .unwrap_or(false)
}
#[cfg(not(unix))]
fn is_root() -> bool {
    false
}

fn main() -> Result<()> {
    // `jani-cloud sandbox-init ...` runs inside a workspace's sandbox as its network relay.
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("sandbox-init") {
        return sandbox_init(&args[2..]);
    }
    // `jani-cloud decide-approval ...` runs as the requester's own Unix user.
    if args.get(1).map(String::as_str) == Some("decide-approval") {
        return approvals::helper(&args[2..]);
    }
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(run())
}

fn sandbox_init(args: &[String]) -> Result<()> {
    let (mut socket, mut listen) = (None, None);
    let mut it = args.iter();
    let mut command: Vec<String> = Vec::new();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--socket" => socket = it.next().cloned(),
            "--listen" => listen = it.next().cloned(),
            "--" => {
                command = it.cloned().collect();
                break;
            }
            other => anyhow::bail!("sandbox-init: unexpected argument {other:?}"),
        }
    }
    let code = egress::sandbox_init(
        &socket.context("sandbox-init needs --socket")?,
        &listen.context("sandbox-init needs --listen")?,
        &command,
    )?;
    std::process::exit(code);
}

async fn run() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    let config = Config::from_env()?;
    let origin = config.redirect_uri.origin().ascii_serialization();
    let settings = settings_from_env()?;
    let plan = settings.plan(is_root())?;
    std::fs::create_dir_all(&settings.data_dir)?;
    let _instance_lock = {
        let lock_file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(settings.data_dir.join(".lock"))
            .context("cannot open jani-cloud startup lock")?;
        lock_file
            .try_lock()
            .context("another jani-cloud instance already owns data_dir")?;
        lock_file
    };
    tracing::info!(
        sandbox = ?plan.sandbox,
        uid_isolation = settings.uid_base.is_some(),
        isolated_network = plan.isolated_net,
        egress_allow = %plan.egress.describe(),
        max_active = settings.max_active,
        "workspace isolation"
    );
    for note in &plan.notes {
        tracing::warn!("{note}");
    }
    let store: Arc<dyn siwi::Store> = match std::env::var("IFINSTA_REDIS_URL") {
        #[cfg(feature = "redis")]
        Ok(_) => Arc::new(siwi_redis::RedisStore::from_env().await?),
        #[cfg(not(feature = "redis"))]
        Ok(_) => anyhow::bail!("IFINSTA_REDIS_URL is set but this build has no `redis` feature"),
        Err(_) => {
            tracing::warn!("IFINSTA_REDIS_URL not set: sessions live in memory and end when the gateway restarts");
            Arc::new(MemoryStore::new())
        }
    };
    let siwi = Arc::new(Siwi::new(config, store)?);
    let disk_check = settings.disk_check;
    let (data_dir, self_bin, uid_isolation) = (
        settings.data_dir.clone(),
        settings.self_bin.clone(),
        settings.uid_base.is_some(),
    );
    let approvers = approvals::Approvers::from_env();
    if !approvers.configured() {
        tracing::warn!("no approvers configured (JANI_CLOUD_APPROVERS / JANI_CLOUD_APPROVERS_FILE): held actions cannot be approved");
    }
    let manager = Manager::new(settings, plan);
    let reaper = manager.clone();
    let tick =
        (manager.settings().idle_ttl / 4).clamp(Duration::from_secs(1), Duration::from_secs(60));
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(tick).await;
            let n = reaper.reap().await;
            if n > 0 {
                tracing::info!(stopped = n, "stopped idle workspaces");
            }
        }
    });
    let watcher = manager.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(disk_check).await;
            watcher.enforce_disk().await;
        }
    });
    let app_manager = manager.clone();
    let profiles = approvals::Profiles::new(&data_dir);
    let tenant = std::env::var("JANI_CLOUD_TENANT")
        .ok()
        .filter(|t| !t.trim().is_empty())
        .unwrap_or_else(|| jani_core::config::defaults::TENANT.into());
    let allow_consent_proxy = std::env::var("JANI_CLOUD_ALLOW_CONSENT").as_deref() == Ok("1");
    let intake_token = std::env::var("JANI_CLOUD_INTAKE_TOKEN")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let app = Arc::new(App {
        manager,
        origin,
        tenant,
        allow_consent_proxy,
        intake_token,
        approvers,
        profiles,
        data_dir,
        self_bin,
        uid_isolation,
    });
    let router = router(AppState {
        siwi: siwi.clone(),
        app,
    });
    let bind: SocketAddr = std::env::var("JANI_CLOUD_BIND")
        .unwrap_or_else(|_| "127.0.0.1:8080".into())
        .parse()
        .context("JANI_CLOUD_BIND must be host:port")?;
    let listener = tokio::net::TcpListener::bind(bind).await?;
    tracing::info!(address = %listener.local_addr()?, "jani-cloud listening");
    let manager = app_manager;
    axum::serve(
        listener,
        router.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await?;
    let stopped = manager.stop_all().await;
    tracing::info!(stopped, "shut down");
    Ok(())
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut term = signal(SignalKind::terminate()).expect("SIGTERM handler");
        tokio::select! {
            _ = term.recv() => {}
            _ = tokio::signal::ctrl_c() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

fn router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(|| async { "ok" }))
        .route("/cloud.css", get(asset_css))
        .route("/cloud.js", get(asset_js))
        .route("/cloud/stop", post(stop))
        .route("/cloud/storage", get(storage))
        .route("/cloud/reset", post(reset))
        .route("/cloud/me", get(me))
        .route("/cloud/approvals", get(approvals_list))
        .route("/cloud/approvals/decide", post(approvals_decide))
        .route("/cloud/kill-switch", post(kill_switch))
        .route("/cloud/intake", post(intake_webhook))
        .route("/cloud/approver", get(approver_page))
        .route(
            "/cloud-approver.js",
            get(|| async { asset(APPROVER_JS, "text/javascript; charset=utf-8").await }),
        )
        .route(
            "/cloud-storage.js",
            get(|| async { asset(STORAGE_JS, "text/javascript; charset=utf-8").await }),
        )
        .merge(siwi_axum::routes(state.siwi.clone()))
        .fallback(gateway)
        .with_state(state)
}

const LOGIN: &str = r##"<!doctype html>
<html lang="en" data-density="comfortable"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>Jani</title>
<script src="/siwi/theme.js"></script>
<link rel="stylesheet" href="/siwi/ui.css">
<link rel="stylesheet" href="/cloud.css">
<script type="module" src="/siwi/siwi.js"></script>
<script type="module" src="/cloud.js"></script>
</head><body>
<a class="c-skip-link" href="#main">Skip to content</a>
<main class="c-cloud" id="main"><div class="c-panel"><div class="c-panel__body o-stack">
 <div class="o-stack o-stack--sm"><h1>Jani</h1><p class="u-muted">A governed coding assistant. Every action is checked against policy and written to an audit journal you can read.</p></div>
 <div class="o-cluster"><siwi-button data-testid="sign-in"></siwi-button><siwi-button intent="sign_up" theme="light" data-testid="sign-up"></siwi-button></div>
 <p class="u-muted" id="status" role="status" aria-live="polite" data-testid="status"></p>
 <p class="u-muted c-cloud__fine">You sign in with your ifinsta account. Your workspace is private to you and runs in its own isolated process. Prototype: not for production use.</p>
</div></div></main></body></html>"##;

const CLOUD_JS: &str = r#"
import { siwi } from "/siwi/siwi.js";
const client = siwi();
const status = document.getElementById("status");
const say = (t) => { status.textContent = t; };
// The same address serves this page signed out and the workspace signed in,
// so a reload after sign-in is the whole redirect.
client.addEventListener("siwi:signed-in", () => { say("Signed in. Opening your workspace…"); location.reload(); });
client.addEventListener("siwi:error", (e) => say("Sign-in did not complete (" + (e.detail && e.detail.error || "error") + "). Try again."));
"#;

const CLOUD_CSS: &str = r#"
.c-cloud{min-height:100vh;display:grid;place-items:center;padding:24px}
.c-cloud .c-panel{width:min(440px,100%)}
.c-cloud--wide .c-panel{width:min(1100px,100%)}
.c-cloud__table{width:100%;border-collapse:collapse}
.c-cloud__table th,.c-cloud__table td{text-align:left;padding:8px 10px;border-bottom:1px solid var(--border,#ccd)}
.mono{font-family:ui-monospace,Menlo,monospace;font-size:.9em;overflow-wrap:anywhere}
.c-cloud h1{margin:0}
.c-cloud__fine{font-size:.85em}
"#;

fn headers(mut r: Response, frame_origin: Option<&str>) -> Response {
    let frame = frame_origin
        .map(|o| format!("'self' {o}"))
        .unwrap_or_else(|| "'none'".into());
    let h = r.headers_mut();
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_str(&format!(
            "default-src 'self'; script-src 'self'; style-src 'self'; frame-src {frame}; frame-ancestors 'none'; base-uri 'none'; form-action 'self'"
        ))
        .expect("validated provider origin"),
    );
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    h.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("strict-origin-when-cross-origin"),
    );
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    r
}

async fn asset(body: &'static str, kind: &'static str) -> Response {
    let mut r = body.into_response();
    r.headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(kind));
    r.headers_mut().insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    r
}
async fn asset_css() -> Response {
    asset(CLOUD_CSS, "text/css; charset=utf-8").await
}
async fn asset_js() -> Response {
    asset(CLOUD_JS, "text/javascript; charset=utf-8").await
}

async fn login_page(siwi: &Siwi) -> Response {
    let frame = siwi.metadata().await.ok().and_then(|m| m.ui_origin());
    headers(Html(LOGIN).into_response(), frame.as_deref())
}

/// Everything that is not a gateway route. Signed out: the sign-in page at `/`,
/// 401 for the API, a redirect for anything else. Signed in: the person's own
/// workspace, proxied.
async fn gateway(
    State(app): State<Arc<App>>,
    State(siwi): State<Arc<Siwi>>,
    MaybePerson(user): MaybePerson,
    req: Request,
) -> Response {
    let path = req.uri().path().to_string();
    let Some(user) = user else {
        return if path == "/" && req.method() == axum::http::Method::GET {
            login_page(&siwi).await
        } else if path.starts_with("/api/") {
            (
                StatusCode::UNAUTHORIZED,
                Json(json!({"error": "login_required"})),
            )
                .into_response()
        } else {
            Redirect::to("/").into_response()
        };
    };
    app.profiles.record(&workspace::user_key(&user.sub), &user);
    proxy::forward(&app, &user.sub, req).await
}

/// Sign-out hook: stop the caller's workspace before the session ends.
async fn stop(State(app): State<Arc<App>>, user: Person, req: Request) -> Response {
    if let Err(why) = proxy::same_origin(&app, req.headers()) {
        return proxy::forbidden(why);
    }
    let stopped = app.manager.stop(&user.sub).await;
    Json(json!({"ok": true, "stopped": stopped})).into_response()
}

/// What a person sees when their workspace cannot be reached. The detail is
/// logged, not shown: it can name paths and processes.
pub fn failure_response(f: &Failure, html: bool) -> Response {
    tracing::error!(error = %f, "workspace unavailable");
    match f {
        Failure::Capacity => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "workspace_unavailable", "message": "All workspaces are busy. Try again in a moment."})),
        )
            .into_response(),
        Failure::Start(_) => (
            StatusCode::BAD_GATEWAY,
            Json(json!({"error": "workspace_unavailable", "message": "Your workspace could not start. Try again shortly."})),
        )
            .into_response(),
        Failure::OverQuota { used_mb, limit_mb } => {
            if html {
                let page = STORAGE_PAGE.replace("__USED__", &used_mb.to_string()).replace("__LIMIT__", &limit_mb.to_string());
                return headers(Html(page).into_response(), None).with_status(StatusCode::INSUFFICIENT_STORAGE);
            }
            (
                StatusCode::INSUFFICIENT_STORAGE,
                Json(json!({"error": "storage_full", "message": format!("Your workspace storage is over its limit ({used_mb} of {limit_mb} MB)."), "usedMb": used_mb, "limitMb": limit_mb})),
            )
                .into_response()
        }
    }
}

trait WithStatus {
    fn with_status(self, status: StatusCode) -> Response;
}
impl WithStatus for Response {
    fn with_status(mut self, status: StatusCode) -> Response {
        *self.status_mut() = status;
        self
    }
}

const STORAGE_PAGE: &str = r##"<!doctype html>
<html lang="en" data-density="comfortable"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>Storage full</title>
<script src="/siwi/theme.js"></script>
<link rel="stylesheet" href="/siwi/ui.css"><link rel="stylesheet" href="/cloud.css">
<script type="module" src="/cloud-storage.js"></script>
</head><body><main class="c-cloud" id="main"><div class="c-panel"><div class="c-panel__body o-stack">
 <h1>Your workspace is full</h1>
 <p>It is using <b>__USED__ MB</b> of its <b>__LIMIT__ MB</b> limit, so it was stopped. Your audit journal and approvals are kept.</p>
 <p class="u-muted">You can delete your workspace files and start again with an empty worktree. This cannot be undone.</p>
 <div class="o-cluster"><button class="c-button c-button--primary" type="button" id="reset" data-testid="reset">Delete my workspace files</button></div>
 <p class="u-muted" id="status" role="status" aria-live="polite"></p>
</div></div></main></body></html>"##;

const STORAGE_JS: &str = r#"
const status = document.getElementById("status");
document.getElementById("reset").addEventListener("click", async () => {
  status.textContent = "Deleting…";
  const r = await fetch("/cloud/reset", { method: "POST", credentials: "same-origin", headers: { "x-siwi": "1", "content-type": "application/json" }, body: "{}" });
  if (r.ok) location.reload(); else status.textContent = "Could not delete (" + r.status + "). Try again.";
});
"#;

/// A person's own storage use, for the page to show.
async fn storage(State(app): State<Arc<App>>, user: Person) -> Response {
    let (used, limit) = app.manager.storage(&user.sub);
    Json(json!({"usedMb": used, "limitMb": limit})).into_response()
}

/// Delete the person's worktree and start over. State is kept.
async fn reset(State(app): State<Arc<App>>, user: Person, req: Request) -> Response {
    if let Err(why) = proxy::same_origin(&app, req.headers()) {
        return proxy::forbidden(why);
    }
    match app.manager.reset(&user.sub).await {
        Ok(()) => Json(json!({"ok": true})).into_response(),
        Err(e) => {
            tracing::error!(error = %e, "reset failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": "reset_failed"})),
            )
                .into_response()
        }
    }
}

// ----------------------------------------------------------------- approvals

#[derive(Deserialize)]
struct DecideBody {
    user: String,
    id: String,
    approve: bool,
}

fn not_an_approver() -> Response {
    (StatusCode::FORBIDDEN, Json(json!({"error": "not_an_approver", "message": "Your account is not on this deployment's approver list."}))).into_response()
}

/// Who the signed-in person is, and whether they may approve. Also what an
/// operator copies `sub` from to add the first approver.
async fn me(State(app): State<Arc<App>>, person: Person) -> Response {
    let approver = app.approvers.is_approver(&person);
    let pending = if approver {
        let me = workspace::user_key(&person.sub);
        approvals::list_pending(&app.data_dir, &app.profiles, &me)
            .iter()
            .filter(|v| v["own"] == false)
            .count()
    } else {
        0
    };
    Json(json!({
        "email": person.email,
        "emailVerified": person.email_verified,
        "name": person.name,
        "sub": person.sub,
        "approver": approver,
        "pending": pending,
    }))
    .into_response()
}

async fn approvals_list(State(app): State<Arc<App>>, person: Person) -> Response {
    if person.degraded || !app.approvers.is_approver(&person) {
        return not_an_approver();
    }
    let me = workspace::user_key(&person.sub);
    Json(json!({"pending": approvals::list_pending(&app.data_dir, &app.profiles, &me)}))
        .into_response()
}

async fn approvals_decide(
    State(app): State<Arc<App>>,
    person: Person,
    headers: axum::http::HeaderMap,
    Json(body): Json<DecideBody>,
) -> Response {
    if person.degraded || !app.approvers.is_approver(&person) {
        return not_an_approver();
    }
    if let Err(why) = proxy::same_origin(&app, &headers) {
        return proxy::forbidden(why);
    }
    match approvals::decide(
        &app.data_dir,
        &app.self_bin,
        app.uid_isolation,
        &app.tenant,
        &person,
        &body.user,
        &body.id,
        body.approve,
    )
    .await
    {
        Ok(result) => Json(result).into_response(),
        // The store's own words (separation of duties, expired, wrong role) are the useful ones.
        Err(e) => (
            StatusCode::CONFLICT,
            Json(json!({"error": "refused", "message": format!("{e:#}")})),
        )
            .into_response(),
    }
}

async fn approver_page(
    State(app): State<Arc<App>>,
    State(siwi): State<Arc<Siwi>>,
    person: Person,
) -> Response {
    if person.degraded || !app.approvers.is_approver(&person) {
        return not_an_approver();
    }
    let _ = siwi;
    headers(Html(APPROVER_PAGE).into_response(), None)
}

const APPROVER_PAGE: &str = r##"<!doctype html>
<html lang="en" data-density="comfortable"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>Approvals</title>
<script src="/siwi/theme.js"></script>
<link rel="stylesheet" href="/siwi/ui.css"><link rel="stylesheet" href="/cloud.css">
<script type="module" src="/cloud-approver.js"></script>
</head><body><main class="c-cloud c-cloud--wide" id="main"><div class="c-panel"><div class="c-panel__body o-stack">
 <div class="o-cluster" style="justify-content:space-between"><h1>Approvals</h1><div class="o-cluster"><button class="c-button" type="button" id="kill-switch" data-testid="kill-switch">Activate org kill switch</button><a class="c-button" href="/">Back to my workspace</a></div></div>
 <p class="u-muted">Actions people's assistants are waiting to take. You see purpose, verification receipt status, and a redacted resource preview. Approving lets that one action run once.</p>
 <div id="list" data-testid="approvals"></div>
 <p class="u-muted" id="status" role="status" aria-live="polite"></p>
</div></div></main></body></html>"##;

const APPROVER_JS: &str = r#"
const list = document.getElementById("list");
const status = document.getElementById("status");
const killBtn = document.getElementById("kill-switch");
const when = (t) => new Date(t * 1000).toLocaleString();
const mono = (t) => { const s = document.createElement("span"); s.className = "mono"; s.textContent = t; return s; };
let armed = null;
let killArmed = false;
if (killBtn) {
  killBtn.addEventListener("click", async () => {
    if (!killArmed) { killArmed = true; killBtn.textContent = "Confirm fleet kill switch"; return; }
    killArmed = false; killBtn.textContent = "Activate org kill switch"; status.textContent = "Stopping fleet…";
    const res = await fetch("/cloud/kill-switch", { method: "POST", credentials: "same-origin", headers: { "content-type": "application/json", "x-siwi": "1" }, body: JSON.stringify({ reason: "approver console trigger" }) });
    const out = await res.json().catch(() => ({}));
    status.textContent = res.ok ? ("Kill switch active. Stopped " + (out.stopped || 0) + " workspaces.") : (out.message || "Refused (" + res.status + ").");
  });
}
async function load() {
  const r = await fetch("/cloud/approvals", { credentials: "same-origin" });
  if (!r.ok) { status.textContent = "Could not load approvals (" + r.status + ")."; return; }
  const { pending } = await r.json();
  list.textContent = "";
  if (!pending.length) { const p = document.createElement("p"); p.className = "u-muted"; p.textContent = "Nothing is waiting for you."; list.append(p); return; }
  const table = document.createElement("table"); table.className = "c-cloud__table";
  table.innerHTML = "<thead><tr><th>Requested by</th><th>Action</th><th>Context / Verified</th><th>Preview</th><th>Risk</th><th>Expires</th><th></th></tr></thead>";
  const body = document.createElement("tbody");
  for (const a of pending) {
    const tr = document.createElement("tr");
    const cell = (...nodes) => { const td = document.createElement("td"); td.append(...nodes); tr.append(td); return td; };
    cell(a.requester + (a.own ? " (you)" : ""));
    cell(mono(a.capability));
    const ctx = (a.taskContext && a.taskContext.purpose ? a.taskContext.purpose : a.taskId) + " | " + (a.verification ? (a.verification.passed ? "verified" : "failed-checks") : "unverified");
    cell(ctx);
    cell(mono(a.resourcePreview || a.resource));
    cell(a.risk);
    cell(when(a.expiresAt));
    const actions = cell();
    if (a.own) { actions.textContent = "Someone else must decide this"; }
    else {
      const key = a.user + ":" + a.approvalId;
      for (const approve of [true, false]) {
        const b = document.createElement("button");
        b.type = "button"; b.className = "c-button" + (approve ? " c-button--primary" : "");
        const mine = armed && armed.key === key && armed.approve === approve;
        b.textContent = mine ? "Confirm " + (approve ? "approve" : "reject") : (approve ? "Approve" : "Reject");
        b.dataset.testid = (approve ? "approve-" : "reject-") + a.approvalId;
        b.addEventListener("click", async () => {
          if (!mine) { armed = { key, approve }; load(); return; }
          armed = null; status.textContent = "Recording…";
          const res = await fetch("/cloud/approvals/decide", { method: "POST", credentials: "same-origin", headers: { "content-type": "application/json", "x-siwi": "1" }, body: JSON.stringify({ user: a.user, id: a.approvalId, approve }) });
          const out = await res.json().catch(() => ({}));
          status.textContent = res.ok ? (approve ? "Approved." : "Rejected.") : (out.message || "Refused (" + res.status + ").");
          load();
        });
        actions.append(b, " ");
      }
    }
    body.append(tr);
  }
  table.append(body); list.append(table);
}
load();
setInterval(() => { if (!document.hidden && !armed) load(); }, 10000);
"#;

#[derive(Deserialize)]
struct KillSwitchBody {
    #[serde(default)]
    reason: Option<String>,
}

async fn kill_switch(State(app): State<Arc<App>>, person: Person, req: Request) -> Response {
    if let Err(why) = proxy::same_origin(&app, req.headers()) {
        return proxy::forbidden(why);
    }
    if person.degraded || !app.approvers.is_approver(&person) {
        return not_an_approver();
    }
    let body_bytes = match axum::body::to_bytes(req.into_body(), 16 * 1024).await {
        Ok(b) => b,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": "invalid body"})),
            )
                .into_response()
        }
    };
    let parsed: KillSwitchBody =
        serde_json::from_slice(&body_bytes).unwrap_or(KillSwitchBody { reason: None });
    let reason = parsed
        .reason
        .as_deref()
        .unwrap_or("cloud approver emergency kill switch");
    if let Err(e) = jani_core::kill::activate(&app.data_dir, Some(&app.tenant), &person.sub, reason)
    {
        tracing::error!(error = %e, "kill switch activation failed");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": "kill_switch_failed"})),
        )
            .into_response();
    }
    let mut killed_tasks = 0usize;
    if let Ok(entries) = std::fs::read_dir(app.data_dir.join("users")) {
        for entry in entries.flatten() {
            let key = entry.file_name().to_string_lossy().into_owned();
            if workspace::user_key(&key) != key && !key.chars().all(|c| c.is_ascii_hexdigit()) {
                continue;
            }
            let st = entry.path().join("state");
            if st.is_dir() {
                let _ = jani_core::kill::activate(&st, Some(&app.tenant), &person.sub, reason);
                if let Ok(mut store) = jani_store::tasks::TaskStore::open(&st) {
                    killed_tasks += store.kill_tenant(&app.tenant).unwrap_or(0);
                }
            }
        }
    }
    let stopped = app.manager.stop_all().await;
    Json(json!({
        "ok": true,
        "stopped": stopped,
        "killedTasks": killed_tasks,
    }))
    .into_response()
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct IntakeBody {
    user: String,
    source: String,
    event: String,
    reference: String,
    delivery_id: String,
    #[serde(default)]
    prompt: Option<String>,
}

fn constant_eq(a: &str, b: &str) -> bool {
    let ab = a.as_bytes();
    let bb = b.as_bytes();
    let len = ab.len().max(bb.len());
    let mut diff = ab.len() ^ bb.len();
    for i in 0..len {
        diff |= usize::from(ab.get(i).copied().unwrap_or(0) ^ bb.get(i).copied().unwrap_or(0));
    }
    diff == 0
}

async fn intake_webhook(
    State(app): State<Arc<App>>,
    MaybePerson(person): MaybePerson,
    req: Request,
) -> Response {
    let authorized_by_token = match (
        app.intake_token.as_deref(),
        req.headers()
            .get("x-jani-intake-token")
            .and_then(|v| v.to_str().ok()),
    ) {
        (Some(expected), Some(presented)) => constant_eq(expected, presented),
        _ => false,
    };
    let authorized_by_person = person.as_ref().is_some_and(|p| !p.degraded);
    if !authorized_by_token && !authorized_by_person {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "intake requires valid X-Jani-Intake-Token or fresh session"})),
        )
            .into_response();
    }
    if authorized_by_person && !authorized_by_token {
        if let Err(why) = proxy::same_origin(&app, req.headers()) {
            return proxy::forbidden(why);
        }
    }
    let body = match axum::body::to_bytes(req.into_body(), 128 * 1024).await {
        Ok(b) => b,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": "invalid body"})),
            )
                .into_response()
        }
    };
    let Ok(payload) = serde_json::from_slice::<IntakeBody>(&body) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "invalid intake JSON"})),
        )
            .into_response();
    };
    if !jani_core::types::valid_id(&payload.source, 64)
        || !jani_core::types::valid_id(&payload.delivery_id, 96)
        || payload.reference.trim().is_empty()
    {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "invalid intake identifiers"})),
        )
            .into_response();
    }
    if jani_core::kill::is_killed(&app.data_dir, &app.tenant)
        .unwrap_or(None)
        .is_some()
    {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "kill_switch_active"})),
        )
            .into_response();
    }
    let user_dir_key = workspace::user_key(&payload.user);
    let state_dir = app.data_dir.join("users").join(&user_dir_key).join("state");
    if let Err(e) = std::fs::create_dir_all(state_dir.join("intake")) {
        tracing::error!(error = %e, "intake mkdir failed");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": "intake_storage_failed"})),
        )
            .into_response();
    }
    let task_id = format!(
        "intake-{}",
        jani_core::types::sha256_hex(payload.reference.as_bytes())
            .chars()
            .take(12)
            .collect::<String>()
    );
    let key = format!(
        "intake:{}:{}:{}",
        payload.source, payload.reference, payload.delivery_id
    );
    let digest = jani_core::types::sha256_hex(&body);
    let mut store = match jani_store::tasks::TaskStore::open(&state_dir) {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(error = %e, "intake task store failed");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": "task_store_failed"})),
            )
                .into_response();
        }
    };
    let _ = store.checkpoint(
        &app.tenant,
        &task_id,
        1,
        "queued",
        None,
        None,
        &payload.user,
        "webhook-intake",
        "",
        &[],
    );
    match store.begin_effect(
        &app.tenant,
        &task_id,
        1,
        &key,
        "intake.webhook",
        &payload.reference,
        &digest,
    ) {
        Ok(jani_store::tasks::EffectGate::AlreadyApplied(_)) => (
            StatusCode::OK,
            Json(json!({"ok": true, "deduplicated": true, "taskId": task_id})),
        )
            .into_response(),
        Ok(jani_store::tasks::EffectGate::InFlight) => (
            StatusCode::CONFLICT,
            Json(json!({"error": "duplicate delivery in flight"})),
        )
            .into_response(),
        Ok(jani_store::tasks::EffectGate::Proceed) => {
            let (scrubbed_prompt, _) =
                jani_core::data::redact_text(payload.prompt.as_deref().unwrap_or(""));
            let envelope = json!({
                "taskId": task_id,
                "source": payload.source,
                "event": payload.event,
                "reference": payload.reference,
                "deliveryId": payload.delivery_id,
                "prompt": scrubbed_prompt,
            });
            let dest = state_dir
                .join("intake")
                .join(format!("{}.json", payload.delivery_id));
            let _ = std::fs::write(
                &dest,
                serde_json::to_vec_pretty(&envelope).unwrap_or_default(),
            );
            let _ = store.finish_effect(&app.tenant, &task_id, &key, true, Some(&envelope));
            (
                StatusCode::ACCEPTED,
                Json(json!({"ok": true, "queued": true, "taskId": task_id})),
            )
                .into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e}))).into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constant_eq_handles_mismatched_lengths() {
        assert!(constant_eq("secret-token", "secret-token"));
        assert!(!constant_eq("secret-token", "secret-toke"));
        assert!(!constant_eq("secret-token", "secret-tokex"));
    }
}
