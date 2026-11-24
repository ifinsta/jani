//! The authenticated reverse proxy to a person's own `jani ui`.
//!
//! Only `/` and `/api/*` are forwarded. The gateway adds the workspace token and
//! a loopback `Origin`; it drops cookies and every other header, so the browser's
//! siwi session cookie never reaches the workspace and the workspace's token
//! never reaches the browser.

use crate::{failure_response, App};
use axum::{
    body::{to_bytes, Body},
    extract::Request,
    http::{header, HeaderMap, HeaderName, HeaderValue, Method, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;
use siwi_axum::SiwiUser;

const MAX_BODY: usize = 64 * 1024;

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

pub async fn forward(app: &App, user: &SiwiUser, req: Request) -> Response {
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
    let endpoint = match app.manager.endpoint(&user.sub, app.sandbox).await {
        Ok(e) => e,
        Err(f) => return failure_response(&f),
    };
    let target = format!(
        "http://127.0.0.1:{}{}",
        endpoint.port,
        parts
            .uri
            .path_and_query()
            .map(|p| p.as_str())
            .unwrap_or("/")
    );
    let mut out = app.client.request(parts.method.clone(), target);
    for name in FORWARD_REQUEST {
        if let Some(v) = parts.headers.get(&name) {
            out = out.header(name, v);
        }
    }
    out = out.header("X-Jani-Token", &endpoint.token);
    if parts.method == Method::POST {
        out = out.header(
            header::ORIGIN,
            format!("http://127.0.0.1:{}", endpoint.port),
        );
        out = out.body(bytes);
    }
    let reply = match out.send().await {
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
    let status = StatusCode::from_u16(reply.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    let mut headers = HeaderMap::new();
    for name in FORWARD_RESPONSE {
        if let Some(v) = reply.headers().get(name.as_str()) {
            if let Ok(v) = HeaderValue::from_bytes(v.as_bytes()) {
                headers.insert(name, v);
            }
        }
    }
    let body = match reply.bytes().await {
        Ok(b) => b,
        Err(_) => {
            return (
                StatusCode::BAD_GATEWAY,
                Json(json!({"error": "workspace_unreachable"})),
            )
                .into_response()
        }
    };
    let mut response = Response::new(Body::from(body));
    *response.status_mut() = status;
    *response.headers_mut() = headers;
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_page_and_the_api_are_forwarded() {
        for ok in ["/", "/api/overview", "/api/session?after=1"] {
            assert!(allowed_path(ok.split('?').next().unwrap()), "{ok}");
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
