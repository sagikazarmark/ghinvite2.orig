//! HTTP presentation/dispatch contract only. Actual retained policy ordering is
//! verified separately by the workflows org-budget runtime tests.
use super::*;
use ghinvite_core::budget::{OrgCapOutcome, OrgCapPolicy, OrgCapRefusal, OrgCapView, SetOrgCap};

const CAP: &str = "/console/accounts/acme/settings/invitation-cap";
#[derive(Clone, Copy)]
enum CapReply {
    Apply,
    LoseAcknowledgement,
    Refuse(OrgCapRefusal),
}
struct CapState {
    enabled: bool,
    unavailable_read: bool,
    reads: usize,
    replies: VecDeque<CapReply>,
    calls: Vec<SetOrgCap>,
    receipts: BTreeMap<String, (SetOrgCap, OrgCapOutcome)>,
    policy: Option<OrgCapPolicy>,
    applied: Vec<bool>,
}
struct CapRpc {
    state: Arc<Mutex<CapState>>,
    client: Arc<RestateClient>,
}
impl CapRpc {
    async fn start(enabled: bool, replies: Vec<CapReply>) -> Self {
        let state = Arc::new(Mutex::new(CapState {
            enabled,
            unavailable_read: false,
            reads: 0,
            replies: replies.into(),
            calls: vec![],
            receipts: BTreeMap::new(),
            policy: None,
            applied: vec![],
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let client = Arc::new(
            RestateClient::new(format!("http://{}", listener.local_addr().unwrap())).unwrap(),
        );
        let router = axum::Router::new()
            .route("/{service}/{key}/{method}", axum::routing::post(cap_rpc))
            .with_state(state.clone());
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        Self { state, client }
    }
}
async fn cap_rpc(
    State(state): State<Arc<Mutex<CapState>>>,
    Path((service, key, method)): Path<(String, String, String)>,
    bytes: axum::body::Bytes,
) -> Response {
    assert_eq!(
        (service.as_str(), key.as_str()),
        ("AccountInstallation", "repo:9001")
    );
    let mut state = state.lock().unwrap();
    if method == "org_cap_policy" {
        assert!(bytes.is_empty(), "unit read has no request body");
        state.reads += 1;
        return if state.unavailable_read {
            StatusCode::SERVICE_UNAVAILABLE.into_response()
        } else {
            axum::Json(OrgCapView {
                enabled: state.enabled,
                policy: state.policy.clone(),
            })
            .into_response()
        };
    }
    assert_eq!(method, "set_org_cap");
    let command: SetOrgCap = serde_json::from_slice(&bytes).unwrap();
    let operation = String::from(command.operation_id.clone());
    state.calls.push(command.clone());
    if let Some((old, result)) = state.receipts.get(&operation) {
        return if old == &command {
            axum::Json(result).into_response()
        } else {
            StatusCode::CONFLICT.into_response()
        };
    }
    let reply = state
        .replies
        .pop_front()
        .expect("explicit policy mutation response");
    let result = match reply {
        CapReply::Refuse(reason) => Err(reason),
        CapReply::Apply | CapReply::LoseAcknowledgement => {
            let policy = OrgCapPolicy {
                account_id: 9001,
                revision: state.policy.as_ref().map_or(1, |p| p.revision + 1),
                command: command.clone(),
                changed_at: chrono::Utc::now(),
            };
            state.applied.push(command.confirmed_established);
            state.policy = Some(policy.clone());
            Ok(policy)
        }
    };
    state.receipts.insert(operation, (command, result.clone()));
    if matches!(reply, CapReply::LoseAcknowledgement) {
        StatusCode::SERVICE_UNAVAILABLE.into_response()
    } else {
        axum::Json(result).into_response()
    }
}
async fn org_fixture(
    rpc: &CapRpc,
    repo_installed: bool,
    role: &str,
    membership: &str,
) -> (axum::Router, String, Arc<SqlxStorage>) {
    let storage = Arc::new(SqlxStorage::in_memory().await.unwrap());
    if repo_installed {
        storage
            .seed_console_installation(&sign_in::acme_installation())
            .await
            .unwrap();
    }
    let mut org = ghinvite_core::account::Installation::repo(sign_in::acme_installation());
    org.app = ghinvite_core::account::App::Org;
    org.account.installation_id = 2;
    org.permissions = [(
        "members".into(),
        ghinvite_core::account::InstallationPermission::Write,
    )]
    .into();
    storage.put_app_installation(&org).await.unwrap();
    let mut github = sign_in::oauth_expectations(GithubUser::new("org-owner", 99));
    github.push(Expectation::ok_json(Method::Get,"https://api.github.com/orgs/acme/memberships/org-owner",json!({"role":role,"state":membership,"user":{"id":99,"login":"org-owner"},"organization":{"id":9001,"login":"acme"}})));
    if repo_installed {
        github.push(repositories("acme", 9001));
    }
    let state = crate::common::sign_in::app_state(
        storage.clone(),
        Arc::new(ConsoleTransport::scripted(github)),
        rpc.client.clone(),
        sign_in::console_config(),
    );
    let app = common::build_app(state, tower_sessions::MemoryStore::default());
    let cookie = sign_in::sign_in(&app).await;
    (app, cookie, storage)
}
fn cap_form(page: &str) -> &str {
    let action = page.find(&format!("action=\"{CAP}\"")).expect("cap form");
    let start = page[..action].rfind("<form").unwrap();
    let end = action + page[action..].find("</form>").unwrap() + 7;
    &page[start..end]
}
fn body(csrf: &str, operation: &str, confirmed: bool) -> String {
    format!("csrf_token={csrf}&operation_id={operation}&confirmed_established={confirmed}")
}

#[tokio::test]
async fn org_owner_settings_are_enabled_for_org_only_and_dual_installations() {
    for repo in [false, true] {
        let rpc = CapRpc::start(true, vec![CapReply::Apply, CapReply::Apply]).await;
        let (app, cookie, _) = org_fixture(&rpc, repo, "admin", "active").await;
        let response = request(&app, &cookie, "GET", SETTINGS, "").await;
        assert_eq!(response.status(), StatusCode::OK);
        let page = html(response).await;
        assert_eq!(page.matches("<head>").count(), 1);
        assert!(page.contains("50 invitations per 24 hours"));
        let form = cap_form(&page);
        assert!(!input(form, "confirmed_established").contains("checked"));
        let operation = value(form, "operation_id");
        let csrf = value(form, "csrf_token");
        assert!(
            ghinvite_core::admission::AdmissionOperationId::try_from(operation.clone()).is_ok()
        );
        for _ in 0..2 {
            let response =
                request(&app, &cookie, "POST", CAP, &body(&csrf, &operation, true)).await;
            assert_eq!(response.status(), StatusCode::SEE_OTHER);
            assert_eq!(response.headers()["location"], SETTINGS);
        }
        let page = html(request(&app, &cookie, "GET", SETTINGS, "").await).await;
        assert!(input(cap_form(&page), "confirmed_established").contains("checked"));
        let off = value(cap_form(&page), "operation_id");
        assert_ne!(off, operation);
        assert_eq!(
            request(
                &app,
                &cookie,
                "POST",
                CAP,
                &format!("csrf_token={csrf}&operation_id={off}")
            )
            .await
            .status(),
            StatusCode::SEE_OTHER
        );
        let state = rpc.state.lock().unwrap();
        assert_eq!(state.calls.len(), 3);
        assert_eq!(state.calls[0], state.calls[1]);
        assert!(state.calls.iter().all(|c| c.actor_user_id == 99));
        assert_eq!(state.applied, vec![true, false]);
        assert_eq!(state.reads, 2);
    }
}

#[tokio::test]
async fn cap_form_rejects_csrf_actor_spoofs_and_malformed_operations_before_dispatch() {
    let rpc = CapRpc::start(true, vec![]).await;
    let (app, cookie, _) = org_fixture(&rpc, false, "admin", "active").await;
    let csrf = common::csrf_token(&app, &cookie).await;
    let op = RequestId::new().to_string();
    for form in [
        format!("operation_id={op}&confirmed_established=true"),
        body("wrong", &op, true),
        format!("{}&csrf_token={csrf}", body(&csrf, &op, true)),
    ] {
        assert_eq!(
            request(&app, &cookie, "POST", CAP, &form).await.status(),
            StatusCode::FORBIDDEN
        );
    }
    for (index, form) in [
        format!("{}&actor_user_id=42", body(&csrf, &op, true)),
        format!("{}&account_id=9002", body(&csrf, &op, true)),
        body(&csrf, "not-an-operation", true),
        format!("{}&operation_id={op}", body(&csrf, &op, true)),
        format!("{}&%6fperation_id={op}", body(&csrf, &op, true)),
    ]
    .into_iter()
    .enumerate()
    {
        assert_eq!(
            request(&app, &cookie, "POST", CAP, &form).await.status(),
            StatusCode::BAD_REQUEST,
            "rejected case {index}"
        );
    }
    assert!(rpc.state.lock().unwrap().calls.is_empty());
}

#[tokio::test]
async fn lost_result_preserves_exact_form_and_conflicting_operation_is_not_retryable() {
    let rpc = CapRpc::start(true, vec![CapReply::LoseAcknowledgement]).await;
    let (app, cookie, _) = org_fixture(&rpc, false, "admin", "active").await;
    let csrf = common::csrf_token(&app, &cookie).await;
    let op = RequestId::new().to_string();
    let response = request(&app, &cookie, "POST", CAP, &body(&csrf, &op, true)).await;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let page = html(response).await;
    assert_eq!(page.matches("<head>").count(), 1);
    assert!(page.contains("result has not been confirmed"));
    assert!(!page.contains("Organization invitation cap updated"));
    let retry = cap_form(&page);
    assert_eq!(value(retry, "operation_id"), op);
    assert_eq!(value(retry, "csrf_token"), csrf);
    assert!(input(retry, "confirmed_established").contains("checked"));
    assert_eq!(
        request(&app, &cookie, "POST", CAP, &body(&csrf, &op, true))
            .await
            .status(),
        StatusCode::SEE_OTHER
    );
    let response = request(&app, &cookie, "POST", CAP, &body(&csrf, &op, false)).await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let page = html(response).await;
    assert!(!page.contains("result has not been confirmed"));
    assert!(!page.contains("Save invitation cap"));
    let state = rpc.state.lock().unwrap();
    assert_eq!(state.calls[0], state.calls[1]);
    assert_eq!(state.applied, vec![true]);
}

#[tokio::test]
async fn retained_authority_refusals_are_concealed_not_success() {
    for refusal in [OrgCapRefusal::NotOwner, OrgCapRefusal::OrgAppRequired] {
        let rpc = CapRpc::start(true, vec![CapReply::Refuse(refusal)]).await;
        let (app, cookie, _) = org_fixture(&rpc, false, "admin", "active").await;
        let csrf = common::csrf_token(&app, &cookie).await;
        let response = request(
            &app,
            &cookie,
            "POST",
            CAP,
            &body(&csrf, &RequestId::new().to_string(), true),
        )
        .await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let page = html(response).await;
        assert!(!page.contains("Organization invitation cap updated"));
        assert!(!page.contains("Save invitation cap"));
        assert!(rpc.state.lock().unwrap().applied.is_empty());
    }
}

#[tokio::test]
async fn pending_or_nonowner_membership_and_repo_installer_cannot_change_cap() {
    for (role, membership) in [("admin", "pending"), ("member", "active")] {
        let rpc = CapRpc::start(true, vec![]).await;
        let (app, cookie, _) = org_fixture(&rpc, true, role, membership).await;
        let csrf = common::csrf_token(&app, &cookie).await;
        assert_eq!(
            request(&app, &cookie, "GET", SETTINGS, "").await.status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            request(
                &app,
                &cookie,
                "POST",
                CAP,
                &body(&csrf, &RequestId::new().to_string(), true)
            )
            .await
            .status(),
            StatusCode::NOT_FOUND
        );
        let state = rpc.state.lock().unwrap();
        assert!(state.calls.is_empty());
        assert_eq!(state.reads, 0);
    }
    let rpc = CapRpc::start(true, vec![]).await;
    let (app, cookie, _) = fixture(rpc.client.clone(), OCTOCAT, true).await;
    let page = html(request(&app, &cookie, "GET", SETTINGS, "").await).await;
    assert!(!page.contains(CAP));
    let csrf = value(&page, "csrf_token");
    assert_eq!(
        request(
            &app,
            &cookie,
            "POST",
            CAP,
            &body(&csrf, &RequestId::new().to_string(), true)
        )
        .await
        .status(),
        StatusCode::NOT_FOUND
    );
    assert!(rpc.state.lock().unwrap().calls.is_empty());
}

#[tokio::test]
async fn disabled_and_unavailable_policy_views_never_imply_an_enabled_cap() {
    let rpc = CapRpc::start(
        false,
        vec![CapReply::Refuse(OrgCapRefusal::BudgetUnavailable)],
    )
    .await;
    let (app, cookie, _) = org_fixture(&rpc, false, "admin", "active").await;
    let page = html(request(&app, &cookie, "GET", SETTINGS, "").await).await;
    assert!(!page.contains(CAP));
    rpc.state.lock().unwrap().unavailable_read = true;
    let page = html(request(&app, &cookie, "GET", SETTINGS, "").await).await;
    assert!(page.contains("Invitation budget settings are unavailable"));
    assert!(!page.contains(CAP));
    let csrf = common::csrf_token(&app, &cookie).await;
    let op = RequestId::new().to_string();
    let response = request(&app, &cookie, "POST", CAP, &body(&csrf, &op, true)).await;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let page = html(response).await;
    assert!(page.contains("change was not applied"));
    assert!(page.contains("Back to settings"));
    assert!(!page.contains("Save invitation cap"));
    assert!(!page.contains("result has not been confirmed"));
    assert!(rpc.state.lock().unwrap().applied.is_empty());
}

async fn seed_budgets(storage: &SqlxStorage) {
    use ghinvite_core::{
        account::App,
        budget::{BucketId, BucketSnapshot, BudgetSnapshot},
        storage::invite_budget::InviteBudgetStorage,
    };
    for (app, account_id, repo_id, used) in [
        (App::Repo, 9001, 801, 17),
        (App::Org, 9001, 0, 13),
        (App::Repo, 9002, 9991, 29),
        (App::Org, 9002, 0, 31),
    ] {
        storage
            .project_invite_budget(&BudgetSnapshot {
                app,
                account_id,
                revision: 1,
                observed_at: "2026-10-10T12:34:56Z".parse().unwrap(),
                confirmed_established: false,
                buckets: vec![BucketSnapshot {
                    bucket: if app == App::Repo {
                        BucketId::Repo { repo_id }
                    } else {
                        BucketId::Org
                    },
                    used_24h: used,
                    cap: 50,
                    closed_until: None,
                    next_slot_at: None,
                }],
                limit_counts: Default::default(),
                content_used_minute: 3,
                content_used_hour: 7,
                content_closed_until: None,
                next_content_write_at: None,
            })
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn budget_panel_is_account_scoped_and_org_usage_requires_current_org_owner() {
    let rpc = CapRpc::start(true, vec![]).await;
    let (app, cookie, storage) = org_fixture(&rpc, false, "admin", "active").await;
    seed_budgets(&storage).await;
    let response = request(&app, &cookie, "GET", SETTINGS, "").await;
    assert_eq!(response.status(), StatusCode::OK);
    let page = html(response).await;
    assert!(page.contains("Invite Budget"));
    assert!(page.contains("Repository 801"));
    assert!(page.contains("17 / 50"));
    assert!(page.contains("13 / 50"));
    assert!(page.contains("Last recorded:"));
    assert!(page.contains("may have changed since the last update"));
    for foreign in ["Repository 9991", "29 / 50", "31 / 50"] {
        assert!(!page.contains(foreign));
    }

    let rpc = CapRpc::start(true, vec![]).await;
    let (app, cookie, storage) = fixture(rpc.client.clone(), OCTOCAT, true).await;
    seed_budgets(&storage).await;
    let response = request(&app, &cookie, "GET", SETTINGS, "").await;
    assert_eq!(response.status(), StatusCode::OK);
    let page = html(response).await;
    assert!(page.contains("Repository 801"));
    assert!(page.contains("17 / 50"));
    for excluded in ["13 / 50", "Repository 9991", "29 / 50", "31 / 50"] {
        assert!(!page.contains(excluded));
    }
    assert_eq!(
        rpc.state.lock().unwrap().reads,
        0,
        "repo installer does not gain org policy access"
    );
}
