//! Runtime-test-only full service with actual D1 and Worker HTTP adapters.
use axum::{Json, http::StatusCode, response::IntoResponse};
use ghinvite_core::storage::{
    self,
    pending_signin::{PendingSignInKey, PendingSignInStorage, StoredPendingSignIn},
};
use ghinvite_web::signin::{
    Callback, PendingSignIn, PendingSignInStore, SignInError, SignInService,
    storage::SqlPendingSignInStore,
};
use std::sync::Arc;
use worker::{Env, HttpRequest};
struct Clock(i64);
impl ghinvite_core::clock::Clock for Clock {
    fn now(&self) -> i64 {
        self.0
    }
}
struct ControlledConsume {
    lose: bool,
    replacement: Option<StoredPendingSignIn>,
    now: i64,
    storage: Arc<ghinvite_storage_d1::D1Storage>,
}
#[async_trait::async_trait]
impl PendingSignInStorage for ControlledConsume {
    async fn replace_pending_signin(
        &self,
        key: PendingSignInKey,
        record: &StoredPendingSignIn,
        now: i64,
    ) -> storage::Result<()> {
        self.storage.replace_pending_signin(key, record, now).await
    }
    async fn read_pending_signin(
        &self,
        key: PendingSignInKey,
    ) -> storage::Result<Option<StoredPendingSignIn>> {
        self.storage.read_pending_signin(key).await
    }
    async fn consume_pending_signin(
        &self,
        key: PendingSignInKey,
        expected: &StoredPendingSignIn,
    ) -> storage::Result<bool> {
        if let Some(record) = &self.replacement {
            self.storage
                .replace_pending_signin(key, record, self.now)
                .await?;
        }
        let consumed = self.storage.consume_pending_signin(key, expected).await?;
        if self.lose {
            Err(storage::pending_signin::unavailable())
        } else {
            Ok(consumed)
        }
    }
}
pub async fn fetch(req: HttpRequest, env: &Env) -> worker::Result<axum::response::Response> {
    let operation = req.uri().path().to_owned();
    let bytes = axum::body::to_bytes(axum::body::Body::new(req.into_body()), 16_384)
        .await
        .map_err(|_| worker::Error::RustError("invalid pending fixture".into()))?;
    let input: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|_| worker::Error::RustError("invalid pending fixture".into()))?;
    let id = input["session_id"]
        .as_str()
        .ok_or_else(|| worker::Error::RustError("invalid pending fixture".into()))?;
    let keyring = ghinvite_web::config::parse_keyring(&env.secret("P1_KEYRING")?.to_string())
        .map_err(|_| worker::Error::RustError("invalid fixture keyring".into()))?;
    let storage = Arc::new(ghinvite_storage_d1::D1Storage::new(env.d1("DB")?));
    let now = input["now"]
        .as_i64()
        .unwrap_or_else(|| chrono::Utc::now().timestamp());
    let replacement = input
        .get("replace_before_consume")
        .map(|value| serde_json::from_value(value.clone()))
        .transpose()
        .map_err(|_| worker::Error::RustError("invalid pending fixture".into()))?;
    let backend: Arc<dyn PendingSignInStorage> = Arc::new(ControlledConsume {
        storage,
        now,
        replacement,
        lose: input["lose_take_ack"] == true,
    });
    let store =
        SqlPendingSignInStore::new(backend, keyring.clone()).with_clock(Arc::new(Clock(now)));
    let response = if operation.ends_with("-replace") {
        let pending: PendingSignIn = serde_json::from_value(input["pending"].clone())
            .map_err(|_| worker::Error::RustError("invalid pending fixture".into()))?;
        match store.replace(id, pending).await {
            Ok(()) => StatusCode::NO_CONTENT.into_response(),
            Err(_) => (StatusCode::SERVICE_UNAVAILABLE, "state unavailable").into_response(),
        }
    } else if operation.ends_with("-take") {
        match store.take(id).await {
            Ok(value) => Json(serde_json::json!({"found":value.is_some()})).into_response(),
            Err(_) => (StatusCode::SERVICE_UNAVAILABLE, "state unavailable").into_response(),
        }
    } else if operation.ends_with("-purpose") {
        match ghinvite_web::signin::storage::verify_context_separation(&keyring, id) {
            Ok(()) => StatusCode::NO_CONTENT.into_response(),
            Err(_) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "context isolation failed",
            )
                .into_response(),
        }
    } else {
        let app = ghinvite_config::apps::SignInApp::new(
            "Iv.fixture-signin",
            "fixture-client-secret",
            "https://signin.test/oauth/callback".parse().unwrap(),
        );
        let service = SignInService::new(&app)
            .map_err(|_| worker::Error::RustError("fixture configuration".into()))?;
        let state = input["state"].as_str().unwrap_or_default().to_owned();
        let code = input["code"].as_str().unwrap_or_default().to_owned();
        match service
            .complete(&store, id, Callback::code(state, code))
            .await
        {
            Ok(identity) => Json(serde_json::json!({"identity":identity})).into_response(),
            Err(error) => {
                let status = match error {
                    SignInError::NoPendingSignIn => StatusCode::CONFLICT,
                    SignInError::StateUnavailable | SignInError::KeyringRetry => {
                        StatusCode::SERVICE_UNAVAILABLE
                    }
                    _ => StatusCode::BAD_REQUEST,
                };
                (status, Json(serde_json::json!({"error":error.to_string()}))).into_response()
            }
        }
    };
    Ok(response)
}
