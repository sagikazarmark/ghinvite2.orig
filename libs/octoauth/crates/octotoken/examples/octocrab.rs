//! An octocrab client with octotoken's installation tokens.
//!
//! ```sh
//! GITHUB_APP_ID=123 GITHUB_APP_KEY=app.pem GITHUB_INSTALLATION_ID=42 GITHUB_ORG=acme \
//!     cargo run -p octotoken --example octocrab
//! ```
//!
//! octocrab is only a dev-dependency here: octoauth and octotoken work with
//! any client. What this example shows:
//!
//! - **One base client, no auth.** Each call derives a client for its token
//!   with `Octocrab::user_access_token`, which is cheap. Configuring auth on
//!   the base client and then deriving would send two `Authorization` headers.
//! - **No automatic retries.** octocrab's default `retry` feature retries
//!   5xx, 429 and transport errors immediately, POSTs included; a retried
//!   invitation is a duplicate invitation. Leave the feature off (as here) or
//!   configure `RetryConfig::None`. Rate limits come back classified, with
//!   GitHub's requested wait.
//! - **No redirects**: the `follow-redirect` feature is off.
//! - **Errors converted at the boundary.** `octocrab::Error`'s `Debug` carries
//!   GitHub's message (or the raw body) and every response header. Convert it
//!   to [`ApiError`] before it reaches a log, a durable journal or a webhook
//!   handler's error (ADR 0003).
//! - **401 handled by [`with_token`]**: one replacement token, one retry.
//!
//! octocrab buffers response bodies with no size limit; behind an untrusted
//! proxy, cap them in a custom service (`OctocrabBuilder::with_service`).

use octocrab::Octocrab;
use octotoken::{
    Access, ApiError, AppTokenProvider, ErrorBody, InstallationId, InstallationTokenProvider,
    OrganizationPermission, Permissions, TokenScope, WithTokenError, octoauth, with_token,
};
use std::time::Duration;

/// Why a call failed, without any upstream text.
#[derive(Debug)]
enum CallError {
    /// GitHub answered with an error.
    Api(ApiError),
    /// No (complete) answer, or one octocrab could not decode. The details
    /// are octocrab's, which may quote the response, so they are dropped.
    Client,
}

impl From<octocrab::Error> for CallError {
    fn from(error: octocrab::Error) -> Self {
        classify(&error).map_or(Self::Client, Self::Api)
    }
}

/// The bridge from octocrab's error to [`ApiError`]. octocrab hands over the
/// body already parsed; when the body was not JSON it puts the raw text in
/// `message`, which then simply matches no template.
fn classify(error: &octocrab::Error) -> Option<ApiError> {
    let octocrab::Error::GitHub { source, .. } = error else {
        return None;
    };
    let headers = source.headers.clone().unwrap_or_default();
    Some(ApiError::classify(
        source.status_code,
        &headers,
        ErrorBody::Parsed {
            message: Some(&source.message),
            errors: source.errors.as_deref().unwrap_or_default(),
        },
    ))
}

/// The base client: no auth, no retries, bounded connect and read times.
fn base_client(base_uri: Option<&str>) -> octocrab::Result<Octocrab> {
    let mut builder = Octocrab::builder()
        .set_connect_timeout(Some(Duration::from_secs(10)))
        .set_read_timeout(Some(Duration::from_secs(30)));
    if let Some(base_uri) = base_uri {
        builder = builder.base_uri(base_uri)?;
    }
    builder.build()
}

/// The organization's pending invitations, as `login`s or emails.
async fn pending_invitations(
    tokens: &impl InstallationTokenProvider,
    base: &Octocrab,
    installation: InstallationId,
    org: &str,
) -> Result<Vec<String>, WithTokenError<CallError>> {
    let scope = TokenScope::from(
        Permissions::new([(OrganizationPermission::Members, Access::Read)])
            .expect("a valid permission set"),
    );
    let unauthorized = |e: &CallError| matches!(e, CallError::Api(e) if e.is_unauthorized());
    with_token(
        tokens,
        installation,
        &scope,
        unauthorized,
        |token| async move {
            let github = base.user_access_token(token.expose_secret().to_owned())?;
            let page = github.orgs(org).invitations().list().send().await?;
            Ok(page
                .items
                .into_iter()
                .map(|invitation| {
                    invitation
                        .login
                        .or(invitation.email)
                        .unwrap_or_else(|| "?".into())
                })
                .collect())
        },
    )
    .await
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let var = |name: &str| std::env::var(name).map_err(|_| format!("set {name}"));
    let app_id: u64 = var("GITHUB_APP_ID")?.parse()?;
    let key = std::fs::read_to_string(var("GITHUB_APP_KEY")?)?;
    let installation = InstallationId::new(var("GITHUB_INSTALLATION_ID")?.parse()?);
    let org = var("GITHUB_ORG")?;

    let app = octoauth::App::new(app_id, octoauth::SigningKey::from_pem(&key)?);
    // One provider per app for the life of the process.
    let tokens = AppTokenProvider::new(app);
    let base = base_client(None)?;

    match pending_invitations(&tokens, &base, installation, &org).await {
        Ok(invitations) => println!("{invitations:?}"),
        // Fixed codes only: safe to log.
        Err(WithTokenError::Token(error)) => eprintln!("no token: {error}"),
        Err(WithTokenError::Call(CallError::Api(error))) => eprintln!("GitHub: {error}"),
        Err(error) => eprintln!("failed: {error:?}"),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

    const PEM: &str = include_str!("../tests/fixtures/rsa2048.pem");

    /// Creates `ghs_1`, `ghs_2`, … each a second later than the last.
    struct Tokens(AtomicUsize);

    impl Respond for Tokens {
        fn respond(&self, _: &Request) -> ResponseTemplate {
            let n = self.0.fetch_add(1, SeqCst) + 1;
            let expires_at = chrono::Utc::now() + chrono::Duration::seconds(3600 + n as i64);
            ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "token": format!("ghs_{n}"),
                "expires_at": expires_at.to_rfc3339(),
            }))
        }
    }

    /// Rejects `ghs_1` as revoked; lists one invitation for any other token.
    struct Invitations;

    impl Respond for Invitations {
        fn respond(&self, request: &Request) -> ResponseTemplate {
            match request.headers["authorization"].to_str().unwrap() {
                "Bearer ghs_1" => ResponseTemplate::new(401)
                    .set_body_json(serde_json::json!({"message": "Bad credentials"})),
                "Bearer ghs_2" => ResponseTemplate::new(200).set_body_json(serde_json::json!([
                    {"id": 1, "login": "octocat", "email": null, "role": "direct_member",
                     "created_at": "2026-10-01T00:00:00Z", "failed_at": null, "failed_reason": null,
                     "inviter": null, "team_count": 0, "node_id": "x",
                     "invitation_teams_url": "https://api.github.com/x", "invitation_source": "member"}
                ])),
                _ => ResponseTemplate::new(403).set_body_json(serde_json::json!({
                    "message": "Although you appear to have the correct authorization credentials, the `acme` organization has an IP allow list enabled, and your IP address is not permitted to access this resource."
                })),
            }
        }
    }

    async fn github() -> MockServer {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/app/installations/42/access_tokens"))
            .respond_with(Tokens(AtomicUsize::new(0)))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/orgs/acme/invitations"))
            .respond_with(Invitations)
            .mount(&server)
            .await;
        server
    }

    fn provider(server: &MockServer) -> AppTokenProvider {
        let app = octoauth::App::builder(7, octoauth::SigningKey::from_pem(PEM).unwrap())
            .base_url(server.uri())
            .build()
            .unwrap();
        AppTokenProvider::new(app)
    }

    #[tokio::test]
    async fn a_revoked_token_is_replaced_once() {
        let server = github().await;
        let tokens = provider(&server);
        let base = base_client(Some(&server.uri())).unwrap();
        let invitations = pending_invitations(&tokens, &base, InstallationId::new(42), "acme")
            .await
            .unwrap();
        assert_eq!(invitations, ["octocat"]);

        // Exactly one Authorization header per request.
        let requests = server.received_requests().await.unwrap();
        let api: Vec<_> = requests
            .iter()
            .filter(|r| r.url.path() == "/orgs/acme/invitations")
            .map(|r| r.headers.get_all("authorization").iter().count())
            .collect();
        assert_eq!(api, [1, 1]);
    }

    #[tokio::test]
    async fn errors_are_classified_without_their_text() {
        let server = github().await;
        let tokens = provider(&server);
        let base = base_client(Some(&server.uri())).unwrap();
        // Burn `ghs_1` and `ghs_2` so the next token is refused by the IP allow list.
        let scope: TokenScope = Permissions::new([(OrganizationPermission::Members, Access::Read)])
            .unwrap()
            .into();
        let first = tokens.token(InstallationId::new(42), &scope).await.unwrap();
        let second = tokens.replace(&first).await.unwrap();
        tokens.replace(&second).await.unwrap();

        let error = pending_invitations(&tokens, &base, InstallationId::new(42), "acme")
            .await
            .unwrap_err();
        let WithTokenError::Call(CallError::Api(error)) = error else {
            panic!("expected an API error, got {error:?}");
        };
        assert_eq!(
            error,
            ApiError::Forbidden {
                reason: octotoken::ForbiddenReason::IpAllowList
            }
        );
        let rendered = format!("{error} {error:?}");
        assert!(!rendered.contains("acme"), "{rendered}");
    }
}
