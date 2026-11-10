//! A plain reqwest client with octotoken's installation tokens.
//!
//! ```sh
//! GITHUB_APP_ID=123 GITHUB_APP_KEY=app.pem GITHUB_INSTALLATION_ID=42 GITHUB_ORG=acme \
//!     cargo run -p octotoken --example reqwest
//! ```
//!
//! - **The client**: no redirects (a redirect could re-send a write
//!   elsewhere), a timeout, GitHub's headers.
//! - **Errors**: any non-success response goes through [`ApiError::classify`],
//!   which keeps fixed codes and GitHub's rate-limit wait, never the body's
//!   text. Transport errors are reduced to a fixed value too: reqwest's
//!   include the URL.
//! - **401** handled by [`with_token`]: one replacement token, one retry.

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
    /// No complete answer: the request may or may not have reached GitHub.
    Transport,
    /// A success response that was not the expected JSON.
    Decode,
}

fn client() -> reqwest::Result<reqwest::Client> {
    reqwest::Client::builder()
        .user_agent("octotoken-example/1")
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(30))
        .build()
}

#[derive(serde::Deserialize)]
struct Invitation {
    login: Option<String>,
    email: Option<String>,
}

/// The organization's pending invitations, as `login`s or emails.
async fn pending_invitations(
    tokens: &impl InstallationTokenProvider,
    client: &reqwest::Client,
    base_url: &str,
    installation: InstallationId,
    org: &str,
) -> Result<Vec<String>, WithTokenError<CallError>> {
    let scope = TokenScope::from(
        Permissions::new([(OrganizationPermission::Members, Access::Read)])
            .expect("a valid permission set"),
    );
    let url = format!("{base_url}/orgs/{org}/invitations");
    let unauthorized = |e: &CallError| matches!(e, CallError::Api(e) if e.is_unauthorized());
    with_token(tokens, installation, &scope, unauthorized, |token| {
        let request = client
            .get(&url)
            .bearer_auth(token.expose_secret())
            .header("accept", "application/vnd.github+json")
            .header("x-github-api-version", octoauth::DEFAULT_API_VERSION);
        async move {
            let response = request.send().await.map_err(|_| CallError::Transport)?;
            let (status, headers) = (response.status(), response.headers().clone());
            let body = response.bytes().await.map_err(|_| CallError::Transport)?;
            if !status.is_success() {
                let error = ApiError::classify(status, &headers, ErrorBody::Bytes(&body));
                return Err(CallError::Api(error));
            }
            let invitations: Vec<Invitation> =
                serde_json::from_slice(&body).map_err(|_| CallError::Decode)?;
            Ok(invitations
                .into_iter()
                .map(|i| i.login.or(i.email).unwrap_or_else(|| "?".into()))
                .collect())
        }
    })
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
    let client = client()?;

    let base_url = octoauth::DEFAULT_BASE_URL;
    match pending_invitations(&tokens, &client, base_url, installation, &org).await {
        Ok(invitations) => println!("{invitations:?}"),
        // Fixed codes only: safe to log.
        Err(WithTokenError::Token(error)) => eprintln!("no token: {error}"),
        Err(WithTokenError::Call(CallError::Api(error))) => match error.suggested_wait() {
            Some(wait) => eprintln!("GitHub: {error}; retry in {wait:?}"),
            None => eprintln!("GitHub: {error}"),
        },
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

    /// Rejects `ghs_1` as revoked, lists one invitation for `ghs_2`, and is
    /// rate limited for any later token.
    struct Invitations;

    impl Respond for Invitations {
        fn respond(&self, request: &Request) -> ResponseTemplate {
            match request.headers["authorization"].to_str().unwrap() {
                "Bearer ghs_1" => ResponseTemplate::new(401)
                    .set_body_json(serde_json::json!({"message": "Bad credentials"})),
                "Bearer ghs_2" => ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!([{"login": "octocat", "email": null}])),
                _ => ResponseTemplate::new(403)
                    .insert_header("retry-after", "42")
                    .set_body_json(serde_json::json!({
                        "message": "You have exceeded a secondary rate limit"
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
        let invitations = pending_invitations(
            &tokens,
            &client().unwrap(),
            &server.uri(),
            InstallationId::new(42),
            "acme",
        )
        .await
        .unwrap();
        assert_eq!(invitations, ["octocat"]);
    }

    #[tokio::test]
    async fn rate_limits_come_back_with_githubs_wait() {
        let server = github().await;
        let tokens = provider(&server);
        let scope: TokenScope = Permissions::new([(OrganizationPermission::Members, Access::Read)])
            .unwrap()
            .into();
        let first = tokens.token(InstallationId::new(42), &scope).await.unwrap();
        let second = tokens.replace(&first).await.unwrap();
        tokens.replace(&second).await.unwrap();

        let error = pending_invitations(
            &tokens,
            &client().unwrap(),
            &server.uri(),
            InstallationId::new(42),
            "acme",
        )
        .await
        .unwrap_err();
        let WithTokenError::Call(CallError::Api(error)) = error else {
            panic!("expected an API error, got {error:?}");
        };
        assert_eq!(error.suggested_wait(), Some(Duration::from_secs(42)));
        assert_eq!(error.code(), "gh:api:rate_limited:secondary");
    }
}
