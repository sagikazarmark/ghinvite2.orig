//! Real link/request owners with retained delivery snapshots and withheld SQL.
//! HTTP owner execution is separately covered by organization/repository suites.
#![cfg(feature = "integration")]
use chrono::{DateTime, Duration, Utc};
use ghinvite_core::{
    InvitationLinkId, InvitationState, RequestId,
    delivery::{CreateCommand, CreateOutcome, CreateReceipt, DeliverySnapshot, Settlement},
};
use ghinvite_e2e::Harness;
use restate_sdk::{
    context::{
        ContextClient, ContextReadState, ContextWriteState, ObjectContext, SharedObjectContext,
    },
    errors::TerminalError,
    serde::Json,
};
use serde_json::{Value, json};
use std::sync::Arc;
struct RepositoryDelivery;
#[restate_sdk::object]
impl RepositoryDelivery {
    #[handler]
    async fn prepare_reissue(
        &self,
        ctx: ObjectContext<'_>,
        Json(input): Json<ghinvite_core::reissue::RepositoryReissueBarrierQuery>,
    ) -> Result<Json<ghinvite_core::reissue::InvitationBarrier>, TerminalError> {
        let old = ctx
            .get::<Json<DeliverySnapshot>>("snapshot")
            .await?
            .unwrap()
            .0;
        assert_eq!(old.create.command, input.command);
        assert!(old.settlement.is_some());
        Ok(Json(
            ctx.get::<Json<ghinvite_core::reissue::InvitationBarrier>>("barrier")
                .await?
                .map(|j| j.0)
                .unwrap_or(ghinvite_core::reissue::InvitationBarrier::Clear),
        ))
    }
    #[handler]
    async fn set_barrier(
        &self,
        ctx: ObjectContext<'_>,
        input: Json<ghinvite_core::reissue::InvitationBarrier>,
    ) -> Result<(), TerminalError> {
        ctx.set("barrier", input);
        Ok(())
    }

    #[handler]
    async fn control(
        &self,
        _ctx: ObjectContext<'_>,
        _input: Json<Value>,
    ) -> Result<Json<Value>, TerminalError> {
        Ok(Json(Value::Null))
    }
    #[handler]
    async fn create(
        &self,
        ctx: ObjectContext<'_>,
        Json(command): Json<CreateCommand>,
    ) -> Result<Json<DeliverySnapshot>, TerminalError> {
        if let Some(old) = ctx.get::<Json<DeliverySnapshot>>("snapshot").await? {
            assert_eq!(old.0.create.command, command);
            return Ok(old);
        }
        let snapshot = DeliverySnapshot::from(CreateReceipt {
            audit_version: ghinvite_core::delivery_audit::new_owner_version(command.invitation_id),
            command,
            outcome: CreateOutcome::Created { upstream_id: 99 },
            revision: 1,
            confirmed_at: Some(Utc::now()),
        });
        ctx.set("snapshot", Json(snapshot.clone()));
        Ok(Json(snapshot))
    }
    #[handler]
    async fn status(
        &self,
        ctx: SharedObjectContext<'_>,
    ) -> Result<Json<Option<DeliverySnapshot>>, TerminalError> {
        Ok(Json(
            ctx.get::<Json<DeliverySnapshot>>("snapshot")
                .await?
                .map(|j| j.0),
        ))
    }
    #[handler]
    async fn finish(
        &self,
        ctx: ObjectContext<'_>,
        Json(input): Json<Value>,
    ) -> Result<(), TerminalError> {
        let mut snapshot = ctx
            .get::<Json<DeliverySnapshot>>("snapshot")
            .await?
            .unwrap()
            .0;
        let state: InvitationState = serde_json::from_value(input["state"].clone()).unwrap();
        let command = &snapshot.create.command;
        let (target_kind, target_id) =
            ghinvite_core::delivery_audit::repository_target(&snapshot.create).unwrap();
        let event = ghinvite_core::audit::AuditEvent {
            id: ghinvite_core::delivery_audit::repository_settlement_id(&snapshot.create).unwrap(),
            account_id: command.account_id,
            occurred_at: Utc::now(),
            event_type: ghinvite_core::storage::settlement::event_type(state).unwrap(),
            actor_kind: ghinvite_core::audit::ActorKind::System,
            actor_id: None,
            target_kind,
            target_id,
            metadata: json!({"reason":input["reason"]}),
            request_id: None,
        };
        let notice = ghinvite_core::request_completion::DeliveryNotice {
            kind: ghinvite_core::delivery_control::DeliveryKind::Repository,
            target_id: command.repo_id,
            delivery_generation: command.delivery_generation,
        };
        let request_id = command.request_id;
        snapshot.settlement = Some(Settlement { state, event });
        snapshot.revision += 1;
        ctx.set("snapshot", Json(snapshot));
        ctx.object_client::<ghinvite_workflows::request_owner::InvitationRequestClient>(
            request_id.to_string(),
        )
        .on_delivery_settled(Json(notice))
        .send()
        .await?;
        Ok(())
    }
}
struct InvitationProjection;
// Authority seam only; production numeric GitHub authorization has its own
// real endpoint suite. This fixture isolates request aggregation/admission.
struct AccountInstallation;
#[restate_sdk::object]
impl AccountInstallation {
    #[handler]
    async fn decision_authority(
        &self,
        ctx: SharedObjectContext<'_>,
        Json(user_id): Json<u64>,
    ) -> Result<Json<bool>, TerminalError> {
        assert_eq!(ctx.key(), "repo:100");
        Ok(Json(user_id == 7))
    }
}
#[restate_sdk::object]
impl InvitationProjection {
    #[handler]
    async fn apply_transition(
        &self,
        _ctx: ObjectContext<'_>,
        _input: Json<Value>,
    ) -> Result<(), TerminalError> {
        Ok(())
    }
}
struct RequestProjection;
#[restate_sdk::object]
impl RequestProjection {
    #[handler]
    async fn apply_reissue(
        &self,
        ctx: ObjectContext<'_>,
        Json(input): Json<ghinvite_core::reissue_projection::RequestReissueProjection>,
    ) -> Result<(), TerminalError> {
        input.validate().unwrap();
        ctx.set("reissue", Json(input.clone()));
        ctx.set(
            "last",
            Json(
                ghinvite_core::storage::projection::RequestProjectionEnvelope {
                    request: input.after,
                    events: input.approval_events,
                    approval: None,
                },
            ),
        );
        Ok(())
    }

    #[handler]
    async fn apply(
        &self,
        ctx: ObjectContext<'_>,
        Json(input): Json<ghinvite_core::storage::projection::RequestProjectionEnvelope>,
    ) -> Result<(), TerminalError> {
        ctx.set("last", Json(input));
        Ok(())
    }
    #[handler]
    async fn inspect(
        &self,
        ctx: SharedObjectContext<'_>,
    ) -> Result<
        Json<Option<ghinvite_core::storage::projection::RequestProjectionEnvelope>>,
        TerminalError,
    > {
        Ok(Json(
            ctx.get::<Json<ghinvite_core::storage::projection::RequestProjectionEnvelope>>("last")
                .await?
                .map(|j| j.0),
        ))
    }
}
struct Rig {
    h: Harness,
    link_clock: Arc<ghinvite_workflows::admission::Faults>,
    request_clock: Arc<ghinvite_workflows::request_owner::Faults>,
}
impl Rig {
    async fn start() -> Self {
        let h = Harness::start().await;
        let link_clock = Arc::new(ghinvite_workflows::admission::Faults::default());
        let request_clock = Arc::new(ghinvite_workflows::request_owner::Faults::default());
        let builder = ghinvite_workflows::admission::bind_with_faults(
            restate_sdk::endpoint::Endpoint::builder(),
            link_clock.clone(),
        );
        let builder =
            ghinvite_workflows::request_owner::bind_with_faults(builder, request_clock.clone());
        h.restate
            .deploy(
                builder
                    .bind(AccountInstallation)
                    .bind(RepositoryDelivery)
                    .bind(InvitationProjection)
                    .bind(RequestProjection)
                    .build(),
            )
            .await;
        Self {
            h,
            link_clock,
            request_clock,
        }
    }
    fn time(&self, at: DateTime<Utc>) {
        self.link_clock.set_clock(Some(at));
        *self.request_clock.clock.lock().unwrap() = Some(at);
    }
    async fn call(&self, service: &str, key: impl ToString, handler: &str, input: &Value) -> Value {
        let request = self.h.client.post(format!(
            "{}/{}/{}/{}",
            self.h.restate.ingress_url(),
            service,
            key.to_string(),
            handler
        ));
        let response = if input.is_null() {
            request
        } else {
            request.json(input)
        }
        .send()
        .await
        .unwrap();
        let status = response.status();
        let text = response.text().await.unwrap();
        assert!(status.is_success(), "{service}/{handler}: {status} {text}");
        if text.is_empty() {
            Value::Null
        } else {
            serde_json::from_str(&text).unwrap()
        }
    }
    async fn create_link(&self, lifetime: u32, repos: &[u64], rules: Value) -> InvitationLinkId {
        let link = InvitationLinkId::new();
        let create = json!({"email_domains":rules,"secret_sha256":"7a".repeat(32),"link_id":link,"account_id":100,"installation_id":1,"admin":{"account_id":100,"user_id":7},"description":"Completion","max_uses":20,"approval_required":false,"invitation_lifetime_seconds":lifetime,"targets":{"organization":null,"teams":[],"repos":repos.iter().map(|id|json!({"repo_id":id,"repo_full_name":format!("acme/r{id}"),"grant":"write"})).collect::<Vec<_>>()}});
        assert!(
            self.call("InvitationLink", link, "create", &create)
                .await
                .get("Ok")
                .is_some()
        );
        link
    }
    async fn fixture(&self, lifetime: u32, repos: &[u64]) -> (InvitationLinkId, Value, Value) {
        let link = self.create_link(lifetime, repos, json!([])).await;
        let input = attempt(link);
        let receipt = self.call("InvitationLink", link, "admit", &input).await;
        for repo in repos {
            self.wait_delivery(receipt["result"]["request_id"].as_str().unwrap(), *repo)
                .await;
        }
        (link, input, receipt)
    }
    async fn wait_delivery(&self, request: &str, repo: u64) {
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            while self
                .call(
                    "RepositoryDelivery",
                    format!("{request}:{repo}:1"),
                    "status",
                    &Value::Null,
                )
                .await
                .is_null()
            {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
    }
    async fn request(&self, link: InvitationLinkId, id: &str) -> Value {
        self.call(
            "InvitationRequest",
            id,
            "request_status",
            &json!({"request_id":id,"link_id":link,"requester_id":8}),
        )
        .await
    }
    async fn wait_state(&self, link: InvitationLinkId, id: &str, state: &str) -> Value {
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                let value = self.request(link, id).await;
                if value["state"] == state {
                    return value;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap()
    }
}
fn attempt(link: InvitationLinkId) -> Value {
    json!({"scope_evidence":[],"secret_sha256":"7a".repeat(32),"secret_generation":1,"link_id":link,"operation_id":RequestId::new(),"requester_id":8})
}

#[tokio::test]
async fn email_domain_admission_uses_journaled_freshness_and_retains_exact_receipts() {
    let r = Rig::start().await;
    let now = DateTime::from_timestamp_millis(Utc::now().timestamp_millis()).unwrap();
    r.time(now);
    let rules = json!([{"subdomains":"example.com"}]);
    let link = r.create_link(604800, &[10], rules.clone()).await;
    let offer_input = json!({"link_id":link,"secret_sha256":"7a".repeat(32),"secret_generation":1});
    let offer = r.call("InvitationLink", link, "offer", &offer_input).await;
    assert_eq!(offer["email_domains"], rules);
    assert_eq!(offer["account_id"], 100);
    assert_eq!(offer["paused"], false);
    let mut keys: Vec<_> = offer
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "account_id",
            "approval_required",
            "email_domains",
            "link_id",
            "paused",
            "targets"
        ]
    );
    for (field, value) in [
        ("secret_generation", json!(2)),
        ("secret_sha256", json!("00".repeat(32))),
    ] {
        let mut invalid = offer_input.clone();
        invalid[field] = value;
        let response =
            r.h.client
                .post(format!(
                    "{}/InvitationLink/{link}/offer",
                    r.h.restate.ingress_url()
                ))
                .json(&invalid)
                .send()
                .await
                .unwrap();
        assert_eq!(response.status().as_u16(), 404);
    }
    for action in ["pause", "resume"] {
        r.call("InvitationLink", link, action, &json!({"link_id":link,"operation_id":RequestId::new(),"admin":{"account_id":100,"user_id":7}})).await;
        assert_eq!(
            r.call("InvitationLink", link, "offer", &offer_input).await["paused"],
            action == "pause"
        );
    }
    let evidence = |link_id: InvitationLinkId, at: DateTime<Utc>, domains: Value| json!({"link_id":link_id,"checked_at":at,"matched_domains":domains});
    let mismatch = json!({"domain_mismatch":{"required":rules}});
    let cases = [
        (Value::Null, json!("domain_evidence_missing")),
        (
            evidence(
                link,
                now - Duration::minutes(11),
                json!(["eng.example.com"]),
            ),
            json!("domain_evidence_stale"),
        ),
        (
            evidence(
                link,
                now + Duration::milliseconds(1),
                json!(["eng.example.com"]),
            ),
            json!("domain_evidence_stale"),
        ),
        (
            evidence(InvitationLinkId::new(), now, json!(["eng.example.com"])),
            json!("domain_evidence_stale"),
        ),
        (evidence(link, now, json!([])), mismatch.clone()),
        (
            evidence(link, now, json!(["example.com"])),
            mismatch.clone(),
        ),
        (
            evidence(link, now, json!(["badexample.com"])),
            mismatch.clone(),
        ),
        (
            evidence(link, now, json!(["users.noreply.github.com"])),
            mismatch.clone(),
        ),
        (
            evidence(link, now, json!(["eng.example.com", "private.other.test"])),
            mismatch,
        ),
    ];
    let mut refusals = Vec::new();
    for (proof, reason) in cases {
        let mut input = attempt(link);
        input["domain_evidence"] = proof;
        let receipt = r.call("InvitationLink", link, "admit", &input).await;
        assert_eq!(receipt["result"]["reason"], reason);
        refusals.push((input, receipt));
    }
    let admin_input = json!({"link_id":link,"admin":{"account_id":100,"user_id":7}});
    assert_eq!(
        r.call("InvitationLink", link, "link_status", &admin_input)
            .await["uses"],
        0
    );
    let query = json!({"link_id":link,"requester_id":8,"secret_sha256":"7a".repeat(32),"secret_generation":1});
    assert_eq!(
        r.call("InvitationLink", link, "requester_page", &query)
            .await["email_domains"],
        rules
    );
    let mut accepted_input = attempt(link);
    accepted_input["domain_evidence"] = evidence(
        link,
        now - Duration::minutes(10),
        json!(["ENG.EXAMPLE.COM."]),
    );
    let receipt = r
        .call("InvitationLink", link, "admit", &accepted_input)
        .await;
    assert_eq!(receipt["result"]["state"], "approved");
    let id = receipt["result"]["request_id"].as_str().unwrap();
    let request = r.request(link, id).await;
    assert_eq!(
        r.call("InvitationLink", link, "offer", &offer_input).await,
        offer,
        "offer reveals no requester or admission data"
    );
    assert_eq!(
        request["domain_evidence"]["matched_domains"],
        json!(["eng.example.com"])
    );
    assert_eq!(
        request["domain_evidence"]["checked_at"],
        json!(now - Duration::minutes(10))
    );
    // Evidence expiration does not change the original accepted operation.
    r.time(now + Duration::minutes(20));
    assert_eq!(
        r.call("InvitationLink", link, "admit", &accepted_input)
            .await,
        receipt
    );
    for (input, old) in refusals {
        assert_eq!(r.call("InvitationLink", link, "admit", &input).await, old);
    }
    let mut changed = accepted_input.clone();
    changed["domain_evidence"]["checked_at"] = json!(now + Duration::minutes(20));
    let response =
        r.h.client
            .post(format!(
                "{}/InvitationLink/{link}/admit",
                r.h.restate.ingress_url()
            ))
            .json(&changed)
            .send()
            .await
            .unwrap();
    assert_eq!(
        response.status().as_u16(),
        409,
        "re-auth must allocate a fresh operation"
    );
    assert_eq!(
        r.call("InvitationLink", link, "link_status", &admin_input)
            .await["uses"],
        1
    );
    r.h.finish().await;
}

#[tokio::test]
async fn prepared_domain_evidence_expires_but_recorded_admission_survives_interruption() {
    let r = Rig::start().await;
    let now = DateTime::from_timestamp_millis(Utc::now().timestamp_millis()).unwrap();
    r.time(now);
    let rules = json!([{"exact":"example.com"}]);
    let link = r.create_link(604800, &[10], rules.clone()).await;
    let mut input = attempt(link);
    input["domain_evidence"] =
        json!({"link_id":link,"checked_at":now,"matched_domains":["example.com"]});
    r.call("InvitationLink", link, "prepare_attempt", &input)
        .await;
    r.time(now + Duration::minutes(11));
    let refused = r.call("InvitationLink", link, "admit", &input).await;
    assert_eq!(refused["result"]["reason"], "domain_evidence_stale");

    // Prepared identity remains bound even when capability rotation makes the
    // original offer unavailable before any decision has been recorded.
    let rotated = r.create_link(604800, &[12], rules.clone()).await;
    let mut prepared = attempt(rotated);
    prepared["domain_evidence"] = json!({"link_id":rotated,"checked_at":now + Duration::minutes(11),"matched_domains":["example.com"]});
    r.call("InvitationLink", rotated, "prepare_attempt", &prepared)
        .await;
    r.call("InvitationLink", rotated, "rotate_secret", &json!({"link_id":rotated,"admin":{"account_id":100,"user_id":7},"operation_id":RequestId::new(),"new_sha256":"7b".repeat(32)})).await;
    let mut changed = prepared.clone();
    changed["domain_evidence"]["checked_at"] = json!(now);
    let response =
        r.h.client
            .post(format!(
                "{}/InvitationLink/{rotated}/admit",
                r.h.restate.ingress_url()
            ))
            .json(&changed)
            .send()
            .await
            .unwrap();
    assert_eq!(response.status().as_u16(), 409);
    let unavailable = r.call("InvitationLink", rotated, "admit", &prepared).await;
    assert_eq!(unavailable["result"]["reason"], "not_available");
    assert_eq!(
        r.call("InvitationLink", rotated, "admit", &prepared).await,
        unavailable
    );

    let other = r.create_link(604800, &[11], rules).await;
    let mut accepted_input = attempt(other);
    let admitted_at = now + Duration::minutes(11);
    accepted_input["domain_evidence"] =
        json!({"link_id":other,"checked_at":admitted_at,"matched_domains":["example.com"]});
    r.link_clock.arm("before-initialize");
    let request =
        r.h.client
            .post(format!(
                "{}/InvitationLink/{other}/admit",
                r.h.restate.ingress_url()
            ))
            .json(&accepted_input);
    let pending = tokio::spawn(async move { request.send().await.unwrap() });
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while r.link_clock.attempts() == 0 {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    r.time(now + Duration::hours(1));
    r.link_clock.release();
    let response = pending.await.unwrap();
    assert!(response.status().is_success());
    let receipt: Value = response.json().await.unwrap();
    assert_eq!(receipt["result"]["state"], "approved");
    assert_eq!(receipt["decided_at"], json!(admitted_at));
    let request = r
        .request(other, receipt["result"]["request_id"].as_str().unwrap())
        .await;
    assert_eq!(request["admitted_at"], json!(admitted_at));
    assert_eq!(request["domain_evidence"]["checked_at"], json!(admitted_at));
    assert_eq!(
        r.call("InvitationLink", other, "admit", &accepted_input)
            .await,
        receipt
    );
    let admin = json!({"link_id":other,"admin":{"account_id":100,"user_id":7}});
    assert_eq!(
        r.call("InvitationLink", other, "link_status", &admin).await["uses"],
        1
    );
    r.h.finish().await;
}

#[tokio::test]
async fn own_expiry_lapses_without_sql_and_rerequests_preserve_receipts_limits_and_approval() {
    let r = Rig::start().await;
    let now = DateTime::from_timestamp_millis(Utc::now().timestamp_millis()).unwrap();
    r.time(now);
    let (link, input, admitted) = r.fixture(3600, &[10]).await;
    let id = admitted["result"]["request_id"].as_str().unwrap();
    let approved = r.request(link, id).await;
    r.call(
        "RepositoryDelivery",
        format!("{id}:10:1"),
        "finish",
        &json!({"state":"cancelled","reason":"own_expiry"}),
    )
    .await;
    let lapsed = r.wait_state(link, id, "lapsed").await;
    assert_eq!(lapsed["decision"], approved["decision"]);
    assert_eq!(
        lapsed["revision"].as_u64().unwrap(),
        approved["revision"].as_u64().unwrap() + 1
    );
    assert_eq!(
        r.call("InvitationLink", link, "admit", &input).await,
        admitted,
        "old successful receipt stays approved"
    );
    let projected = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let v = r
                .call("RequestProjection", id, "inspect", &Value::Null)
                .await;
            if v["request"]["state"] == "lapsed" {
                break v;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(projected["approval"]["request"], approved);
    let retry = attempt(link);
    let second = r.call("InvitationLink", link, "admit", &retry).await;
    assert_eq!(
        second["result"]["state"], "pending",
        "short lifetime forces fresh approval"
    );
    assert_eq!(
        r.call("InvitationLink", link, "admit", &retry).await,
        second
    );
    assert_eq!(
        r.call("InvitationLink", link, "admit", &attempt(link))
            .await["result"]["reason"],
        "already_pending"
    );
    let second_id = second["result"]["request_id"].as_str().unwrap();
    r.call("InvitationRequest",second_id,"decide",&json!({"link_id":link,"request_id":second_id,"operation_id":RequestId::new(),"admin":{"account_id":100,"user_id":7},"action":{"kind":"decline","reason":null}})).await;
    let limited_input = attempt(link);
    let limited = r
        .call("InvitationLink", link, "admit", &limited_input)
        .await;
    assert_eq!(
        limited["result"]["reason"]["rerequest_limited"]["retry_after"],
        json!(now + Duration::days(7))
    );
    let page=r.call("InvitationLink",link,"requester_page",&json!({"link_id":link,"requester_id":8,"secret_sha256":"7a".repeat(32),"secret_generation":1})).await;
    assert_eq!(page["can_start_fresh"], false);
    assert_eq!(page["rerequest_after"], json!(now + Duration::days(7)));
    r.time(now + Duration::days(7));
    assert_eq!(
        r.call("InvitationLink", link, "admit", &limited_input)
            .await,
        limited,
        "refusal receipt immutable after window"
    );
    let third = r
        .call("InvitationLink", link, "admit", &attempt(link))
        .await;
    assert_eq!(third["result"]["state"], "pending");
    let admin = r
        .call(
            "InvitationLink",
            link,
            "link_status",
            &json!({"link_id":link,"admin":{"account_id":100,"user_id":7}}),
        )
        .await;
    assert_eq!(
        admin["uses"], 3,
        "only three successful requests consume uses"
    );
    r.h.finish().await;
}

#[tokio::test]
async fn mixed_results_never_lapse_and_old_generation_notices_cannot_supply_completion() {
    let r = Rig::start().await;
    let (link, _, admitted) = r.fixture(604800, &[10, 11]).await;
    let id = admitted["result"]["request_id"].as_str().unwrap();
    r.call(
        "InvitationRequest",
        id,
        "on_delivery_settled",
        &json!({"kind":"repository","target_id":11,"delivery_generation":99}),
    )
    .await;
    r.call(
        "RepositoryDelivery",
        format!("{id}:10:1"),
        "finish",
        &json!({"state":"expired"}),
    )
    .await;
    assert_eq!(
        r.request(link, id).await["state"],
        "approved",
        "missing final target is not completion"
    );
    r.call(
        "RepositoryDelivery",
        format!("{id}:11:1"),
        "finish",
        &json!({"state":"accepted"}),
    )
    .await;
    // Exclusive notification barrier repeats the final read after the first send.
    r.call(
        "InvitationRequest",
        id,
        "on_delivery_settled",
        &json!({"kind":"repository","target_id":11,"delivery_generation":1}),
    )
    .await;
    assert_eq!(r.request(link, id).await["state"], "approved");
    assert_eq!(
        r.call("InvitationLink", link, "admit", &attempt(link))
            .await["result"]["reason"],
        "already_approved"
    );
    r.h.finish().await;
}

#[tokio::test]
async fn interrupted_lapse_replays_original_completion_and_duplicate_notices_do_not_revise_it() {
    let r = Rig::start().await;
    let now = DateTime::from_timestamp_millis(Utc::now().timestamp_millis()).unwrap();
    r.time(now);
    let (link, _, admitted) = r.fixture(3600, &[10]).await;
    let id = admitted["result"]["request_id"].as_str().unwrap();
    let approved = r.request(link, id).await;
    *r.request_clock.stage.lock().unwrap() = Some("after-lapsed-recorded".into());
    r.call(
        "RepositoryDelivery",
        format!("{id}:10:1"),
        "finish",
        &json!({"state":"cancelled","reason":"own_expiry"}),
    )
    .await;
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while r
            .request_clock
            .reached
            .load(std::sync::atomic::Ordering::SeqCst)
            == 0
        {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    r.time(now + Duration::hours(1));
    *r.request_clock.stage.lock().unwrap() = None;
    let lapsed = r.wait_state(link, id, "lapsed").await;
    let notice = json!({"kind":"repository","target_id":10,"delivery_generation":1});
    for _ in 0..3 {
        r.call("InvitationRequest", id, "on_delivery_settled", &notice)
            .await;
    }
    assert_eq!(r.request(link, id).await, lapsed);
    assert_eq!(lapsed["decision"], approved["decision"]);
    let envelope = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let value = r
                .call("RequestProjection", id, "inspect", &Value::Null)
                .await;
            if value["request"]["state"] == "lapsed" {
                break value;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let event = envelope["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["kind"] == "request.lapsed")
        .unwrap();
    assert_eq!(
        event["effective_at"],
        json!(now),
        "projection retries preserve original completion time"
    );
    assert_eq!(envelope["approval"]["request"], approved);
    r.h.finish().await;
}

#[path = "request_completion/reissue.rs"]
mod reissue;
