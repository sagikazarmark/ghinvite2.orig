//! The public API as a downstream crate sees it: only re-exported items, an
//! externally implemented signer, and the feature-gated builder method. No
//! network; behaviour is covered by the crate's unit tests.

use octoauth::{
    Access, AccountKind, ApiError, App, AppCredential, AppId, AppKeySigner, ConfigError, Error,
    ErrorBody, ForbiddenReason, Installation, InstallationAccount, InstallationId,
    InstallationInfo, InstallationToken, OrganizationPermission, Permissions, RateLimitScope,
    RepositoryPermission, RepositorySelection, SigningError, SigningKey, ValidationCode,
};
use std::sync::Arc;

/// A KMS-style signer implemented outside the crate.
struct Kms;

impl AppKeySigner for Kms {
    fn sign_rs256(&self, _: &[u8]) -> Result<Vec<u8>, SigningError> {
        Err(SigningError::Failed)
    }
}

#[test]
fn builds_apps_from_public_items() -> Result<(), ConfigError> {
    let pem = include_str!("fixtures/rsa2048.pem");
    let app = App::new(7, SigningKey::from_pem(pem)?);
    assert_eq!(app.id(), AppId::new(7));

    let shared: Arc<dyn AppKeySigner> = Arc::new(Kms);
    let app = App::builder(7, shared)
        .client_id("Iv23.example")
        .client(reqwest::Client::new())
        .user_agent("downstream/1.0")
        .base_url("https://ghe.example.com/api/v3")
        .build()?;
    let installation = app.installation(42);
    assert_eq!(installation.id(), InstallationId::new(42));

    let _permissions = Permissions::builder()
        .grant(RepositoryPermission::Metadata, Access::Read)
        .grant(OrganizationPermission::Members, Access::Write)
        .build()?;
    Ok(())
}

#[cfg(feature = "middleware")]
#[test]
fn accepts_a_middleware_client() -> Result<(), ConfigError> {
    let client = reqwest_middleware::ClientBuilder::new(reqwest::Client::new()).build();
    App::builder(7, Kms).middleware_client(client).build()?;
    Ok(())
}

/// `InstallationToken` and `Error` are usable as plain public types.
#[allow(dead_code)]
fn signatures(token: &InstallationToken, error: &Error) {
    let _: std::time::SystemTime = token.expires_at();
    let _: &'static str = error.code();
    let _: &str = token.expose_secret();
}

/// The app's credential is lent without I/O; a failing signer fails it.
#[test]
fn lends_the_app_credential() -> Result<(), Box<dyn std::error::Error>> {
    let pem = include_str!("fixtures/rsa2048.pem");
    let app = App::new(7, SigningKey::from_pem(pem)?);
    let credential: AppCredential = app.credential()?;
    assert_eq!(credential.app_id(), AppId::new(7));
    assert!(credential.expose_secret().starts_with("eyJ"));
    assert_eq!(
        format!("{credential:?}"),
        "AppCredential { app_id: AppId(7), .. }"
    );
    assert_eq!(
        App::new(7, Kms).credential().unwrap_err(),
        Error::Signing(SigningError::Failed)
    );
    Ok(())
}

/// `InstallationInfo` is read through public fields; the enums are
/// non-exhaustive.
#[allow(dead_code)]
async fn reads_installations(installation: &Installation) -> Result<bool, Error> {
    let info: InstallationInfo = installation.get().await?;
    let _: InstallationId = info.id;
    let _: AppId = info.app_id;
    let account: &InstallationAccount = &info.account;
    let _: (u64, &str) = (account.id, &account.login);
    let _: Option<std::time::SystemTime> = info.suspended_at;
    let _all = matches!(info.repository_selection, RepositorySelection::All);
    let organization = matches!(account.kind, AccountKind::Organization);
    Ok(organization && info.permission("members").is_some() && info.permissions.len() > 1)
}

/// reqwest's status and header types are the ones `ApiError::classify` takes.
#[test]
fn classifies_reqwest_parts() {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert("retry-after", "30".parse().unwrap());
    let error = ApiError::classify(
        reqwest::StatusCode::FORBIDDEN,
        &headers,
        ErrorBody::Bytes(br#"{"message":"You have exceeded a secondary rate limit"}"#),
    );
    assert_eq!(
        error,
        ApiError::RateLimited {
            scope: RateLimitScope::Secondary,
            retry_after: Some(std::time::Duration::from_secs(30)),
        }
    );
    let error = ApiError::classify(
        reqwest::StatusCode::UNPROCESSABLE_ENTITY,
        &reqwest::header::HeaderMap::new(),
        ErrorBody::Parsed {
            message: Some("Validation Failed"),
            errors: &[serde_json::json!({"code": "already_exists"})],
        },
    );
    assert_eq!(
        error,
        ApiError::Validation {
            codes: vec![ValidationCode::AlreadyExists]
        }
    );
    assert_eq!(
        ApiError::classify(
            reqwest::StatusCode::FORBIDDEN,
            &reqwest::header::HeaderMap::new(),
            ErrorBody::Empty
        ),
        ApiError::Forbidden {
            reason: ForbiddenReason::Other
        }
    );
}
