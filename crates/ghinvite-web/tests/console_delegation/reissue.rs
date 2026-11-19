//! HTTP boundary only; actual owner/barrier receipts are covered by workflows.
use super::*;
use ghinvite_core::reissue::{ReissueCommand, ReissueOutcome, Reissued};
struct ReissueRpc {
    client: Arc<RestateClient>,
    state: Arc<Mutex<ReissueState>>,
    id: RequestId,
}
struct ReissueState {
    plan: Value,
    calls: Vec<ReissueCommand>,
    receipts: BTreeMap<String, ReissueCommand>,
    lose: bool,
    refuse: bool,
}
impl ReissueRpc {
    async fn start(lose: bool) -> Self {
        let id = RequestId::new();
        let link = InvitationLinkId::new();
        let at = "2026-01-01T00:00:00Z";
        let request = json!({"request_id":id,"link_id":link,"account_id":9001,"requester_id":99,"justification":null,"state":"approved","admitted_at":at,"decision_deadline":null,"revision":1,"decision":{"decision_id":format!("request.approved/{id}"),"decided_by":null,"effective_at":at,"evaluated_at":at,"decline_reason":null}});
        let command = json!({"invitation_id":ghinvite_core::GithubInvitationId::new(),"link_id":link,"request_id":id,"approval_id":format!("request.approved/{id}"),"account_id":9001,"installation_id":1,"requester_id":99,"repo_id":10,"repo_full_name":"acme/api","permission":"push","approved_at":at,"delivery_generation":1});
        let plan = json!({"dispatch_id":format!("dispatch/{id}"),"input":{"request":request,"installation_id":1,"targets":{"repos":[{"repo_id":10,"repo_full_name":"acme/api","grant":"write"}],"organization":null,"teams":[]},"approval_required":false},"commands":[command]});
        // A typed round trip prevents the ingress fake from hiding wire drift.
        serde_json::from_value::<ghinvite_core::request_lifecycle::ApprovedDispatch>(plan.clone())
            .unwrap();
        let state = Arc::new(Mutex::new(ReissueState {
            plan,
            calls: vec![],
            receipts: BTreeMap::new(),
            lose,
            refuse: false,
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let client = Arc::new(
            RestateClient::new(format!("http://{}", listener.local_addr().unwrap())).unwrap(),
        );
        let app = axum::Router::new()
            .route("/{service}/{key}/{method}", axum::routing::post(rpc))
            .with_state(state.clone());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Self { client, state, id }
    }
    fn path(&self) -> String {
        format!(
            "/console/accounts/acme/requests/{}/reissue/repo/10",
            self.id
        )
    }
}
async fn rpc(
    State(state): State<Arc<Mutex<ReissueState>>>,
    Path((service, key, method)): Path<(String, String, String)>,
    bytes: axum::body::Bytes,
) -> Response {
    assert_eq!(service, "InvitationRequest");
    let mut state = state.lock().unwrap();
    assert_eq!(
        key,
        state.plan["input"]["request"]["request_id"]
            .as_str()
            .unwrap()
    );
    if method == "admin_dispatch" {
        let input: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(input["admin"], json!({"account_id":9001,"user_id":42}));
        return axum::Json(state.plan.clone()).into_response();
    }
    assert_eq!(method, "reissue");
    let input: ReissueCommand = serde_json::from_slice(&bytes).unwrap();
    let op = String::from(input.operation_id.clone());
    state.calls.push(input.clone());
    if let Some(old) = state.receipts.get(&op) {
        if old != &input {
            return StatusCode::CONFLICT.into_response();
        }
    } else {
        state.receipts.insert(op, input);
        if std::mem::take(&mut state.lose) {
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
    }
    let outcome: ReissueOutcome = if state.refuse {
        Err(ghinvite_core::reissue::ReissueRefusal::NotReissuable { final_result: None })
    } else {
        Ok(Reissued {
            delivery_generation: 2,
        })
    };
    axum::Json(outcome).into_response()
}
async fn app(rpc: &ReissueRpc) -> (axum::Router, String) {
    let (app, cookie, _) = fixture_with_reissues(
        rpc.client.clone(),
        OCTOCAT,
        false,
        vec![
            sign_in::repo_identity(10, "acme/api", 9001),
            sign_in::repo_admin("acme/api", 42, "octocat"),
        ],
        true,
    )
    .await;
    (app, cookie)
}
#[tokio::test]
async fn linkless_batch_reissue_recovers_the_exact_operation_and_returns_to_batch() {
    let rpc = ReissueRpc::start(true).await;
    let batch = ghinvite_core::bulk::BulkBatchId::new();
    {
        let mut state = rpc.state.lock().unwrap();
        state.plan["input"]["request"]["link_id"] = Value::Null;
        state.plan["input"]["request"]["batch"] = json!({"batch_id":batch,"row":1});
        state.plan["commands"][0]["link_id"] = Value::Null;
        state.plan["commands"][0]["batch"] = json!({"batch_id":batch,"row":1});
        serde_json::from_value::<ghinvite_core::request_lifecycle::ApprovedDispatch>(
            state.plan.clone(),
        )
        .unwrap();
    }
    let (app, cookie) = app(&rpc).await;
    let path = rpc.path();
    let response = request(&app, &cookie, "GET", &path, "").await;
    assert_eq!(response.status(), StatusCode::OK);
    let page = html(response).await;
    let back = format!("/console/accounts/acme/bulk/{batch}");
    assert!(page.contains(&format!("href=\"{back}\"")));
    assert!(page.contains("Back to batch"));
    assert!(!page.contains("Back to request"));
    let op = value(&page, "operation_id");
    let csrf = value(&page, "csrf_token");
    let form = format!("csrf_token={csrf}&operation_id={op}&confirmation=REISSUE");
    for expected in [StatusCode::SERVICE_UNAVAILABLE, StatusCode::OK] {
        let response = request(&app, &cookie, "POST", &path, &form).await;
        assert_eq!(response.status(), expected);
        let page = html(response).await;
        assert!(page.contains(&back));
        assert!(!page.contains("Back to request"));
    }
    let state = rpc.state.lock().unwrap();
    assert_eq!(state.calls.len(), 2);
    assert_eq!(state.calls[0], state.calls[1]);
    assert_eq!(state.calls[0].request_id, rpc.id);
}

#[tokio::test]
async fn reissue_confirmation_and_unknown_retry_preserve_operation_and_actor() {
    let rpc = ReissueRpc::start(true).await;
    let (app, cookie) = app(&rpc).await;
    let path = rpc.path();
    let response = request(&app, &cookie, "GET", &path, "").await;
    assert_eq!(response.status(), StatusCode::OK);
    let page = html(response).await;
    assert_eq!(page.matches("<head>").count(), 1);
    assert!(page.contains("Administrator") || page.contains("administrator budget"));
    let op = value(&page, "operation_id");
    let csrf = value(&page, "csrf_token");
    let form = format!("csrf_token={csrf}&operation_id={op}&confirmation=REISSUE");
    let response = request(&app, &cookie, "POST", &path, &form).await;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let retry = html(response).await;
    assert_eq!(value(&retry, "operation_id"), op);
    assert_eq!(value(&retry, "confirmation"), "REISSUE");
    let response = request(&app, &cookie, "POST", &path, &form).await;
    assert_eq!(response.status(), StatusCode::OK);
    let page = html(response).await;
    assert!(page.contains("generation 2 was issued"));
    assert!(page.contains("access are not yet confirmed"));
    assert!(!page.contains("name=\"operation_id\""));
    let state = rpc.state.lock().unwrap();
    assert_eq!(state.calls.len(), 2);
    assert_eq!(state.calls[0], state.calls[1]);
    assert_eq!(state.calls[0].admin.user_id, 42);
}
#[tokio::test]
async fn reissue_rejects_spoofed_duplicate_and_unconfirmed_commands_before_dispatch() {
    let rpc = ReissueRpc::start(false).await;
    let (app, cookie) = app(&rpc).await;
    let path = rpc.path();
    let csrf = common::csrf_token(&app, &cookie).await;
    let op = RequestId::new();
    for form in [
        format!("operation_id={op}&confirmation=REISSUE"),
        format!("csrf_token={csrf}&operation_id={op}&confirmation=REISSUE&admin=99"),
        format!("csrf_token={csrf}&operation_id={op}&operation_id={op}&confirmation=REISSUE"),
        format!("csrf_token={csrf}&operation_id=bad&confirmation=REISSUE"),
        format!("csrf_token={csrf}&operation_id={op}&confirmation=NO"),
    ] {
        let response = request(&app, &cookie, "POST", &path, &form).await;
        assert!(matches!(
            response.status(),
            StatusCode::BAD_REQUEST | StatusCode::FORBIDDEN | StatusCode::UNPROCESSABLE_ENTITY
        ));
    }
    assert!(rpc.state.lock().unwrap().calls.is_empty());
    assert_eq!(
        request(
            &app,
            &cookie,
            "GET",
            &path.replace("/repo/10", "/repo/11"),
            ""
        )
        .await
        .status(),
        StatusCode::NOT_FOUND
    );
}
#[tokio::test]
async fn reissue_retained_refusal_does_not_offer_a_same_operation_retry() {
    let rpc = ReissueRpc::start(false).await;
    rpc.state.lock().unwrap().refuse = true;
    let (app, cookie) = app(&rpc).await;
    let csrf = common::csrf_token(&app, &cookie).await;
    let response = request(
        &app,
        &cookie,
        "POST",
        &rpc.path(),
        &format!(
            "csrf_token={csrf}&operation_id={}&confirmation=REISSUE",
            RequestId::new()
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let page = html(response).await;
    assert!(page.contains("refusal is retained"));
    assert!(!page.contains("name=\"operation_id\""));
}

#[tokio::test]
async fn disabled_reissue_is_concealed_before_retained_manifest_or_commands() {
    let rpc = ReissueRpc::start(false).await;
    let (app, cookie, _) = fixture(rpc.client.clone(), OCTOCAT, false).await;
    assert_eq!(
        request(&app, &cookie, "GET", &rpc.path(), "")
            .await
            .status(),
        StatusCode::NOT_FOUND
    );
    assert!(rpc.state.lock().unwrap().calls.is_empty());
}

#[tokio::test]
#[ignore = "long-running Playwright fixture"]
async fn reissue_browser_server() {
    let rpc = ReissueRpc::start(true).await;
    let (app, cookie) = app(&rpc).await;
    let path = rpc.path();
    let state = rpc.state.clone();
    let effects = state.clone();
    let fixture = axum::Router::new()
        .route("/health", axum::routing::get(|| async { StatusCode::OK }))
        .route(
            "/fixture-login",
            axum::routing::get(move || {
                let state = state.clone();
                let cookie = cookie.clone();
                let path = path.clone();
                async move {
                    let mut state = state.lock().unwrap();
                    state.calls.clear();
                    state.receipts.clear();
                    state.lose = true;
                    state.refuse = false;
                    (
                        StatusCode::SEE_OTHER,
                        [
                            (
                                axum::http::header::SET_COOKIE,
                                format!("{cookie}; Path=/; HttpOnly; SameSite=Lax"),
                            ),
                            (axum::http::header::LOCATION, path),
                        ],
                    )
                }
            }),
        )
        .route(
            "/fixture-effects",
            axum::routing::get(move || {
                let state = effects.clone();
                async move {
                    let state = state.lock().unwrap();
                    axum::Json(json!({"calls":state.calls,"receipts":state.receipts.len()}))
                }
            }),
        )
        .fallback(move |request: Request<Body>| {
            let app = app.clone();
            async move { app.oneshot(request).await.unwrap() }
        });
    crate::common::browser_fixture::serve("127.0.0.1:4197", fixture).await;
}
