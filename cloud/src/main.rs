//! jani-cloud: a prototype single-container front end for Jani.
//!
//! People sign in with ifinsta (`siwi`, against ifinsta accounts). Each signed-in
//! person is proxied to their own `jani ui` process. The browser never sees that
//! process's token and cannot reach any other person's.

mod proxy;
mod workspace;

use anyhow::{Context, Result};
use axum::{
    extract::{FromRef, Request, State},
    http::{header, HeaderValue, StatusCode},
    response::{Html, IntoResponse, Redirect, Response},
    routing::{get, post},
    Json, Router,
};
use serde_json::json;
use siwi::{Config, MemoryStore, Siwi};
use siwi_axum::{MaybeSiwiUser, SiwiUser};
use std::{net::SocketAddr, path::PathBuf, sync::Arc, time::Duration};
use workspace::{Failure, Manager, Sandbox, Settings};

pub struct App {
    pub manager: Arc<Manager>,
    pub sandbox: Sandbox,
    /// Exact origin of the registered redirect URI; the only Origin we accept.
    pub origin: String,
    pub client: reqwest::Client,
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
    let number = |name: &str, default: u64| -> Result<u64> {
        match std::env::var(name) {
            Ok(v) => v
                .parse()
                .with_context(|| format!("{name} must be a number")),
            Err(_) => Ok(default),
        }
    };
    Ok(Settings {
        data_dir: PathBuf::from(
            std::env::var("JANI_CLOUD_DATA").unwrap_or_else(|_| "./data".into()),
        ),
        jani_bin: PathBuf::from(
            std::env::var("JANI_CLOUD_JANI_BIN").unwrap_or_else(|_| "jani".into()),
        ),
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
        max_active: number("JANI_CLOUD_MAX_ACTIVE", 8)? as usize,
        idle_ttl: Duration::from_secs(number("JANI_CLOUD_IDLE_SECONDS", 1800)?),
        sandbox,
        uid_base: match std::env::var("JANI_CLOUD_UID_BASE") {
            Ok(v) => Some(v.parse().context("JANI_CLOUD_UID_BASE must be a number")?),
            Err(_) => None,
        },
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

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    let config = Config::from_env()?;
    let origin = config.redirect_uri.origin().ascii_serialization();
    let settings = settings_from_env()?;
    let sandbox = settings.validate(is_root())?;
    std::fs::create_dir_all(&settings.data_dir)?;
    tracing::info!(
        ?sandbox,
        uid_isolation = settings.uid_base.is_some(),
        max_active = settings.max_active,
        "workspace isolation"
    );

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
    let manager = Manager::new(settings);
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
    let app_manager = manager.clone();
    let app = Arc::new(App {
        manager,
        sandbox,
        origin,
        client: reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(60))
            .build()?,
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
    MaybeSiwiUser(user): MaybeSiwiUser,
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
    proxy::forward(&app, &user, req).await
}

/// Sign-out hook: stop the caller's workspace before the session ends.
async fn stop(State(app): State<Arc<App>>, user: SiwiUser, req: Request) -> Response {
    if let Err(why) = proxy::same_origin(&app, req.headers()) {
        return proxy::forbidden(why);
    }
    let stopped = app.manager.stop(&user.sub).await;
    Json(json!({"ok": true, "stopped": stopped})).into_response()
}

pub fn failure_response(f: &Failure) -> Response {
    let status = match f {
        Failure::Capacity => StatusCode::SERVICE_UNAVAILABLE,
        Failure::Start(_) => StatusCode::BAD_GATEWAY,
    };
    // The detail is logged, not shown: it can name paths and processes.
    tracing::error!(error = %f, "workspace unavailable");
    let public = match f {
        Failure::Capacity => "All workspaces are busy. Try again in a moment.",
        Failure::Start(_) => "Your workspace could not start. Try again shortly.",
    };
    (
        status,
        Json(json!({"error": "workspace_unavailable", "message": public})),
    )
        .into_response()
}
