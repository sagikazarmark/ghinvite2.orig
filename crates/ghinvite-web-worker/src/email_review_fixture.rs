//! Runtime-test-only driver; escrow never exposes plaintext on production routes.
use axum::{Json, http::StatusCode, response::IntoResponse};
use ghinvite_core::{
    email_delivery::{EmailCreateCommand, EmailRef},
    storage::{
        self,
        email_review::{EmailReviewStorage, NewReviewEmail, ReviewEmailRecord},
        email_vault::EmailVaultBinding,
    },
};
use ghinvite_email_vault::{EmailVault, Error, ReviewEmail};
use std::sync::Arc;
use worker::{Env, HttpRequest};
struct LostAck {
    inner: Arc<ghinvite_storage_d1::D1Storage>,
    lose: bool,
}
#[async_trait::async_trait]
impl EmailReviewStorage for LostAck {
    async fn retain_review_email(&self, r: &NewReviewEmail) -> storage::Result<bool> {
        let result = self.inner.retain_review_email(r).await?;
        if self.lose {
            Err(storage::email_vault::unavailable())
        } else {
            Ok(result)
        }
    }
    async fn review_email(&self, r: EmailRef) -> storage::Result<Option<ReviewEmailRecord>> {
        self.inner.review_email(r).await
    }
    async fn clear_review_email(&self, b: &EmailVaultBinding, n: i64) -> storage::Result<bool> {
        self.inner.clear_review_email(b, n).await
    }
    async fn prune_review_emails(&self, n: i64) -> storage::Result<u64> {
        self.inner.prune_review_emails(n).await
    }
}
pub async fn fetch(req: HttpRequest, env: &Env) -> worker::Result<axum::response::Response> {
    let path = req.uri().path().to_owned();
    let bytes = axum::body::to_bytes(axum::body::Body::new(req.into_body()), 131072)
        .await
        .map_err(|_| worker::Error::RustError("invalid review fixture".into()))?;
    let input: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|_| worker::Error::RustError("invalid review fixture".into()))?;
    let now = input["now"]
        .as_i64()
        .ok_or_else(|| worker::Error::RustError("invalid review fixture".into()))?;
    let ring = ghinvite_config::bulk::email_keyring(
        true,
        Some(env.secret("P1_EMAIL_KEYRING")?.to_string()),
    )
    .map_err(|_| worker::Error::RustError("invalid review configuration".into()))?
    .ok_or_else(|| worker::Error::RustError("invalid review configuration".into()))?;
    let storage = Arc::new(ghinvite_storage_d1::D1Storage::new(env.d1("DB")?));
    let vault = EmailVault::new(storage.clone(), ring).with_review_storage(Arc::new(LostAck {
        inner: storage.clone(),
        lose: input["lose_ack"] == true,
    }));
    let result: Result<serde_json::Value, Error> = async {
        if path.ends_with("/retain") {
            #[derive(serde::Deserialize)]
            struct Record {
                binding: EmailVaultBinding,
                email: String,
            }
            impl Drop for Record {
                fn drop(&mut self) {
                    use zeroize::Zeroize;
                    self.email.zeroize();
                }
            }
            let records: Vec<Record> =
                serde_json::from_value(input["records"].clone()).map_err(|_| Error::Conflict)?;
            let parsed = records
                .iter()
                .map(|r| {
                    ghinvite_email_vault::Address::parse(&r.email).map_err(|_| Error::Conflict)
                })
                .collect::<Result<Vec<_>, _>>()?;
            let entries = records
                .iter()
                .zip(&parsed)
                .map(|(r, a)| ReviewEmail {
                    binding: &r.binding,
                    address: a,
                })
                .collect::<Vec<_>>();
            vault.retain_review(&entries, now).await?;
            Ok(serde_json::json!({"retained":true}))
        } else if path.ends_with("/stage") {
            let commands: Vec<EmailCreateCommand> =
                serde_json::from_value(input["commands"].clone()).map_err(|_| Error::Conflict)?;
            vault.stage_reviewed(&commands, now).await?;
            Ok(serde_json::json!({"staged":true}))
        } else if path.ends_with("/retire") {
            let bindings: Vec<EmailVaultBinding> =
                serde_json::from_value(input["bindings"].clone()).map_err(|_| Error::Conflict)?;
            vault.retire_removed(&bindings, now).await?;
            Ok(serde_json::json!({"retired":true}))
        } else if path.ends_with("/prune") {
            let count = storage
                .prune_review_emails(now)
                .await
                .map_err(|_| Error::Unavailable)?;
            Ok(serde_json::json!({"count":count}))
        } else {
            Err(Error::Conflict)
        }
    }
    .await;
    Ok(match result {
        Ok(value) => Json(value).into_response(),
        Err(error) => {
            let status = match error {
                Error::Unavailable => StatusCode::SERVICE_UNAVAILABLE,
                Error::Conflict => StatusCode::CONFLICT,
                Error::Expired => StatusCode::GONE,
                Error::NotActive => StatusCode::PRECONDITION_FAILED,
            };
            (status, Json(serde_json::json!({"error":error.to_string()}))).into_response()
        }
    })
}
