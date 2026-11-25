//! Who is signed in. This mirrors `siwi_axum::SiwiUser` but also carries
//! `email_verified`, which the approver list needs: an unverified email must
//! never grant the power to approve another person's action.

use axum::{
    extract::{FromRef, FromRequestParts},
    http::{header, request::Parts, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;
use siwi::Siwi;
use std::sync::Arc;

#[derive(Clone, Debug)]
pub struct Person {
    pub sub: String,
    pub email: Option<String>,
    pub email_verified: bool,
    pub name: Option<String>,
    pub degraded: bool,
}

fn cookie(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|p| p.trim().split_once('='))
        .find(|(k, _)| *k == name)
        .map(|(_, v)| v.to_owned())
}

pub struct Unauthenticated(StatusCode);

impl IntoResponse for Unauthenticated {
    fn into_response(self) -> Response {
        let error = if self.0 == StatusCode::UNAUTHORIZED {
            "login_required"
        } else {
            "identity_provider_unavailable"
        };
        (self.0, Json(json!({ "error": error }))).into_response()
    }
}

impl<S> FromRequestParts<S> for Person
where
    S: Send + Sync,
    Arc<Siwi>: FromRef<S>,
{
    type Rejection = Unauthenticated;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let siwi = Arc::<Siwi>::from_ref(state);
        let name = if siwi.config().secure_cookies() {
            "__Host-siwi"
        } else {
            "siwi_session"
        };
        let token =
            cookie(&parts.headers, name).ok_or(Unauthenticated(StatusCode::UNAUTHORIZED))?;
        let state = match siwi.session(&token).await {
            Ok(Some(s)) => s,
            Ok(None) => return Err(Unauthenticated(StatusCode::UNAUTHORIZED)),
            Err(_) => return Err(Unauthenticated(StatusCode::SERVICE_UNAVAILABLE)),
        };
        let degraded = matches!(state, siwi::SessionState::Degraded(_));
        let record = state.record();
        Ok(Person {
            sub: record.sub.clone(),
            email: record.email.clone(),
            email_verified: record.email_verified == Some(true),
            name: record.name.clone(),
            degraded,
        })
    }
}

/// Never rejects.
pub struct MaybePerson(pub Option<Person>);

impl<S> FromRequestParts<S> for MaybePerson
where
    S: Send + Sync,
    Arc<Siwi>: FromRef<S>,
{
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        Ok(MaybePerson(
            Person::from_request_parts(parts, state).await.ok(),
        ))
    }
}
