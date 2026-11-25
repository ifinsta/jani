//! The authenticated reverse proxy to a person's own `jani ui`.
//!
//! Only `/` and `/api/*` are forwarded. The gateway adds the workspace token and
//! a fixed `Origin`/`Host`; it drops cookies and every other header, so the
//! browser's siwi session cookie never reaches the workspace and the workspace's
//! token never reaches the browser. The workspace listens on a Unix socket in
//! the person's own directory, so there is no network port to reach it by.

use crate::{failure_response, App};
use axum::{
    body::{to_bytes, Body},
    extract::Request,
    http::{header, HeaderMap, HeaderName, HeaderValue, Method, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::client::conn::http1;
use hyper_util::rt::TokioIo;
use serde_json::json;
use std::{path::Path, time::Duration};
use tokio::net::UnixStream;

const MAX_BODY: usize = 64 * 1024;
const MAX_RESPONSE_BODY: usize = 2 * 1024 * 1024;
/// The fixed Host the workspace is told it is served as.
const WORKSPACE_HOST: &str = "localhost";

/// Paths a workspace serves to a browser. Anything else is not forwarded.
pub fn allowed_path(path: &str) -> bool {
    (path == "/" || path.starts_with("/api/"))
        && !path.contains("..")
        && !path.contains("//")
        && !path.contains('\\')
}

/// CSRF defence for state-changing browser calls, matching siwi's own rules:
/// an exact `Origin`, and `Sec-Fetch-Site: same-origin` when the browser sends it.
pub fn same_origin(app: &App, headers: &HeaderMap) -> Result<(), &'static str> {
    match headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) {
        Some(origin) if origin == app.origin => {}
        _ => return Err("cross-origin request refused"),
    }
    match headers.get("sec-fetch-site").and_then(|v| v.to_str().ok()) {
        None | Some("same-origin") => Ok(()),
        _ => Err("cross-site request refused"),
    }
}

/// The 403 sent when `same_origin` refuses.
pub fn forbidden(why: &str) -> Response {
    (
        StatusCode::FORBIDDEN,
        Json(json!({"error": "forbidden", "message": why})),
    )
        .into_response()
}

const FORWARD_REQUEST: [HeaderName; 2] = [header::CONTENT_TYPE, header::ACCEPT];
const FORWARD_RESPONSE: [HeaderName; 6] = [
    header::CONTENT_TYPE,
    header::CONTENT_SECURITY_POLICY,
    header::X_CONTENT_TYPE_OPTIONS,
    header::REFERRER_POLICY,
    header::CACHE_CONTROL,
    HeaderName::from_static("x-frame-options"),
];

async fn call(
    socket: &Path,
    request: hyper::Request<Full<Bytes>>,
) -> Result<(StatusCode, HeaderMap, Bytes), String> {
    let work = async {
        let stream = UnixStream::connect(socket)
            .await
            .map_err(|e| format!("connect: {e}"))?;
        let (mut sender, conn) = http1::handshake(TokioIo::new(stream))
            .await
            .map_err(|e| format!("handshake: {e}"))?;
        tokio::spawn(async move {
            let _ = conn.await;
        });
        let reply = sender
            .send_request(request)
            .await
            .map_err(|e| format!("request: {e}"))?;
        let (parts, body) = reply.into_parts();
        let bytes = http_body_util::Limited::new(body, MAX_RESPONSE_BODY)
            .collect()
            .await
            .map_err(|e| format!("body: {e}"))?
            .to_bytes();
        Ok::<_, String>((parts.status, parts.headers, bytes))
    };
    tokio::time::timeout(Duration::from_secs(60), work)
        .await
        .map_err(|_| "timed out".to_string())?
}

pub async fn forward(app: &App, sub: &str, req: Request) -> Response {
    let (parts, body) = req.into_parts();
    if !allowed_path(parts.uri.path()) {
        return (StatusCode::NOT_FOUND, Json(json!({"error": "not found"}))).into_response();
    }
    if parts.method != Method::GET && parts.method != Method::POST {
        return (
            StatusCode::METHOD_NOT_ALLOWED,
            Json(json!({"error": "method not allowed"})),
        )
            .into_response();
    }
    if parts.uri.path() == "/api/consent" && !app.allow_consent_proxy {
        return forbidden("direct command consent via cloud proxy is disabled");
    }
    if parts.method == Method::POST {
        if let Err(why) = same_origin(app, &parts.headers) {
            return forbidden(why);
        }
    }
    let bytes = match to_bytes(body, MAX_BODY).await {
        Ok(b) => b,
        Err(_) => {
            return (
                StatusCode::PAYLOAD_TOO_LARGE,
                Json(json!({"error": "body too large"})),
            )
                .into_response()
        }
    };
    let is_page = parts.method == Method::GET && parts.uri.path() == "/";
    let endpoint = match app.manager.endpoint(sub).await {
        Ok(e) => e,
        Err(f) => return failure_response(&f, is_page),
    };
    let mut out = hyper::Request::builder()
        .method(parts.method.clone())
        .uri(
            parts
                .uri
                .path_and_query()
                .map(|p| p.as_str())
                .unwrap_or("/"),
        )
        .header(header::HOST, WORKSPACE_HOST)
        .header("x-jani-token", endpoint.token.as_str());
    for name in FORWARD_REQUEST {
        if let Some(v) = parts.headers.get(&name) {
            out = out.header(name, v);
        }
    }
    if parts.uri.path() == "/api/consent" && !app.allow_consent_proxy {
        return forbidden("direct command consent via cloud proxy is disabled");
    }
    if parts.method == Method::POST {
        out = out.header(header::ORIGIN, format!("http://{WORKSPACE_HOST}"));
    }
    let Ok(request) = out.body(Full::new(if parts.method == Method::POST {
        bytes
    } else {
        Bytes::new()
    })) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "bad request"})),
        )
            .into_response();
    };
    let (status, headers, body) = match call(&endpoint.socket, request).await {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(error = %e, "workspace did not answer");
            return (
                StatusCode::BAD_GATEWAY,
                Json(json!({"error": "workspace_unreachable"})),
            )
                .into_response();
        }
    };
    let mut kept = HeaderMap::new();
    for name in FORWARD_RESPONSE {
        if let Some(v) = headers.get(&name) {
            if let Ok(v) = HeaderValue::from_bytes(v.as_bytes()) {
                kept.insert(name, v);
            }
        }
    }
    if !kept.contains_key(header::CONTENT_SECURITY_POLICY) {
        kept.insert(
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static(
                "default-src 'self'; script-src 'self'; style-src 'self'; object-src 'none'; frame-ancestors 'none'; base-uri 'none'",
            ),
        );
    }
    let mut response = Response::new(Body::from(body));
    *response.status_mut() = status;
    *response.headers_mut() = kept;
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_page_and_the_api_are_forwarded() {
        for ok in ["/", "/api/overview", "/api/session"] {
            assert!(allowed_path(ok), "{ok}");
        }
        for bad in [
            "/siwi/start",
            "/cloud/stop",
            "/etc/passwd",
            "/api/../x",
            "//api/x",
            "/apix",
            "/api\\x",
            "/health",
        ] {
            assert!(!allowed_path(bad), "{bad}");
        }
    }
}
