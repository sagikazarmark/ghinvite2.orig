//! The stateful stub, driven through the real typed endpoints over HTTP with
//! an octoauth installation token (gh-toolkit drove it through its broker).
use super::*;
use crate::{
    RepoRole,
    endpoints::{self, CollaboratorWrite, Repository},
    error::ApiError,
    mocks,
    rest::{PathTemplate, Rest, RestEnv},
    transport::{Method, ReqwestTransport},
};

async fn serve() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        axum::serve(listener, router()).await.unwrap();
    });
    base
}

fn env(base: &str) -> RestEnv {
    RestEnv::new(
        Arc::new(ReqwestTransport::new().unwrap()),
        base,
        "stub-test",
    )
    .unwrap()
}

const fn repo(name: &'static str) -> Repository<'static> {
    Repository {
        owner: "acme",
        name,
    }
}

async fn is_collaborator(rest: &Rest<'_>, name: &str, login: &str) -> Result<bool, ApiError> {
    let path = PathTemplate::new("/repos/{}/{}/collaborators/{}")
        .seg("acme")
        .seg(name)
        .seg(login);
    match rest.call::<(), ()>(Method::Get, path, &[], None).await {
        Ok(response) if response.status == 204 => Ok(true),
        Err(ApiError::NotFound) => Ok(false),
        Ok(_) => Err(ApiError::Decode {
            what: "collaborator_status",
        }),
        Err(e) => Err(e),
    }
}

#[tokio::test]
async fn controlled_outcomes_record_safe_calls_and_retry_once() {
    let base = serve().await;
    let http = reqwest::Client::new();
    let env = env(&base);
    let credential = mocks::installation_credential(123, 1, "test-token-for-1");
    let rest = Rest::new(&env, &credential);
    for (repo, outcome) in [
        ("member", "already_collaborator"),
        ("denied", "terminal_failure"),
        ("retry", "transient_once"),
        ("throttled", "throttled_once"),
    ] {
        let response = http
            .post(format!("{base}/outcomes"))
            .json(&json!({
                "owner": "acme", "repo": repo, "user": "alice", "outcome": outcome
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
    }
    let add = |name: &'static str| {
        let rest = &rest;
        async move { endpoints::put_collaborator(rest, repo(name), "alice", &RepoRole::Read).await }
    };
    let answer = add("member").await;
    assert!(
        matches!(answer, Ok(CollaboratorWrite::NoContent)),
        "{answer:?}"
    );
    assert!(is_collaborator(&rest, "member", "alice").await.unwrap());
    let answer = add("denied").await;
    assert!(
        matches!(answer, Err(ApiError::Validation { .. })),
        "{answer:?}"
    );
    let answer = add("retry").await;
    assert!(
        matches!(answer, Err(ApiError::Server { status: 502 })),
        "{answer:?}"
    );
    let answer = add("retry").await;
    assert!(
        matches!(answer, Ok(CollaboratorWrite::Created { .. })),
        "{answer:?}"
    );
    // The throttled refusal is a 403 like an access refusal, but its
    // documented evidence separates it from a permission decision.
    let answer = add("throttled").await;
    assert!(
        matches!(
            answer,
            Err(ApiError::RateLimited { retry_after, .. })
                if retry_after == Some(std::time::Duration::from_secs(1))
        ),
        "{answer:?}"
    );
    let answer = add("throttled").await;
    assert!(
        matches!(answer, Ok(CollaboratorWrite::Created { .. })),
        "{answer:?}"
    );
    let calls: serde_json::Value = http
        .get(format!("{base}/calls"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let puts: Vec<_> = calls["requests"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["method"] == "PUT")
        .collect();
    assert_eq!(
        puts,
        vec![
            &json!({"method":"PUT", "path":"/repos/acme/member/collaborators/alice", "permission":"pull", "status":204}),
            &json!({"method":"PUT", "path":"/repos/acme/denied/collaborators/alice", "permission":"pull", "status":422}),
            &json!({"method":"PUT", "path":"/repos/acme/retry/collaborators/alice", "permission":"pull", "status":502}),
            &json!({"method":"PUT", "path":"/repos/acme/retry/collaborators/alice", "permission":"pull", "status":201}),
            &json!({"method":"PUT", "path":"/repos/acme/throttled/collaborators/alice", "permission":"pull", "status":403}),
            &json!({"method":"PUT", "path":"/repos/acme/throttled/collaborators/alice", "permission":"pull", "status":201}),
        ]
    );
    assert!(!calls.to_string().contains("test-token"));
    assert!(!calls.to_string().contains("Bearer"));
}

#[tokio::test]
async fn successful_invitation_is_pending_and_can_be_listed_and_deleted() {
    let base = serve().await;
    let env = env(&base);
    let credential = mocks::installation_credential(123, 1, "test-token-for-1");
    let rest = Rest::new(&env, &credential);
    let Ok(CollaboratorWrite::Created {
        invitation_id: id, ..
    }) = endpoints::put_collaborator(&rest, repo("api"), "alice", &RepoRole::Write).await
    else {
        panic!("stub must return a decodable 201 invitation");
    };
    assert!(id > 0);
    assert!(!is_collaborator(&rest, "api", "alice").await.unwrap());
    let pending = endpoints::list_repo_invitations(&rest, repo("api"))
        .await
        .unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].id, id);
    assert_eq!(pending[0].invitee.as_ref().unwrap().login, "alice");
    assert_eq!(pending[0].permissions, RepoRole::Write);
    endpoints::delete_repo_invitation(&rest, repo("api"), id)
        .await
        .unwrap();
    assert!(
        endpoints::list_repo_invitations(&rest, repo("api"))
            .await
            .unwrap()
            .is_empty()
    );
    assert!(matches!(
        endpoints::delete_repo_invitation(&rest, repo("api"), id).await,
        Err(ApiError::NotFound)
    ));
}

#[tokio::test]
async fn installation_reads_decode_through_typed_endpoints() {
    let base = serve().await;
    let env = env(&base);
    let app = mocks::app_credential(123);
    let installation = endpoints::get_installation(&Rest::new(&env, &app), 42)
        .await
        .unwrap();
    assert_eq!((installation.id, installation.app_id), (42, 123));
    assert_eq!(
        installation.account.account_type.as_deref(),
        Some("Organization")
    );
    let credential = mocks::installation_credential(123, 42, "test-token-for-42");
    let rest = Rest::new(&env, &credential);
    let repos = endpoints::list_installation_repos(&rest).await.unwrap();
    assert_eq!(
        repos
            .iter()
            .map(|r| r.full_name.as_str())
            .collect::<Vec<_>>(),
        ["acme/api", "acme/web"]
    );
    let repository = endpoints::get_repository(&rest, repo("api")).await.unwrap();
    assert_eq!(repository.id, 10);
    assert_eq!(
        endpoints::get_user_by_id(&rest, 8).await.unwrap().login,
        "alice"
    );
}

/// The stub's token route answers in the shape octoauth parses, so octoauth's
/// own `App` can create tokens against it.
#[tokio::test]
async fn octoauth_creates_installation_tokens_against_the_stub() {
    struct FixedSigner;
    impl octoauth::AppKeySigner for FixedSigner {
        fn sign_rs256(&self, _: &[u8]) -> Result<Vec<u8>, octoauth::SigningError> {
            Ok(vec![7; 256])
        }
    }
    let base = serve().await;
    let app = octoauth::App::builder(123, FixedSigner)
        .base_url(base.clone())
        .build()
        .unwrap();
    let permissions = octoauth::Permissions::builder()
        .grant(
            octoauth::RepositoryPermission::Administration,
            octoauth::Access::Write,
        )
        .grant(
            octoauth::RepositoryPermission::Metadata,
            octoauth::Access::Read,
        )
        .build()
        .unwrap();
    let token = app
        .installation(42)
        .access_token(&permissions)
        .await
        .unwrap();
    assert_eq!(token.expose_secret(), "test-token-for-42");
    assert_eq!(token.installation_id(), octoauth::InstallationId::new(42));
    assert!(token.expires_at() > std::time::SystemTime::now());
    // And the token works with this crate's REST client.
    let env = env(&base);
    let credential = crate::InstallationCredential::new(token);
    let repos = endpoints::list_installation_repos(&Rest::new(&env, &credential))
        .await
        .unwrap();
    assert_eq!(repos.len(), 2);
}
