//! Real link/request owners: replay, arbitration, races and retained business state.
#![cfg(feature = "integration")]
use ghinvite_core::{InvitationLinkId, RequestId, storage::RecordStorage};
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};

struct Runtime {
    client: reqwest::Client,
    ingress: String,
    admin: String,
    link_faults: Arc<ghinvite_workflows::admission::Faults>,
    request_faults: Arc<ghinvite_workflows::request_owner::Faults>,
    harness: ghinvite_e2e::Harness,
    storage: Arc<ghinvite_storage_sqlx::SqlxStorage>,
}
impl Runtime {
    async fn start() -> Self {
        Self::start_mode(false).await
    }
    async fn start_mode(cascade: bool) -> Self {
        let harness = ghinvite_e2e::Harness::start().await;
        let ingress = harness.restate.ingress_url().to_owned();
        let admin = harness.restate.admin_url().to_owned();
        let client = harness.client.clone();
        let github_url = harness.github_url.clone();
        let link_faults = Arc::new(ghinvite_workflows::admission::Faults::default());
        let request_faults = Arc::new(ghinvite_workflows::request_owner::Faults::default());
        let storage = Arc::new(
            ghinvite_storage_sqlx::SqlxStorage::in_memory()
                .await
                .unwrap(),
        );
        for (user_id, login) in [(7, "creator"), (8, "alice"), (9, "bob")] {
            storage
                .upsert_user(&ghinvite_core::User {
                    user_id,
                    login: login.into(),
                    avatar_url: None,
                    github_created_at: None,
                    last_seen_at: chrono::Utc::now(),
                })
                .await
                .unwrap();
        }
        let state = ghinvite_workflows::AppState::new(storage.clone()).with_github_apps(Arc::new(
            ghinvite_workflows::RepositoryGithub::new(
                ghinvite_e2e::github_transport(client.clone()),
                &github_url,
                ghinvite_e2e::repository_apps(&github_url),
            )
            .unwrap(),
        ));
        let cascade_state = state
            .clone()
            .with_pre_activation_cancellation()
            .with_invite_budget(storage.clone());
        let builder = ghinvite_workflows::admission::bind_with_availability_faults(
            restate_sdk::endpoint::Endpoint::builder(),
            link_faults.clone(),
        );
        let builder =
            ghinvite_workflows::request_owner::bind_with_faults(builder, request_faults.clone());
        let builder = ghinvite_workflows::projection::bind(builder, storage.clone());
        let endpoint = ghinvite_workflows::delivery::bind(builder, state.clone())
            .bind(ghinvite_workflows::availability::InstallationProjection {
                state: state.clone(),
            })
            .bind(ghinvite_workflows::availability::AccountInstallation { state })
            .bind(ghinvite_workflows::sweep_schedule::SweepSchedule::default())
            .build();
        let endpoint = if cascade {
            ghinvite_workflows::build_endpoint(cascade_state, storage.clone(), None).unwrap()
        } else {
            endpoint
        };
        harness.restate.deploy(endpoint).await;
        let runtime = Self {
            client,
            ingress,
            admin,
            link_faults,
            request_faults,
            harness,
            storage,
        };
        runtime.ok("AccountInstallation", 100, "onboard", &ghinvite_e2e::setup_command(json!({"app":"repo","installation_id":1,"actor_user_id":7,"account_id":100,"account_login":"acme","account_type":"Organization","selected_repos":"all","installed_at":chrono::Utc::now()}))).await;
        let status = runtime
            .ok("AccountInstallation", 100, "status", &Value::Null)
            .await;
        let setup = json!({"verified":{"app":"repo","account":status["account"],"permissions":{"administration":"write","metadata":"read"},"suspended_at":null}});
        runtime
            .ok(
                "AccountInstallation",
                100,
                "onboard_webhook",
                &ghinvite_e2e::signed_owner_command(&setup, 7),
            )
            .await;
        runtime
    }
    fn call(
        &self,
        service: &str,
        key: impl ToString,
        handler: &str,
        input: &Value,
    ) -> reqwest::RequestBuilder {
        let key = key.to_string();
        let key = if service == "AccountInstallation" {
            format!("repo:{key}")
        } else {
            key
        };
        let path = ghinvite_e2e::Call::object(service, &key, handler).path();
        let request = self.client.post(format!("{}{}", self.ingress, path));
        if input.is_null() {
            request
        } else {
            request.json(input)
        }
    }
    async fn ok(&self, service: &str, key: impl ToString, handler: &str, input: &Value) -> Value {
        let response = self
            .call(service, key, handler, input)
            .send()
            .await
            .unwrap();
        let status = response.status();
        let text = response.text().await.unwrap();
        assert!(status.is_success(), "{handler}: {status} {text}");
        if text.is_empty() {
            Value::Null
        } else {
            serde_json::from_str(&text).unwrap()
        }
    }
    async fn create(&self, max: Option<u32>) -> InvitationLinkId {
        let id = InvitationLinkId::new();
        self.ok("InvitationLink", id, "create", &json!({"secret_sha256":"2a".repeat(32),"link_id":id,"account_id":100,"installation_id":1,
            "admin":{"account_id":100,"user_id":7},"description":"Request ownership","approval_required":true,
            "max_uses":max,"targets":{"organization":null,"teams":[],"repos":[{"repo_id":10,"repo_full_name":"acme/api","grant":"write"}]}})).await;
        id
    }
    async fn uses(&self, id: InvitationLinkId) -> Value {
        self.ok(
            "InvitationLink",
            id,
            "link_status",
            &json!({"link_id":id,"admin":{"account_id":100,"user_id":7}}),
        )
        .await["uses"]
            .clone()
    }
}
fn attempt(id: InvitationLinkId, user: u64) -> Value {
    json!({"scope_evidence":[],"secret_sha256":"2a".repeat(32),"secret_generation":1,"link_id":id,"operation_id":RequestId::new(),"requester_id":user,"justification":"Docs"})
}
fn query(id: InvitationLinkId, receipt: &Value, user: u64) -> Value {
    json!({"link_id":id,"request_id":receipt["result"]["request_id"],"requester_id":user})
}
fn decision(id: InvitationLinkId, receipt: &Value, action: Value) -> Value {
    json!({"link_id":id,"request_id":receipt["result"]["request_id"],"operation_id":RequestId::new(),
        "admin":{"account_id":100,"user_id":7},"action":action})
}

fn revoke_command(id: InvitationLinkId) -> Value {
    json!({
        "link_id": id,
        "admin": {"account_id": 100, "user_id": 7},
        "operation_id": RequestId::new(),
        "cascade": {
            "decline_pending_requests": false,
            "cancel_pending_invitations": false,
            "remove_granted_access": false,
        }
    })
}

#[tokio::test]
async fn manual_pause_resume_retains_results_and_orders_admission_without_changing_requests() {
    let r = Runtime::start().await;
    let id = r.create(Some(3)).await;
    let admin = json!({"link_id":id,"admin":{"account_id":100,"user_id":7}});
    let lifecycle = |id: InvitationLinkId| json!({"link_id":id,"admin":{"account_id":100,"user_id":7},"operation_id":RequestId::new()});
    let original_attempt = attempt(id, 8);
    let accepted = r.ok("InvitationLink", id, "admit", &original_attempt).await;
    assert_eq!(accepted["result"]["kind"], "accepted");
    let pause = lifecycle(id);
    r.link_faults.arm("lifecycle-after-state");
    let pending_pause = r.call("InvitationLink", id, "pause", &pause);
    let task = tokio::spawn(async move { pending_pause.send().await.unwrap() });
    tokio::time::timeout(Duration::from_secs(5), async {
        while r.link_faults.attempts() == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    // This admission queues behind the exclusive pause even while its response
    // and projection handoff are interrupted.
    let fresh = attempt(id, 9);
    let queued = r.call("InvitationLink", id, "admit", &fresh);
    let queued = tokio::spawn(async move { queued.send().await.unwrap() });
    r.link_faults.release();
    let paused: Value = task
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(paused["Ok"]["pause"]["reason"]["kind"], "manual");
    let page_input = json!({"secret_sha256":"2a".repeat(32),"secret_generation":1,"link_id":id,"requester_id":10});
    let page = r
        .ok("InvitationLink", id, "requester_page", &page_input)
        .await;
    assert_eq!(page["paused"], true);
    assert_eq!(page["can_start_fresh"], false);
    assert!(page.get("pause").is_none());
    let mut invalid_page = page_input.clone();
    invalid_page["secret_sha256"] = json!("99".repeat(32));
    assert_eq!(
        r.call("InvitationLink", id, "requester_page", &invalid_page)
            .send()
            .await
            .unwrap()
            .status(),
        404
    );

    let refused: Value = queued
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(refused["result"]["reason"], "paused");
    assert_eq!(r.uses(id).await, 1);
    assert_eq!(
        r.ok("InvitationLink", id, "admit", &original_attempt).await,
        accepted
    );
    let mut wrong_secret = attempt(id, 10);
    wrong_secret["secret_sha256"] = json!("99".repeat(32));
    assert_eq!(
        r.ok("InvitationLink", id, "admit", &wrong_secret).await["result"]["reason"],
        "not_available"
    );
    assert_eq!(
        r.ok("InvitationLink", id, "pause", &lifecycle(id)).await["Err"],
        "already_paused"
    );
    // Manual pause leaves pending decisions and their immutable deadline intact.
    let request_id = accepted["result"]["request_id"].as_str().unwrap();
    let before = r
        .ok(
            "InvitationRequest",
            request_id,
            "request_status",
            &query(id, &accepted, 8),
        )
        .await;
    assert_eq!(
        before["decision_deadline"],
        accepted["result"]["decision_deadline"]
    );
    let approved = r
        .ok(
            "InvitationRequest",
            request_id,
            "decide",
            &decision(id, &accepted, json!({"kind":"approve"})),
        )
        .await;
    assert!(approved.to_string().contains("approved"), "{approved}");
    let resume = lifecycle(id);
    let resumed = r.ok("InvitationLink", id, "resume", &resume).await;
    assert!(resumed["Ok"].get("pause").is_none());
    assert_eq!(r.ok("InvitationLink", id, "pause", &pause).await, paused);
    assert_eq!(r.ok("InvitationLink", id, "resume", &resume).await, resumed);
    assert_eq!(r.ok("InvitationLink", id, "admit", &fresh).await, refused);
    assert_eq!(
        r.ok("InvitationLink", id, "admit", &attempt(id, 9)).await["result"]["kind"],
        "accepted"
    );
    assert_eq!(
        r.call("InvitationLink", id, "resume", &pause)
            .send()
            .await
            .unwrap()
            .status(),
        409
    );
    let mut conflicting = pause.clone();
    conflicting["admin"]["user_id"] = json!(9);
    assert_eq!(
        r.call("InvitationLink", id, "pause", &conflicting)
            .send()
            .await
            .unwrap()
            .status(),
        409
    );
    let revocation = revoke_command(id);
    assert_eq!(
        r.call("InvitationLink", id, "revoke", &admin)
            .send()
            .await
            .unwrap()
            .status(),
        400,
        "legacy revoke input cannot silently supply an operation or cascade"
    );
    let revoked = r.ok("InvitationLink", id, "revoke", &revocation).await;
    assert!(revoked["Ok"].is_object(), "{revoked}");
    assert_eq!(
        r.ok("InvitationLink", id, "revoke", &revocation).await,
        revoked
    );
    let mut changed_revoke = revocation.clone();
    changed_revoke["cascade"]["decline_pending_requests"] = json!(true);
    assert_eq!(
        r.call("InvitationLink", id, "revoke", &changed_revoke)
            .send()
            .await
            .unwrap()
            .status(),
        409
    );
    assert_eq!(
        r.ok("InvitationLink", id, "revoke", &revoke_command(id))
            .await,
        json!({"Err":"revoked"})
    );
    assert_eq!(
        r.ok("InvitationLink", id, "admit", &original_attempt).await,
        accepted
    );
    assert_eq!(r.ok("InvitationLink", id, "pause", &pause).await, paused);
    assert_eq!(
        r.ok("InvitationLink", id, "resume", &lifecycle(id)).await["Err"],
        "revoked"
    );
    assert!(r.ok("InvitationLink", id, "link_status", &admin).await["revoked_at"].is_string());

    let exhausted = r.create(Some(1)).await;
    r.ok(
        "InvitationLink",
        exhausted,
        "admit",
        &attempt(exhausted, 15),
    )
    .await;
    r.ok("InvitationLink", exhausted, "pause", &lifecycle(exhausted))
        .await;
    assert_eq!(
        r.ok("InvitationLink", exhausted, "resume", &lifecycle(exhausted))
            .await["Err"],
        "expired_or_exhausted"
    );
    assert_eq!(r.uses(exhausted).await, 1);
    let expiry_id = InvitationLinkId::new();
    let now = chrono::Utc::now();
    r.ok("InvitationLink",expiry_id,"create",&json!({"secret_sha256":"2a".repeat(32),"link_id":expiry_id,"account_id":100,"installation_id":1,
        "admin":{"account_id":100,"user_id":7},"description":"Expiry pause","approval_required":true,
        "expires_at":now+chrono::Duration::hours(1),"targets":{"organization":null,"teams":[],"repos":[{"repo_id":10,"repo_full_name":"acme/api","grant":"write"}]}})).await;
    r.ok("InvitationLink", expiry_id, "pause", &lifecycle(expiry_id))
        .await;
    r.link_faults
        .set_clock(Some(now + chrono::Duration::hours(2)));
    assert_eq!(
        r.ok("InvitationLink", expiry_id, "resume", &lifecycle(expiry_id))
            .await["Err"],
        "expired_or_exhausted"
    );
}

#[tokio::test]
async fn request_authority_contract() {
    let runtime = Runtime::start().await;
    let r = &runtime;
    // Retry after a journaled clock read but before computing the decision.
    // Advancing the injected clock beyond the pending lifetime must not move
    // either admission time or the deadline derived from that reading.
    let id = r.create(None).await;
    let first_time =
        chrono::DateTime::from_timestamp_millis(chrono::Utc::now().timestamp_millis()).unwrap();
    r.link_faults.set_clock(Some(first_time));
    r.link_faults.arm("after-admission-time");
    let a = attempt(id, 8);
    let sending = r.call("InvitationLink", id, "admit", &a);
    let pending = tokio::spawn(async move { sending.send().await.unwrap() });
    tokio::time::timeout(Duration::from_secs(5), async {
        while r.link_faults.attempts() == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    r.link_faults
        .set_clock(Some(first_time + chrono::Duration::days(8)));
    r.link_faults.release();
    let receipt: Value = pending
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(receipt["result"]["kind"], "accepted");
    assert_eq!(receipt["decided_at"], json!(first_time));
    assert_eq!(
        receipt["result"]["decision_deadline"],
        json!(first_time + chrono::Duration::days(7))
    );
    r.link_faults.set_clock(None);

    // Scope evidence is caller-supplied. A recent negative refuses, while
    // stale/missing observations do not turn the link owner into a GitHub reader.
    let id = r.create(None).await;
    let mut a = attempt(id, 8);
    a["scope_evidence"] = json!([{"app":"repo","observed_at":chrono::Utc::now() - chrono::Duration::minutes(2),"available":false,"unavailable_target_ids":[10]}]);
    let rejected = r.ok("InvitationLink", id, "admit", &a).await;
    assert_eq!(rejected["result"]["reason"], "scope_unavailable");
    assert_eq!(r.uses(id).await, 0);
    assert_eq!(r.ok("InvitationLink", id, "admit", &a).await, rejected);
    a["operation_id"] = json!(RequestId::new());
    a["scope_evidence"][0]["observed_at"] =
        json!(chrono::Utc::now() - chrono::Duration::minutes(6));
    assert_eq!(
        r.ok("InvitationLink", id, "admit", &a).await["result"]["kind"],
        "accepted"
    );
    assert_eq!(r.uses(id).await, 1);
    for stage in [
        "creation-before-decision",
        "creation-after-decision",
        "creation-after-link",
        "creation-after-receipt",
        "creation-after-projection-send",
    ] {
        let id = InvitationLinkId::new();
        let now = chrono::Utc::now();
        let command = json!({"secret_sha256":"2a".repeat(32),"link_id":id,"account_id":100,"installation_id":1,
            "admin":{"account_id":100,"user_id":7},"description":"Original creation","approval_required":true,
            "expires_at":now + chrono::Duration::hours(1),"targets":{"organization":null,"teams":[],"repos":[{"repo_id":10,"repo_full_name":"acme/api","grant":"write"}]}});
        r.link_faults.arm(stage);
        let sending = r.call("InvitationLink", id, "create", &command);
        let first = tokio::spawn(async move { sending.send().await.unwrap() });
        tokio::time::timeout(Duration::from_secs(5), async {
            while r.link_faults.attempts() == 0 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert!(!first.is_finished(), "{stage}");
        let sending = r.call("InvitationLink", id, "create", &command);
        let concurrent = tokio::spawn(async move { sending.send().await.unwrap() });
        r.link_faults.release();
        let first: Value = first
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(
            concurrent
                .await
                .unwrap()
                .error_for_status()
                .unwrap()
                .json::<Value>()
                .await
                .unwrap(),
            first
        );
        let mut changed = command.clone();
        changed["description"] = json!("Conflicting creation");
        assert_eq!(
            r.call("InvitationLink", id, "create", &changed)
                .send()
                .await
                .unwrap()
                .status(),
            409
        );
        let admin = json!({"link_id":id,"admin":{"account_id":100,"user_id":7}});
        let mut metadata = admin.clone();
        metadata["description"] = json!("Current metadata");
        r.ok("InvitationLink", id, "update_metadata", &metadata)
            .await;
        assert_eq!(r.ok("InvitationLink", id, "create", &command).await, first);
        let revocation = revoke_command(id);
        let revoked = r.ok("InvitationLink", id, "revoke", &revocation).await;
        assert!(revoked["Ok"].is_object(), "{revoked}");
        assert_eq!(
            r.ok("InvitationLink", id, "revoke", &revocation).await,
            revoked
        );
        assert_eq!(r.ok("InvitationLink", id, "create", &command).await, first);
        r.link_faults
            .set_clock(Some(now + chrono::Duration::hours(2)));
        assert_eq!(r.ok("InvitationLink", id, "create", &command).await, first);
        r.link_faults.set_clock(None);
    }
    // A failed short initialization retains handoff progress, never another use.
    let unavailable_link = r.create(None).await;
    let unavailable_attempt = attempt(unavailable_link, 8);
    r.request_faults
        .unavailable
        .store(true, std::sync::atomic::Ordering::SeqCst);
    assert_eq!(
        r.call(
            "InvitationLink",
            unavailable_link,
            "admit",
            &unavailable_attempt
        )
        .send()
        .await
        .unwrap()
        .status(),
        503
    );
    assert_eq!(r.uses(unavailable_link).await, 1);
    r.request_faults
        .unavailable
        .store(false, std::sync::atomic::Ordering::SeqCst);
    let recovered = r
        .ok(
            "InvitationLink",
            unavailable_link,
            "admit",
            &unavailable_attempt,
        )
        .await;
    assert_eq!(recovered["result"]["kind"], "accepted");
    assert_eq!(r.uses(unavailable_link).await, 1);
    let fresh = attempt(unavailable_link, 8);
    r.request_faults
        .unavailable
        .store(true, std::sync::atomic::Ordering::SeqCst);
    assert_eq!(
        r.call("InvitationLink", unavailable_link, "admit", &fresh)
            .send()
            .await
            .unwrap()
            .status(),
        503
    );
    r.request_faults
        .unavailable
        .store(false, std::sync::atomic::Ordering::SeqCst);
    let rid = recovered["result"]["request_id"].as_str().unwrap();
    r.ok(
        "InvitationRequest",
        rid,
        "decide",
        &decision(unavailable_link, &recovered, json!({"kind":"decline"})),
    )
    .await;
    assert_eq!(
        r.ok("InvitationLink", unavailable_link, "admit", &fresh)
            .await["result"]["kind"],
        "accepted"
    );
    assert_eq!(r.uses(unavailable_link).await, 2);
    // Last-use serialization and immutable replay, including changed-input conflicts.
    let id = r.create(Some(1)).await;
    let a = attempt(id, 8);
    let b = attempt(id, 9);
    let (a_receipt, b_receipt) = tokio::join!(
        r.ok("InvitationLink", id, "admit", &a),
        r.ok("InvitationLink", id, "admit", &b)
    );
    assert_eq!(r.uses(id).await, 1);
    assert_ne!(a_receipt["result"]["kind"], b_receipt["result"]["kind"]);
    assert_eq!(r.ok("InvitationLink", id, "admit", &a).await, a_receipt);
    let mut changed = a.clone();
    changed["justification"] = json!("different");
    assert_eq!(
        r.call("InvitationLink", id, "admit", &changed)
            .send()
            .await
            .unwrap()
            .status(),
        409
    );

    // Terminal-then-fresh admission consults the request; stale old timers do not release the new pointer.
    let id = r.create(None).await;
    let a = attempt(id, 8);
    let receipt = r.ok("InvitationLink", id, "admit", &a).await;
    let rid = receipt["result"]["request_id"].as_str().unwrap();
    let decline = decision(
        id,
        &receipt,
        json!({"kind":"decline","reason":"  private reason  "}),
    );
    let decided = r.ok("InvitationRequest", rid, "decide", &decline).await;
    assert_eq!(decided["outcome"], "applied");
    assert_eq!(
        decided["request"]["decision"]["decline_reason"],
        "private reason"
    );
    assert_eq!(
        r.ok("InvitationRequest", rid, "decide", &decline).await,
        decided
    );
    let safe = r
        .ok(
            "InvitationRequest",
            rid,
            "request_status",
            &query(id, &receipt, 8),
        )
        .await;
    assert_eq!(safe["decision"]["decline_reason"], Value::Null);
    let fresh = r.ok("InvitationLink", id, "admit", &attempt(id, 8)).await;
    assert_eq!(fresh["result"]["kind"], "accepted");
    r.ok("InvitationRequest", rid, "tick_expire", &Value::Null)
        .await;
    let blocked = r.ok("InvitationLink", id, "admit", &attempt(id, 8)).await;
    assert_eq!(blocked["result"]["reason"], "already_pending");
    assert_eq!(r.uses(id).await, 2);
    assert_eq!(r.ok("InvitationLink", id, "admit", &a).await, receipt);
    let mut different = decline.clone();
    different["action"] = json!({"kind":"approve"});
    assert_eq!(
        r.call("InvitationRequest", rid, "decide", &different)
            .send()
            .await
            .unwrap()
            .status(),
        409
    );
    different["operation_id"] = json!(RequestId::new());
    assert_eq!(
        r.ok("InvitationRequest", rid, "decide", &different).await["outcome"],
        "incompatible"
    );

    // Concurrent approve/decline has one recorded winner and truthful losing response.
    let id = r.create(None).await;
    let receipt = r.ok("InvitationLink", id, "admit", &attempt(id, 8)).await;
    let rid = receipt["result"]["request_id"].as_str().unwrap();
    let approve = decision(id, &receipt, json!({"kind":"approve"}));
    let decline = decision(id, &receipt, json!({"kind":"decline"}));
    let (approved, declined) = tokio::join!(
        r.ok("InvitationRequest", rid, "decide", &approve),
        r.ok("InvitationRequest", rid, "decide", &decline)
    );
    assert_eq!(approved["request"]["state"], declined["request"]["state"]);
    assert_ne!(approved["outcome"], declined["outcome"]);

    // Initialization interruption cannot expose acceptance early; recovery retains the same use.
    let id = r.create(None).await;
    let a = attempt(id, 8);
    r.link_faults.arm("after-initialize");
    let sending = r.call("InvitationLink", id, "admit", &a);
    let pending = tokio::spawn(async move { sending.send().await.unwrap() });
    tokio::time::timeout(Duration::from_secs(5), async {
        while r.link_faults.attempts() == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(!pending.is_finished());
    r.link_faults.release();
    let receipt: Value = pending.await.unwrap().json().await.unwrap();
    assert_eq!(r.ok("InvitationLink", id, "admit", &a).await, receipt);
    assert_eq!(r.uses(id).await, 1);

    // A blocking verdict published before the deadline binds the rejection even if the link resumes late.
    let id = r.create(None).await;
    let deadline = chrono::Utc::now() + chrono::Duration::seconds(3);
    r.link_faults
        .set_clock(Some(deadline - chrono::Duration::days(7)));
    let original = r.ok("InvitationLink", id, "admit", &attempt(id, 8)).await;
    r.link_faults.set_clock(None);
    r.link_faults.arm("after-verdict");
    let a = attempt(id, 8);
    let sending = r.call("InvitationLink", id, "admit", &a);
    let pending = tokio::spawn(async move { sending.send().await.unwrap() });
    tokio::time::timeout(Duration::from_secs(5), async {
        while r.link_faults.attempts() == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_secs(4)).await;
    r.link_faults.release();
    let rejected: Value = pending.await.unwrap().json().await.unwrap();
    assert_eq!(rejected["result"]["reason"], "already_pending");
    assert_eq!(r.ok("InvitationLink", id, "admit", &a).await, rejected);
    assert_eq!(
        r.ok("InvitationLink", id, "admit", &attempt(id, 8)).await["result"]["kind"],
        "accepted"
    );
    assert_eq!(r.uses(id).await, 2);
    let rid = original["result"]["request_id"].as_str().unwrap();
    // Actual invocation cleanup, followed by original decision/admission recovery.
    tokio::time::timeout(Duration::from_secs(15), async { loop {
        let result: Value = r.client.post(format!("{}/query", r.admin)).header("accept", "application/json")
            .json(&json!({"query":format!("SELECT id FROM sys_invocation WHERE target_service_name = 'InvitationRequest' AND target_service_key = '{rid}' AND status = 'completed'")}))
            .send().await.unwrap().json().await.unwrap();
        if result["rows"].as_array().unwrap().is_empty() { break; }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }}).await.expect("actual request invocation cleanup");
    assert_eq!(r.ok("InvitationLink", id, "admit", &a).await, rejected);
    assert_eq!(
        r.ok(
            "InvitationRequest",
            rid,
            "request_status",
            &query(id, &original, 8)
        )
        .await["state"],
        "expired"
    );
    // Strict deadline arbitration, including a fully journaled timely decision
    // whose after-images are applied after the deadline.
    for (offset, expected) in [(-1, "approved"), (0, "expired"), (1, "expired")] {
        let id = r.create(None).await;
        let receipt = r.ok("InvitationLink", id, "admit", &attempt(id, 8)).await;
        let rid = receipt["result"]["request_id"].as_str().unwrap();
        let deadline: chrono::DateTime<chrono::Utc> =
            serde_json::from_value(receipt["result"]["decision_deadline"].clone()).unwrap();
        *r.request_faults.clock.lock().unwrap() =
            Some(deadline + chrono::Duration::milliseconds(offset));
        let approve = decision(id, &receipt, json!({"kind":"approve"}));
        let decided = r.ok("InvitationRequest", rid, "decide", &approve).await;
        assert_eq!(decided["request"]["state"], expected);
        *r.request_faults.clock.lock().unwrap() = None;
    }
    for (stage, expected) in [
        ("before-decision", "expired"),
        ("after-decision", "approved"),
    ] {
        let id = r.create(None).await;
        let receipt = r.ok("InvitationLink", id, "admit", &attempt(id, 8)).await;
        let rid = receipt["result"]["request_id"].as_str().unwrap();
        let deadline: chrono::DateTime<chrono::Utc> =
            serde_json::from_value(receipt["result"]["decision_deadline"].clone()).unwrap();
        *r.request_faults.clock.lock().unwrap() =
            Some(deadline - chrono::Duration::milliseconds(1));
        *r.request_faults.stage.lock().unwrap() = Some(stage.into());
        r.request_faults
            .reached
            .store(0, std::sync::atomic::Ordering::SeqCst);
        let approve = decision(id, &receipt, json!({"kind":"approve"}));
        let sending = r.call("InvitationRequest", rid, "decide", &approve);
        let pending = tokio::spawn(async move { sending.send().await.unwrap() });
        tokio::time::timeout(Duration::from_secs(5), async {
            while r
                .request_faults
                .reached
                .load(std::sync::atomic::Ordering::SeqCst)
                == 0
            {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        *r.request_faults.clock.lock().unwrap() = Some(deadline);
        *r.request_faults.stage.lock().unwrap() = None;
        let decided: Value = pending.await.unwrap().json().await.unwrap();
        assert_eq!(decided["request"]["state"], expected);
        assert_eq!(
            r.ok("InvitationRequest", rid, "decide", &approve).await,
            decided
        );
        *r.request_faults.clock.lock().unwrap() = None;
    }
    runtime.harness.finish().await;
}

#[tokio::test]
async fn abuse_window_refuses_without_a_use_holds_approval_and_resumes_or_aborts() {
    let r = Runtime::start().await;
    async fn wait_outcome(r: &Runtime, receipt: &Value, expected: &str) -> Value {
        let key = format!("{}:10:1", receipt["result"]["request_id"].as_str().unwrap());
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                let status = r
                    .ok("RepositoryDelivery", &key, "status", &Value::Null)
                    .await;
                if status["create"]["outcome"]["kind"] == expected {
                    return status;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("delivery did not reach {expected}"))
    }
    async fn writes(r: &Runtime) -> usize {
        let trace: Value = r
            .client
            .get(format!("{}/calls", r.harness.github_url))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        trace["requests"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|v| matches!(v["method"].as_str(), Some("PUT" | "DELETE")))
            .count()
    }
    for revoke in [false, true] {
        let id = InvitationLinkId::new();
        let create = json!({"secret_sha256":"2a".repeat(32),"link_id":id,"account_id":100,"installation_id":1,
            "admin":{"account_id":100,"user_id":7},"description":"Abuse hold","approval_required":true,
            "max_uses":null,"abuse_threshold_per_hour":1,"invitation_lifetime_seconds":604800,
            "targets":{"organization":null,"teams":[],"repos":[{"repo_id":10,"repo_full_name":"acme/api","grant":"write"}]}});
        r.ok("InvitationLink", id, "create", &create).await;
        let original = attempt(id, 8);
        let accepted = r.ok("InvitationLink", id, "admit", &original).await;
        let rid = accepted["result"]["request_id"].as_str().unwrap();
        let deadline = accepted["result"]["decision_deadline"].clone();
        let before = writes(&r).await;
        let mut refused_attempt = attempt(id, 9);
        // A user-chosen operation equal to an existing request identity cannot
        // collide with the private hold operation and suppress its effect.
        refused_attempt["operation_id"] = accepted["result"]["request_id"].clone();
        let admitted_at: chrono::DateTime<chrono::Utc> = serde_json::from_value(
            r.ok(
                "InvitationRequest",
                rid,
                "request_status",
                &query(id, &accepted, 8),
            )
            .await["admitted_at"]
                .clone(),
        )
        .unwrap();
        r.link_faults
            .set_clock(Some(admitted_at - chrono::Duration::seconds(1)));
        assert_eq!(
            r.call("InvitationLink", id, "admit", &refused_attempt)
                .send()
                .await
                .unwrap()
                .status(),
            503
        );
        assert_eq!(r.uses(id).await, 1);
        assert!(
            r.ok(
                "InvitationLink",
                id,
                "link_status",
                &json!({"link_id":id,"admin":{"account_id":100,"user_id":7}})
            )
            .await["pause"]
                .is_null()
        );
        r.link_faults.set_clock(None);
        let refused = r.ok("InvitationLink", id, "admit", &refused_attempt).await;
        assert_eq!(refused["result"]["reason"], "paused");
        assert_eq!(r.uses(id).await, 1);
        assert_eq!(
            r.ok("InvitationLink", id, "admit", &original).await,
            accepted
        );
        let admin = json!({"link_id":id,"admin":{"account_id":100,"user_id":7}});
        let paused = r.ok("InvitationLink", id, "link_status", &admin).await;
        assert_eq!(paused["pause"]["reason"]["kind"], "abuse_suspected");
        let pending = r
            .ok(
                "InvitationRequest",
                rid,
                "request_status",
                &query(id, &accepted, 8),
            )
            .await;
        assert_eq!(pending["state"], "pending");
        assert_eq!(pending["decision_deadline"], deadline);
        assert_eq!(
            paused["pause"]["reason"]["window_start"],
            pending["admitted_at"]
        );
        let approved = r
            .ok(
                "InvitationRequest",
                rid,
                "decide",
                &decision(id, &accepted, json!({"kind":"approve"})),
            )
            .await;
        assert_eq!(approved["request"]["state"], "approved");
        wait_outcome(&r, &accepted, "held").await;
        assert_eq!(
            writes(&r).await,
            before,
            "approval while held makes no GitHub write"
        );
        if revoke {
            let revocation = revoke_command(id);
            let revoked = r.ok("InvitationLink", id, "revoke", &revocation).await;
            assert!(revoked["Ok"].is_object(), "{revoked}");
            wait_outcome(&r, &accepted, "aborted").await;
            assert_eq!(writes(&r).await, before);
            assert_eq!(
                r.ok("InvitationLink", id, "revoke", &revocation).await,
                revoked
            );
        } else {
            let resume = json!({"link_id":id,"admin":{"account_id":100,"user_id":7},"operation_id":RequestId::new()});
            let resumed = r.ok("InvitationLink", id, "resume", &resume).await;
            assert!(resumed["Ok"]["pause"].is_null());
            wait_outcome(&r, &accepted, "created").await;
            assert_eq!(writes(&r).await, before + 1);
            assert_eq!(r.ok("InvitationLink", id, "resume", &resume).await, resumed);
            assert_eq!(
                r.ok("InvitationLink", id, "admit", &refused_attempt).await,
                refused
            );
            // The same hour still contains the original use; resume does not
            // reset the immutable guardrail's sliding window.
            assert_eq!(
                r.ok("InvitationLink", id, "admit", &attempt(id, 9)).await["result"]["reason"],
                "paused"
            );
            r.link_faults
                .set_clock(Some(admitted_at + chrono::Duration::hours(1)));
            let fresh_resume = json!({"link_id":id,"admin":{"account_id":100,"user_id":7},"operation_id":RequestId::new()});
            r.ok("InvitationLink", id, "resume", &fresh_resume).await;
            assert_eq!(
                r.ok("InvitationLink", id, "admit", &attempt(id, 9)).await["result"]["kind"],
                "accepted"
            );
            assert_eq!(r.uses(id).await, 2);
            r.link_faults.set_clock(None);
        }
    }
    r.harness.finish().await;
}

#[path = "authoritative_admission/control_replay.rs"]
mod control_replay;

#[path = "authoritative_admission/revocation.rs"]
mod revocation;

#[path = "authoritative_admission/load_ordering.rs"]
mod load_ordering;

#[path = "authoritative_admission/requester_message.rs"]
mod requester_message;
