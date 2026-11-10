//! Synchronizer tokens for native browser forms. OAuth/setup redirects and
//! HMAC-authenticated webhooks have their own verification contracts.

use axum::{
    body::{Body, to_bytes},
    extract::{FromRequest, Request},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use rand::{RngCore, rngs::OsRng};
use serde::de::DeserializeOwned;
use subtle::ConstantTimeEq;

pub(crate) fn generate_token() -> String {
    let mut bytes = [0; 32];
    OsRng.fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

/// Body extractor used after account authorization. Tokens never enter command
/// payloads, URLs, or parser error responses. Only native URL-encoded forms are
/// accepted; missing, duplicate, and invalid tokens share one denial.
pub struct CsrfForm<T>(pub T);

impl<S, T> FromRequest<S> for CsrfForm<T>
where
    S: Send + Sync,
    T: DeserializeOwned + Send,
{
    type Rejection = Response;

    async fn from_request(request: Request, state: &S) -> Result<Self, Self::Rejection> {
        read_form(request, state, true).await.map(Self)
    }
}

/// Anonymous sessions use the same synchronizer token for capability submission.
pub struct PublicCsrfForm<T>(pub T);
impl<S, T> FromRequest<S> for PublicCsrfForm<T>
where
    S: Send + Sync,
    T: DeserializeOwned + Send,
{
    type Rejection = Response;
    async fn from_request(request: Request, state: &S) -> Result<Self, Self::Rejection> {
        read_form(request, state, false).await.map(Self)
    }
}

// Axum extractors return their complete HTTP rejection response.
#[allow(clippy::result_large_err)]
async fn read_form<S: Send + Sync, T: DeserializeOwned + Send>(
    request: Request,
    state: &S,
    require_authenticated: bool,
) -> Result<T, Response> {
    let deny = || {
        (
            StatusCode::FORBIDDEN,
            "Invalid CSRF token. Reload the page and try again.",
        )
            .into_response()
    };
    let tower = request
        .extensions()
        .get::<tower_sessions::Session>()
        .ok_or_else(deny)?;
    let session = crate::session::load(tower)
        .await
        .map_err(|error| crate::WebError::Session(error.to_string()).into_response())?;
    if require_authenticated && !session.is_authenticated() {
        let path = request
            .uri()
            .path_and_query()
            .map(|p| p.as_str())
            .unwrap_or("/");
        let encoded: String = url::form_urlencoded::byte_serialize(path.as_bytes()).collect();
        return Err(
            axum::response::Redirect::to(&format!("/login?return_to={encoded}")).into_response(),
        );
    }
    let expected = session.csrf_token.as_deref().ok_or_else(deny)?;
    if request
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(';').next())
        != Some("application/x-www-form-urlencoded")
    {
        return Err(deny());
    }
    let (parts, body) = request.into_parts();
    let bytes = to_bytes(body, 2 * 1024 * 1024).await.map_err(|_| deny())?;
    let mut tokens = url::form_urlencoded::parse(&bytes).filter(|(key, _)| key == "csrf_token");
    let (_, supplied) = tokens.next().ok_or_else(deny)?;
    if tokens.next().is_some() || !bool::from(expected.as_bytes().ct_eq(supplied.as_bytes())) {
        return Err(deny());
    }
    // Operation identity is scalar even when other form fields (repository IDs
    // and grants) are repeated. Decode names before counting so percent-encoded
    // duplicates cannot change which command an HTTP parser sees.
    if url::form_urlencoded::parse(&bytes)
        .filter(|(key, _)| key == "operation_id")
        .count()
        > 1
    {
        return Err((StatusCode::BAD_REQUEST, "Duplicate operation identity.").into_response());
    }
    let serde_qs::axum::QsForm(form) = serde_qs::axum::QsForm::<T>::from_request(
        Request::from_parts(parts, Body::from(bytes)),
        state,
    )
    .await
    .map_err(|_| (StatusCode::BAD_REQUEST, "Invalid form.").into_response())?;
    Ok(form)
}

#[derive(serde::Deserialize)]
pub struct EmptyForm {}
