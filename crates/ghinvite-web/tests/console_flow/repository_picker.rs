//! The create form's repository picker over an installation with more
//! repositories than one GitHub page: server-side search and paging that keep
//! the form's values, and creation that still checks every selection.
use super::*;

/// Repositories in the fixture installation: more than GitHub's 100 per page.
const REPOSITORIES: u64 = 120;

fn name(id: u64) -> String {
    format!("acme/app-{id:03}")
}

/// `GET /installation/repositories`, page `number` (1-based) of the fixture.
fn listing_page(number: u64) -> Expectation {
    let first = (number - 1) * 100 + 1;
    let ids = first..=(first + 99).min(REPOSITORIES);
    let url = if number == 1 {
        "https://api.github.com/installation/repositories?per_page=100".to_string()
    } else {
        format!("https://api.github.com/installation/repositories?per_page=100&page={number}")
    };
    let mut page = Expectation::ok_json(
        Method::Get,
        url,
        serde_json::json!({
            "total_count": REPOSITORIES,
            "repositories": ids.map(|id| serde_json::json!({
                "id": id, "full_name": name(id), "owner": {"id": 9001, "login": "acme"}
            })).collect::<Vec<_>>(),
        }),
    );
    if number * 100 < REPOSITORIES {
        page.response.headers.insert(
            "link".into(),
            format!(
                "<https://api.github.com/installation/repositories?per_page=100&page={}>; rel=\"next\"",
                number + 1
            ),
        );
    }
    page
}

/// Signed in as `acme`'s installing owner, whose installation lists `pages`
/// of the fixture. Creating checks the identity of, and octocat's admin
/// permission on, each repository in `selected` (never anything listed).
async fn picker_app(pages: &[u64], selected: &[u64]) -> (axum::Router, String, FakeLinkAuthority) {
    let authority = FakeLinkAuthority::start().await;
    let mut expectations = oauth_expectations(ACME_ADMIN);
    expectations.extend(pages.iter().map(|page| listing_page(*page)));
    for id in selected {
        expectations.push(repo_identity(*id, &name(*id), 9001));
        expectations.push(repo_admin(&name(*id), 42, "octocat"));
    }
    let (app, cookie) =
        signed_in_app(&[acme_installation()], expectations, authority.client()).await;
    (app, cookie, authority)
}

fn props(html: &str) -> ghinvite_ui::link_form::LinkFormIslandProps {
    let blob = html
        .split_once("<script type=\"application/json\" id=\"link-form-props\">")
        .expect("props blob")
        .1
        .split_once("</script>")
        .unwrap()
        .0;
    serde_json::from_str(blob).unwrap()
}

fn listed(html: &str) -> Vec<String> {
    props(html)
        .repos
        .into_iter()
        .map(|repo| repo.full_name)
        .collect()
}

async fn new_form(app: &axum::Router, cookie: &str, query: &str) -> (StatusCode, String) {
    let response = identity_request(
        app,
        cookie,
        "GET",
        &format!("/console/accounts/acme/links/new{query}"),
    )
    .await;
    (response.status(), response_html(response).await)
}

#[tokio::test]
async fn opening_the_form_lists_one_page_from_one_listing_read() {
    // Only GitHub's first page is scripted: reading the second would fail
    // the page with "Repository access could not be checked".
    let (app, cookie, authority) = picker_app(&[1], &[]).await;
    let (status, html) = new_form(&app, &cookie, "").await;
    assert_eq!(status, StatusCode::OK, "{html}");

    assert_eq!(listed(&html), (1..=50).map(name).collect::<Vec<_>>());
    assert!(html.contains("acme/app-050"));
    assert!(!html.contains("acme/app-051"));
    assert_eq!(html.matches("name=\"repo_ids\"").count(), 50);
    assert!(html.contains("120 repositories in this installation."));
    assert!(html.contains("Page 1 of 3"));
    let picker = props(&html).picker;
    assert_eq!((picker.page, picker.pages, picker.matches), (1, 3, 120));
    // Every listed repository carries the grant its select shows.
    assert_eq!(props(&html).values.repo_grants.len(), 50);

    // The second page is still GitHub's first.
    let (status, html) = new_form(&app, &cookie, "?page=2").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(listed(&html), (51..=100).map(name).collect::<Vec<_>>());
    assert!(authority.calls().is_empty());
}

#[tokio::test]
async fn searching_and_paging_keep_every_value_and_selection_without_creating() {
    let (app, cookie, authority) = picker_app(&[1, 2], &[5, 115]).await;
    let (_, html) = new_form(&app, &cookie, "").await;
    let action = format!(
        "/console/accounts/acme/links?{}",
        creation_form_action(&html)
    );

    // Search with an unfinished form: nothing is validated or created.
    let response = post_form(
        &app,
        &cookie,
        &action,
        "description=&permission=push&max_uses=7&internal_note=Keep+me&repo_ids=5&repo_grants=5:triage&repo_grants=6:pull&repo_query=app-11&repo_page=1&repo_search=true",
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let html = response_html(response).await;
    assert!(!html.contains(SUMMARY), "a search does not validate");
    // The selection from page 1 is pinned ahead of the matches.
    let mut expected = vec![name(5)];
    expected.extend((110..=119).map(name));
    assert_eq!(listed(&html), expected);
    assert!(html.contains("10 repositories match “app-11”."));
    assert!(html.contains("Selected, not on this page"));
    assert!(html.contains("name=\"repo_query\" value=\"app-11\""));
    assert!(html.contains("value=\"5\" checked"));
    assert!(html.contains("<option value=\"5:triage\" selected=true>"));
    assert!(html.contains("value=\"7\""));
    assert!(html.contains("Keep me"));
    assert!(html.contains("value=\"push\" selected"));
    assert!(!html.contains("Repository pages"), "one page of matches");
    assert_eq!(props(&html).action, action);

    // Clear the filter and page to the last page, selecting on the way.
    let response = post_form(
        &app,
        &cookie,
        &action,
        "description=Picker+workshop&permission=push&max_uses=7&repo_ids=5&repo_grants=5:triage&repo_ids=115&repo_grants=115:maintain&repo_query=&repo_page=1&repo_goto=3",
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let html = response_html(response).await;
    let mut expected = vec![name(5)];
    expected.extend((101..=120).map(name));
    assert_eq!(listed(&html), expected);
    assert!(html.contains("Page 3 of 3"));
    assert!(html.contains("<input type=\"hidden\" name=\"repo_page\" value=\"3\""));
    assert!(html.contains("value=\"115\" checked"));
    assert!(html.contains("<option value=\"115:maintain\" selected=true>"));
    assert_eq!(props(&html).values.selected_repo_ids, vec![5, 115]);
    assert!(authority.calls().is_empty());

    // Creating takes both selections, wherever they were listed.
    let response = post_form(
        &app,
        &cookie,
        &action,
        "description=Picker+workshop&permission=push&max_uses=7&repo_ids=5&repo_grants=5:triage&repo_ids=115&repo_grants=115:maintain&repo_query=&repo_page=3",
    )
    .await;
    let status = response.status();
    let html = response_html(response).await;
    assert_eq!(status, StatusCode::OK, "{html}");
    assert!(html.contains("/i#"), "the one-time URL");
    let created = authority.created();
    assert_eq!(created.len(), 1);
    let targets: Vec<_> = created[0]
        .targets
        .repos
        .iter()
        .map(|repo| (repo.repo_id, repo.repo_full_name.clone(), repo.grant))
        .collect();
    assert_eq!(
        targets,
        vec![
            (5, name(5), ghinvite_core::targets::RepoRole::Triage),
            (115, name(115), ghinvite_core::targets::RepoRole::Maintain),
        ]
    );
}

#[tokio::test]
async fn creation_checks_every_selection_and_a_failed_form_keeps_its_page() {
    let (app, cookie, authority) = picker_app(&[1, 2], &[]).await;
    let (_, html) = new_form(&app, &cookie, "").await;
    let action = format!(
        "/console/accounts/acme/links?{}",
        creation_form_action(&html)
    );

    // A repository the installation does not expose is never created, even
    // when the client claims it was listed.
    let response = post_form(
        &app,
        &cookie,
        &action,
        "description=&permission=pull&repo_ids=115&repo_grants=115:pull&repo_ids=999&repo_grants=999:pull&repo_query=app-11&repo_page=1",
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let html = response_html(response).await;
    assert!(authority.created().is_empty());
    assert!(html.contains(SUMMARY));
    assert!(html.contains("Selected repositories are no longer available: 999"));
    assert!(!html.contains("value=\"999\""));
    // The failed form shows the same search, with 115 checked in place.
    assert_eq!(
        listed(&html),
        (110..=119).map(name).collect::<Vec<_>>(),
        "on this page, so not pinned"
    );
    assert!(html.contains("value=\"115\" checked"));
    assert!(html.contains("name=\"repo_query\" value=\"app-11\""));
    assert!(!html.contains("Selected, not on this page"));
}

const SUMMARY: &str = "Fix the highlighted fields before creating this invitation link.";

/// GitHub for the browser fixture, answered as often as asked: sign-in, the
/// fixture's two listing pages, and each repository's identity and octocat's
/// admin permission on it (the selection checks).
struct PickerGithub;

impl ghinvite_github::HttpTransport for PickerGithub {
    async fn send(
        &self,
        request: ghinvite_github::Request,
    ) -> std::result::Result<Response, ghinvite_github::TransportError> {
        let repository = |path: &str| {
            let id: u64 = path.strip_prefix("app-")?.parse().ok()?;
            (1..=REPOSITORIES).contains(&id).then_some(id)
        };
        let answer = oauth_expectations(ACME_ADMIN)
            .into_iter()
            .chain([listing_page(1), listing_page(2)])
            .find(|expected| expected.method == request.method && expected.url == request.url)
            .or_else(|| {
                let path = request
                    .url
                    .strip_prefix("https://api.github.com/repos/acme/")?;
                match path.split_once('/') {
                    None => repository(path).map(|id| repo_identity(id, &name(id), 9001)),
                    Some((repo, "collaborators/octocat/permission")) => {
                        repository(repo).map(|id| repo_admin(&name(id), 42, "octocat"))
                    }
                    Some(_) => None,
                }
            });
        match answer {
            Some(answer) => MockTransport::scripted(vec![answer]).send(request).await,
            None => Err(ghinvite_github::TransportError::SentNoResponse { timed_out: false }),
        }
    }
}

/// Authority reads stay metered per request (50) and cached for five minutes;
/// the fixture only lifts the per-minute binding, which the browser suite's
/// repeated sign-ins and selection checks would otherwise exhaust.
struct Unmetered;

#[async_trait::async_trait]
impl ghinvite_web::rate_limit::EdgeRateLimiter for Unmetered {
    async fn check(
        &self,
        _bucket: ghinvite_web::rate_limit::Bucket,
        _key: &str,
    ) -> Result<bool, ghinvite_web::rate_limit::Unavailable> {
        Ok(true)
    }
}

/// `tests/browser/repository-picker.config.mjs`: the production create route
/// over a 120-repository installation, with creation answered by the HTTP
/// link authority fixture. No island bundle is served.
#[tokio::test]
#[ignore = "long-running Playwright fixture"]
async fn repository_picker_browser_server() {
    use axum::response::IntoResponse;
    let storage = Arc::new(
        ghinvite_storage_sqlx::SqlxStorage::in_memory()
            .await
            .unwrap(),
    );
    storage
        .seed_console_installation(&acme_installation())
        .await
        .unwrap();
    let authority = FakeLinkAuthority::start().await;
    let app = ghinvite_web::build_app(
        crate::common::sign_in::with_signin_bridge(crate::common::sign_in::app_state(
            storage,
            Arc::new(PickerGithub),
            authority.client(),
            console_config(),
        ))
        .with_authority_cache(Arc::new(
            ghinvite_web::authority_cache::MemoryAuthorityCache::default(),
        ))
        .with_edge_limits(Arc::new(Unmetered)),
        tower_sessions::MemoryStore::default(),
    )
    .layer(axum::Extension(
        ghinvite_web::rate_limit::TrustedClientIp::new(std::net::Ipv4Addr::LOCALHOST.into()),
    ));
    let login_app = app.clone();
    let fixture = axum::Router::new()
        .route(
            "/fixture-login",
            axum::routing::get(move || {
                let app = login_app.clone();
                async move {
                    let cookie = sign_in(&app).await;
                    (
                        [(
                            axum::http::header::SET_COOKIE,
                            format!("{cookie}; Path=/; HttpOnly; SameSite=Lax"),
                        )],
                        axum::response::Redirect::to("/console/accounts/acme/links/new"),
                    )
                        .into_response()
                }
            }),
        )
        .route("/health", axum::routing::get(|| async { StatusCode::OK }))
        .fallback(move |request: Request<Body>| {
            let app = app.clone();
            async move { app.oneshot(request).await.unwrap() }
        });
    crate::common::browser_fixture::serve("127.0.0.1:4177", fixture).await;
    drop(authority);
}
