//! The consumer contract as a downstream crate sees it: a fake provider, and
//! generic consumers that spawn work natively.

use octotoken::{
    Access, ApiError, AppId, AppTokenProvider, DynInstallationTokenProvider, InstallationId,
    InstallationToken, InstallationTokenProvider, OrganizationPermission, Permissions,
    ProviderRegistry, TokenError, TokenParts, TokenScope, WithTokenError, octoauth, with_token,
};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

/// A provider for consumers' tests: always the same token.
struct Fixed(InstallationToken);

impl InstallationTokenProvider for Fixed {
    async fn token(
        &self,
        _: InstallationId,
        _: &TokenScope,
    ) -> Result<InstallationToken, TokenError> {
        Ok(self.0.clone())
    }

    async fn replace(&self, _: &InstallationToken) -> Result<InstallationToken, TokenError> {
        Err(TokenError::ForeignToken)
    }
}

fn scope() -> TokenScope {
    Permissions::new([(OrganizationPermission::Members, Access::Read)])
        .unwrap()
        .into()
}

fn fixed() -> Fixed {
    Fixed(
        InstallationToken::from_parts(TokenParts {
            secret: "ghs_fixed".into(),
            app_id: AppId::new(7),
            installation_id: InstallationId::new(42),
            scope: scope(),
            expires_at: SystemTime::now() + Duration::from_secs(3600),
            issued_at: None,
        })
        .unwrap(),
    )
}

/// A consumer generic over any provider, spawning on a multi-threaded runtime.
async fn spawned<P: InstallationTokenProvider + 'static>(provider: Arc<P>) -> String {
    tokio::spawn(async move {
        let token = provider
            .token(InstallationId::new(42), &scope())
            .await
            .unwrap();
        token.expose_secret().to_owned()
    })
    .await
    .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn fake_providers_work_with_generic_consumers() {
    assert_eq!(spawned(Arc::new(fixed())).await, "ghs_fixed");
}

/// `with_token` with a generic provider, spawned: its future is `Send`. The
/// call fails with `failure`, or returns the token.
async fn spawned_call<P: InstallationTokenProvider + 'static>(
    provider: Arc<P>,
    failure: Option<ApiError>,
) -> Result<String, WithTokenError<ApiError>> {
    tokio::spawn(async move {
        let scope = scope();
        with_token(
            &*provider,
            InstallationId::new(42),
            &scope,
            ApiError::is_unauthorized,
            |token| {
                let answer = match failure.clone() {
                    Some(error) => Err(error),
                    None => Ok(token.expose_secret().to_owned()),
                };
                async move { answer }
            },
        )
        .await
    })
    .await
    .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn with_token_works_with_generic_consumers() {
    let provider = Arc::new(fixed());
    assert_eq!(
        spawned_call(provider.clone(), None).await,
        Ok("ghs_fixed".to_owned())
    );
    assert_eq!(
        spawned_call(provider.clone(), Some(ApiError::NotFound)).await,
        Err(WithTokenError::Call(ApiError::NotFound))
    );
    // The fake provider cannot replace: the 401 surfaces as its error.
    assert_eq!(
        spawned_call(provider, Some(ApiError::Unauthorized)).await,
        Err(WithTokenError::Token(TokenError::ForeignToken))
    );
}

/// The real provider satisfies the same bounds (compile-time check).
#[allow(dead_code)]
fn real_provider_is_spawnable(provider: Arc<AppTokenProvider>) {
    drop(tokio::spawn(spawned(provider.clone())));
    drop(tokio::spawn(spawned_call(
        provider,
        Some(ApiError::NotFound),
    )));
}

/// Any provider, held without a type parameter: `Send` and `'static`, so a
/// handler can spawn work with it, and usable with `with_token`.
#[tokio::test(flavor = "multi_thread")]
async fn dyn_providers_work_with_spawned_consumers() {
    let provider: Arc<dyn DynInstallationTokenProvider> = Arc::new(fixed());
    let secret = tokio::spawn({
        let provider = provider.clone();
        async move {
            let token =
                DynInstallationTokenProvider::token(&*provider, InstallationId::new(42), &scope())
                    .await
                    .unwrap();
            token.expose_secret().to_owned()
        }
    })
    .await
    .unwrap();
    assert_eq!(secret, "ghs_fixed");

    let result = tokio::spawn(async move {
        let scope = scope();
        with_token(
            &*provider,
            InstallationId::new(42),
            &scope,
            ApiError::is_unauthorized,
            |_| async { Err::<(), _>(ApiError::Unauthorized) },
        )
        .await
    })
    .await
    .unwrap();
    assert_eq!(result, Err(WithTokenError::Token(TokenError::ForeignToken)));
}

/// A registry in a `static`, as a Worker keeps it.
#[test]
fn registries_live_in_statics() {
    static PROVIDERS: ProviderRegistry = ProviderRegistry::new();
    let pem = include_str!("fixtures/rsa2048.pem");
    let app = octoauth::App::new(9, octoauth::SigningKey::from_pem(pem).unwrap());
    let provider = PROVIDERS.provider(&app);
    let dyn_provider: Arc<dyn DynInstallationTokenProvider> = Arc::new(PROVIDERS.provider(&app));
    drop(dyn_provider);
    assert_eq!(provider.app().id(), AppId::new(9));
}

#[test]
fn provider_is_built_from_an_app() {
    let pem = include_str!("fixtures/rsa2048.pem");
    let app = octoauth::App::new(7, octoauth::SigningKey::from_pem(pem).unwrap());
    let provider = AppTokenProvider::builder(app)
        .min_remaining(Duration::from_secs(600))
        .build()
        .unwrap();
    assert_eq!(provider.app().id(), AppId::new(7));
}
