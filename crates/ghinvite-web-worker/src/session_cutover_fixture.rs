//! Runtime-only inspection of a session the real KV adapter stored.
use axum::{Json, response::IntoResponse};
use ghinvite_web::session_store::ProtectedStore;
use tower_sessions::{SessionStore, session::Id};
use worker::{Env, HttpRequest};

/// `/__fixture/session-cutover-inspect?{id}`: whether the stored session
/// payload holds a credential field, and its fixed deadline.
pub async fn fetch(req: HttpRequest, env: &Env) -> worker::Result<axum::response::Response> {
    let config = crate::config_from_env(env)?;
    let store = ProtectedStore::new(crate::KvSessionStore::from_env(env)?, config.keyring);
    let id: Id = req
        .uri()
        .query()
        .unwrap_or_default()
        .parse()
        .map_err(|_| crate::worker_err("invalid fixture ID"))?;
    let record = store
        .load(&id)
        .await
        .map_err(crate::worker_err)?
        .ok_or_else(|| crate::worker_err("fixture session missing"))?;
    let session = &record.data["ghinvite"];
    Ok(Json(serde_json::json!({
        "credential_field": session.get("access_token").is_some(),
        "user_id": session["user_id"],
        "deadline": record.data["ghinvite_lifetime"]["deadline"],
    }))
    .into_response())
}
