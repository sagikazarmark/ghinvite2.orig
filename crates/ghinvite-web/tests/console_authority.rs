//! Router proof: delegated identity is insufficient unless every link target
//! is administered. Uses real SQL queries, installation broker, and ingress seam.
mod common;
use axum::{
    body::Body,
    http::{Request, StatusCode},
    response::Response,
};
use chrono::Utc;
use common::authority_http_fixture::FakeLinkAuthority;
use common::sign_in::{
    ConsoleTransport, OCTOCAT, console_config, oauth_expectations, repo_identity, sign_in,
};
use ghinvite_core::{
    Account, AccountType, InvitationLink, InvitationLinkId, SelectedRepos,
    account::{AccountSettings, Installation},
    storage::{AppInstallationStorage, RecordStorage, projection::fixture::Seed},
    targets::{RepoRole, RepoTarget, Targets},
};
use ghinvite_github::{Method, mocks::Expectation};
use http_body_util::BodyExt;
use std::sync::Arc;
use tower::ServiceExt;

fn link(ids: &[u64], description: &str) -> InvitationLink {
    InvitationLink {
        batch_source: None,
        access_lifetime_seconds: None,
        email_domains: vec![],
        id: InvitationLinkId::new(),
        organization_installation_id: None,
        installation_id: 77,
        account_id: 9001,
        created_by: 7,
        created_at: Utc::now(),
        expires_at: None,
        max_uses: Some(1),
        invitation_lifetime_seconds: 604800,
        abuse_threshold_per_hour: None,
        budget_share_pct: 50,
        uses_count: 0,
        approval_required: true,
        description: description.into(),
        internal_note: None,
        revoked_at: None,
        revoked_by: None,
        pause: None,
        secret: common::capability::state(),
        targets: Targets {
            repos: ids
                .iter()
                .map(|id| RepoTarget {
                    repo_id: *id,
                    repo_full_name: format!("acme/r{id}"),
                    grant: RepoRole::Read,
                })
                .collect(),
            ..Default::default()
        },
    }
}
fn permission(id: u64, role: &str) -> Expectation {
    Expectation::ok_json(
        Method::Get,
        format!("https://api.github.com/repos/acme/r{id}/collaborators/octocat/permission"),
        serde_json::json!({"user":{"id":42,"login":"octocat"},"role_name":role}),
    )
}
fn repository_page(ids: &[u64]) -> Expectation {
    Expectation::ok_json(
        Method::Get,
        "https://api.github.com/installation/repositories?per_page=100",
        serde_json::json!({"total_count":ids.len(),"repositories":ids.iter().map(|id|
            serde_json::json!({"id":id,"full_name":format!("acme/r{id}"),"owner":{"id":9001,"login":"acme"}})
        ).collect::<Vec<_>>()}),
    )
}
async fn fixture(
    delegation: bool,
    reads: Vec<Expectation>,
) -> (
    axum::Router,
    String,
    FakeLinkAuthority,
    InvitationLink,
    InvitationLink,
) {
    let (app, cookie, ingress, visible, hidden, _) = fixture_with_storage(delegation, reads).await;
    (app, cookie, ingress, visible, hidden)
}
async fn fixture_with_storage(
    delegation: bool,
    reads: Vec<Expectation>,
) -> (
    axum::Router,
    String,
    FakeLinkAuthority,
    InvitationLink,
    InvitationLink,
    Arc<ghinvite_storage_sqlx::SqlxStorage>,
) {
    let storage = Arc::new(
        ghinvite_storage_sqlx::SqlxStorage::in_memory()
            .await
            .unwrap(),
    );
    let mut installation = Installation::repo(Account {
        installation_id: 77,
        account_id: 9001,
        account_login: "acme".into(),
        account_type: AccountType::Organization,
        installed_at: Utc::now(),
        uninstalled_at: None,
        selected_repos: SelectedRepos::All,
    });
    installation.installed_by_user_id = Some(7);
    installation.installed_by_login = Some("installer".into());
    storage.put_app_installation(&installation).await.unwrap();
    storage
        .put_account_settings(&AccountSettings {
            account_id: 9001,
            revision: 1,
            repo_admin_delegation: delegation,
            changed_by: 7,
            changed_at: Utc::now(),
        })
        .await
        .unwrap();
    storage
        .upsert_user(&ghinvite_core::User {
            user_id: 7,
            login: "installer".into(),
            avatar_url: None,
            github_created_at: None,
            last_seen_at: Utc::now(),
        })
        .await
        .unwrap();
    let visible = link(&[10], "Visible workshop");
    let hidden = link(&[10, 11], "Hidden sensitive workshop");
    let ingress = FakeLinkAuthority::start().await;
    for link in [&visible, &hidden] {
        storage.seed_link(link).await.unwrap();
        ingress.seed_link(link);
    }
    let mut expectations = oauth_expectations(OCTOCAT);
    expectations.extend(reads);
    let state = crate::common::sign_in::app_state(
        storage.clone(),
        Arc::new(ConsoleTransport::scripted(expectations)),
        ingress.client(),
        console_config(),
    );
    let app = common::build_app(state, tower_sessions::MemoryStore::default());
    let cookie = sign_in(&app).await;
    (app, cookie, ingress, visible, hidden, storage)
}
async fn visit(app: &axum::Router, cookie: &str, path: &str, body: Option<String>) -> Response {
    let mut request = Request::builder().uri(path).header("cookie", cookie);
    if body.is_some() {
        request = request
            .method("POST")
            .header("content-type", "application/x-www-form-urlencoded");
    }
    app.clone()
        .oneshot(request.body(Body::from(body.unwrap_or_default())).unwrap())
        .await
        .unwrap()
}
async fn text(response: Response) -> String {
    String::from_utf8(
        response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec(),
    )
    .unwrap()
}

#[tokio::test]
async fn delegation_off_conceals_every_console_entry_without_authority_reads() {
    let (app, cookie, ingress, _, hidden) = fixture(false, vec![]).await;
    for path in [
        "/console/accounts/acme".to_owned(),
        "/console/accounts/acme/links".into(),
        "/console/accounts/acme/links/new".into(),
        "/console/accounts/acme/links/new/selection?repo_id=10".into(),
        "/console/accounts/acme/requests".into(),
        "/console/accounts/acme/audit".into(),
        "/console/accounts/acme/settings".into(),
        format!("/console/accounts/acme/links/{}", hidden.id),
    ] {
        let response = visit(&app, &cookie, &path, None).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
        assert!(!text(response).await.contains("Hidden sensitive"));
    }
    let csrf = common::csrf_token(&app, &cookie).await;
    let response = visit(
        &app,
        &cookie,
        "/console/accounts/acme/links",
        Some(format!("csrf_token={csrf}")),
    )
    .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert!(ingress.calls().is_empty());
}

#[tokio::test]
async fn all_targets_control_lists_details_mutations_and_reuse_session_independent_cache() {
    let reads = vec![
        repository_page(&[10, 11]),
        repo_identity(10, "acme/r10", 9001),
        permission(10, "admin"),
        repo_identity(11, "acme/r11", 9001),
        permission(11, "write"),
    ];
    let (app, cookie, ingress, visible, hidden) = fixture(true, reads).await;
    for path in ["/console/accounts/acme/links", "/console/accounts/acme"] {
        let response = visit(&app, &cookie, path, None).await;
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "{}",
            text(response).await
        );
        let html = text(visit(&app, &cookie, path, None).await).await;
        assert!(html.contains("Visible workshop"));
        assert!(!html.contains("Hidden sensitive workshop"));
    }
    assert_eq!(
        visit(
            &app,
            &cookie,
            &format!("/console/accounts/acme/links/{}", visible.id),
            None
        )
        .await
        .status(),
        StatusCode::OK
    );
    assert_eq!(
        visit(
            &app,
            &cookie,
            &format!("/console/accounts/acme/links/{}", hidden.id),
            None
        )
        .await
        .status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        visit(
            &app,
            &cookie,
            &format!("/console/accounts/acme/links/{}/requests", hidden.id),
            None
        )
        .await
        .status(),
        StatusCode::NOT_FOUND
    );
    let csrf = common::csrf_token(&app, &cookie).await;
    for (path, extra) in [
        (
            format!("/console/accounts/acme/links/{}/pause", hidden.id),
            "&operation_id=01K00000000000000000000001",
        ),
        (
            format!("/console/accounts/acme/links/{}/resume", hidden.id),
            "&operation_id=01K00000000000000000000002",
        ),
        (
            format!("/console/accounts/acme/links/{}/revoke", hidden.id),
            "&operation_id=01K00000000000000000000003",
        ),
        (
            format!("/console/accounts/acme/links/{}/edit", hidden.id),
            "&description=changed",
        ),
        (
            format!("/console/accounts/acme/links/{}/rotate", hidden.id),
            "&operation_id=01K00000000000000000000001",
        ),
    ] {
        assert_eq!(
            visit(
                &app,
                &cookie,
                &path,
                Some(format!("csrf_token={csrf}{extra}"))
            )
            .await
            .status(),
            StatusCode::NOT_FOUND,
            "{path}"
        );
    }
    assert!(
        ingress.calls().iter().all(|call| call == "link_status"),
        "{:?}",
        ingress.calls()
    );
}

#[tokio::test]
async fn delegated_search_reads_only_installation_list_and_denied_selection_cannot_create() {
    let page = Expectation::ok_json(
        Method::Get,
        "https://api.github.com/installation/repositories?per_page=100",
        serde_json::json!({"total_count":1,"repositories":[{"id":11,"full_name":"acme/r11","owner":{"id":9001,"login":"acme"}}]}),
    );
    let (app, cookie, ingress, _, _) = fixture(
        true,
        vec![
            page,
            repo_identity(11, "acme/r11", 9001),
            permission(11, "write"),
        ],
    )
    .await;
    // Nothing is listed before a search.
    let response = visit(&app, &cookie, "/console/accounts/acme/links/new", None).await;
    assert_eq!(response.status(), StatusCode::OK);
    let html = text(response).await;
    assert!(html.contains("Search by repository name. Up to 20 matches are listed"));
    assert!(html.contains("name=\"repo_query\""));
    assert!(!html.contains("acme/r11"));
    assert!(!html.contains("name=\"repo_ids\""));
    let response = visit(
        &app,
        &cookie,
        "/console/accounts/acme/links/new?q=r11",
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let html = text(response).await;
    assert!(html.contains("acme/r11"));
    assert!(html.contains("1 repository matches “r11”."));
    assert_eq!(html.matches("Pending request count unavailable").count(), 2);
    assert!(!html.contains("aria-label=\"Pending requests: 0\""));
    let selection = visit(
        &app,
        &cookie,
        "/console/accounts/acme/links/new/selection?repo_id=11",
        None,
    )
    .await;
    assert_eq!(selection.status(), StatusCode::NOT_FOUND);
    assert!(ingress.calls().is_empty());
    let action = html
        .split("action=\"")
        .map(|s| s.split('"').next().unwrap())
        .find(|s| s.contains("links?link_id="))
        .unwrap()
        .replace("&#38;", "&")
        .replace("&amp;", "&");
    let csrf = common::csrf_token(&app, &cookie).await;
    let response = visit(
        &app,
        &cookie,
        &action,
        Some(format!(
            "csrf_token={csrf}&description=Denied&permission=pull&repo_ids=11&repo_grants=11:pull&max_uses=1"
        )),
    )
    .await;
    assert_eq!(
        response.status(),
        StatusCode::NOT_FOUND,
        "{}",
        text(response).await
    );
    assert!(ingress.calls().is_empty());
}

#[tokio::test]
async fn selection_checks_only_the_selected_numeric_repository_without_dispatch() {
    let (app, cookie, ingress, _, _) = fixture(
        true,
        vec![
            repository_page(&[10, 11]),
            repo_identity(10, "acme/r10", 9001),
            permission(10, "admin"),
        ],
    )
    .await;
    let response = visit(
        &app,
        &cookie,
        "/console/accounts/acme/links/new/selection?repo_id=10",
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["cache-control"], "private, no-store");
    assert_eq!(text(response).await, r#"{"allowed":true}"#);
    for path in [
        "/console/accounts/acme/links/new/selection?repo_id=999",
        "/console/accounts/acme/links/new/selection?repo_id=0",
    ] {
        assert_eq!(
            visit(&app, &cookie, path, None).await.status(),
            StatusCode::NOT_FOUND
        );
    }
    assert!(ingress.calls().is_empty());
}

#[tokio::test]
async fn navigation_pending_badge_uses_complete_target_scope_before_counting() {
    let reads = vec![
        repository_page(&[10, 11]),
        repo_identity(10, "acme/r10", 9001),
        permission(10, "admin"),
        repo_identity(11, "acme/r11", 9001),
        permission(11, "write"),
    ];
    let (app, cookie, _, visible, hidden, storage) = fixture_with_storage(true, reads).await;
    for (link, user) in [(&visible, 42), (&hidden, 42), (&hidden, 7)] {
        let snapshot:ghinvite_core::request_lifecycle::RequestSnapshot=serde_json::from_value(
            serde_json::json!({"request_id":ghinvite_core::RequestId::new(),"link_id":link.id,
                "account_id":9001,"requester_id":user,"justification":null,"state":"pending",
                "admitted_at":Utc::now(),"decision_deadline":Utc::now()+chrono::Duration::days(7),"revision":1})
        ).unwrap();
        storage.seed_request(&snapshot.as_request()).await.unwrap();
    }
    for path in [
        "/console/accounts/acme/links",
        "/console/accounts/acme/requests",
    ] {
        let response = visit(&app, &cookie, path, None).await;
        assert_eq!(response.status(), StatusCode::OK);
        let html = text(response).await;
        assert_eq!(
            html.matches("aria-label=\"Pending requests: 1\"").count(),
            2,
            "{html}"
        );
        assert!(!html.contains("aria-label=\"Pending requests: 3\""));
        assert!(!html.contains("Hidden sensitive workshop"));
    }
}
