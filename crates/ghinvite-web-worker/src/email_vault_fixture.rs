//! Runtime-test-only driver of the real vault, keyring and primary D1 adapter.
use axum::{Json, http::StatusCode, response::IntoResponse};
use ghinvite_core::{
    email_delivery::EmailRef,
    storage::{
        self,
        email_delivery::EmailDeliveryStorage,
        email_vault::{EmailVaultBinding, EmailVaultRecord, EmailVaultStorage, NewEmailRecord},
    },
};
use ghinvite_email_vault::{EmailVault, Error};
use std::sync::Arc;
use worker::{Env, HttpRequest};
struct LostAck {
    inner: Arc<ghinvite_storage_d1::D1Storage>,
    lose: bool,
}
#[async_trait::async_trait]
impl EmailVaultStorage for LostAck {
    async fn stage_email(&self, r: &NewEmailRecord) -> storage::Result<bool> {
        let answer = self.inner.stage_email(r).await?;
        if self.lose {
            return Err(storage::email_vault::unavailable());
        }
        Ok(answer)
    }
    async fn email_record(&self, r: EmailRef) -> storage::Result<Option<EmailVaultRecord>> {
        self.inner.email_record(r).await
    }
    async fn activate_email(&self, b: &EmailVaultBinding, n: i64) -> storage::Result<bool> {
        self.inner.activate_email(b, n).await
    }
    async fn retire_email(&self, b: &EmailVaultBinding, n: i64) -> storage::Result<bool> {
        self.inner.retire_email(b, n).await
    }
    async fn prune_emails(&self, n: i64) -> storage::Result<u64> {
        self.inner.prune_emails(n).await
    }
}
pub async fn fetch(req: HttpRequest, env: &Env) -> worker::Result<axum::response::Response> {
    let operation = req.uri().path().to_owned();
    let bytes = axum::body::to_bytes(axum::body::Body::new(req.into_body()), 16_384)
        .await
        .map_err(|_| worker::Error::RustError("invalid email fixture".into()))?;
    let input: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|_| worker::Error::RustError("invalid email fixture".into()))?;
    let binding: EmailVaultBinding = serde_json::from_value(input["binding"].clone())
        .map_err(|_| worker::Error::RustError("invalid email fixture binding".into()))?;
    let now = input["now"]
        .as_i64()
        .ok_or_else(|| worker::Error::RustError("invalid email fixture time".into()))?;
    let storage = Arc::new(ghinvite_storage_d1::D1Storage::new(env.d1("DB")?));
    if operation.contains("/delivery/") {
        let result: storage::Result<serde_json::Value> = async {
            let command: ghinvite_core::email_delivery::EmailCreateCommand = serde_json::from_value(input["command"].clone()).map_err(|_| storage::email_delivery::invalid())?;
            let attempt=ghinvite_core::email_delivery::EmailAttempt{create:command.clone()};
            if operation.ends_with("/claim") {
                let generation=storage.claim_email_attempt(&attempt).await?;
                if input["lose_ack"]==true {return Err(storage::email_delivery::unavailable());}
                Ok(serde_json::json!({"generation":generation}))
            } else if operation.ends_with("/exists") {
                Ok(serde_json::json!({"exists":storage.email_attempt_exists(&attempt).await?}))
            } else if operation.ends_with("/reject") {
                storage.reject_email_attempt(&attempt,input["generation"].as_u64().ok_or_else(storage::email_delivery::invalid)?).await?;
                Ok(serde_json::json!({"released":true}))
            } else if operation.ends_with("/project") {
                let snapshot=serde_json::from_value(input["snapshot"].clone()).map_err(|_|storage::email_delivery::invalid())?;
                storage.project_email_delivery(&snapshot).await?;
                Ok(serde_json::json!({"projected":true}))
            } else if operation.ends_with("/read") {
                Ok(serde_json::json!({"snapshot":storage.email_delivery(&command.storage_id()).await?,"batch":storage.batch_email_deliveries(command.account_id,command.batch.batch_id).await?}))
            } else {Err(storage::email_delivery::invalid())}
        }.await;
        return Ok(match result {
            Ok(v) => Json(v).into_response(),
            Err(_) => (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({"error":"email delivery fixture unavailable"})),
            )
                .into_response(),
        });
    }
    let keyring = if let Ok(descriptor) = env.secret("P1_EMAIL_KEYRING") {
        ghinvite_config::bulk::email_keyring(true, Some(descriptor.to_string()))
            .map_err(|_| worker::Error::RustError("invalid fixture keyring".into()))?
            .ok_or_else(|| worker::Error::RustError("missing fixture keyring".into()))?
    } else {
        ghinvite_web::config::parse_keyring(&env.secret("P1_KEYRING")?.to_string())
            .map_err(|_| worker::Error::RustError("invalid fixture keyring".into()))?
    };
    let backend: Arc<dyn EmailVaultStorage> = Arc::new(LostAck {
        inner: storage.clone(),
        lose: input["lose_ack"] == true,
    });
    let vault = EmailVault::new(backend, keyring);
    let result: Result<serde_json::Value, Error> = if operation.ends_with("/stage") {
        match input["email"]
            .as_str()
            .and_then(|s| ghinvite_email_vault::Address::parse(s).ok())
        {
            Some(address) => vault
                .stage(&binding, &address, now)
                .await
                .map(|()| serde_json::json!({"stored":true})),
            None => Err(Error::Unavailable),
        }
    } else if operation.ends_with("/load") {
        vault.load_active(&binding, now).await.and_then(|address| {
            if input["expected"]
                .as_str()
                .is_some_and(|expected| expected != address.as_str())
            {
                return Err(Error::Conflict);
            }
            Ok(serde_json::json!({"loaded":true}))
        })
    } else if operation.ends_with("/activate") {
        storage
            .activate_email(&binding, now)
            .await
            .map(|changed| serde_json::json!({"changed":changed}))
            .map_err(|_| Error::Unavailable)
    } else if operation.ends_with("/retire") {
        storage
            .retire_email(&binding, now)
            .await
            .map(|changed| serde_json::json!({"changed":changed}))
            .map_err(|_| Error::Unavailable)
    } else if operation.ends_with("/prune") {
        storage
            .prune_emails(now)
            .await
            .map(|count| serde_json::json!({"count":count}))
            .map_err(|_| Error::Unavailable)
    } else {
        return Ok(StatusCode::NOT_FOUND.into_response());
    };
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
