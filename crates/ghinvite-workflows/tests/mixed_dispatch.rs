//! Real admission/request/control owners with typed delivery recorders. These
//! tests cover dispatch identity and ordering; organization_delivery covers HTTP.
#![cfg(feature = "integration")]
use ghinvite_core::{InvitationLinkId, RequestId, delivery_control::DeliveryKind};
use ghinvite_workflows::delivery_control::DeliveryControlInput;
use restate_sdk::{
    context::{ContextReadState, ContextWriteState, ObjectContext, SharedObjectContext},
    errors::TerminalError,
    serde::Json,
};
use serde_json::{Value, json};
use std::{
    sync::{Arc, atomic::Ordering},
    time::Duration,
};

macro_rules! recorder {
    ($name:ident, $kind:expr) => {
        struct $name;
        #[restate_sdk::object]
        impl $name {
            #[handler]
            async fn control(&self, ctx: ObjectContext<'_>, Json(input): Json<DeliveryControlInput>) -> Result<Json<Value>, TerminalError> {
                assert_eq!(input.envelope.identity.kind, $kind);
                ctx.set("control", Json(input));
                Ok(Json(Value::Null))
            }
            #[handler]
            async fn create(&self, ctx: ObjectContext<'_>, Json(input): Json<Value>) -> Result<Json<Value>, TerminalError> {
                let control = ctx.get::<Json<DeliveryControlInput>>("control").await?.expect("control must precede every create").0;
                assert_eq!(control.envelope.identity.kind, $kind);
                assert_eq!(input["request_id"], json!(control.envelope.identity.request_id));
                if let Some(Json(old)) = ctx.get::<Json<Value>>("command").await? {
                    assert_eq!(old, input, "recovery must resend the exact retained command");
                }
                ctx.set("command", Json(input));
                ctx.set("creates", ctx.get::<u64>("creates").await?.unwrap_or(0) + 1);
                Ok(Json(Value::Null))
            }
            #[handler]
            async fn inspect(&self, ctx: SharedObjectContext<'_>) -> Result<Json<Value>, TerminalError> {
                Ok(Json(json!({"command":ctx.get::<Json<Value>>("command").await?.map(|j|j.0),"creates":ctx.get::<u64>("creates").await?.unwrap_or(0),"control":ctx.get::<Json<DeliveryControlInput>>("control").await?.map(|j|j.0)})))
            }
            #[handler]
            async fn cascade_progress(&self, ctx: SharedObjectContext<'_>, Json(query): Json<ghinvite_core::revocation::DeliveryCascadeQuery>) -> Result<Json<ghinvite_core::revocation::DeliveryCascadeProgress>, TerminalError> {
                use ghinvite_core::revocation::{CascadeEntryState,DeliveryCascadeProgress};
                assert_eq!(query.identity.kind, $kind, "numeric collisions must route by target kind");
                let current = ctx.get::<Json<DeliveryControlInput>>("control").await?.expect("initial control").0;
                assert_eq!(current.envelope.identity,query.identity);
                Ok(Json(DeliveryCascadeProgress { observed_control_revision: current.envelope.revision, state: if matches!(current.envelope.control, ghinvite_core::delivery_control::LinkControl::Revoked { .. }) { CascadeEntryState::Done } else { CascadeEntryState::Pending } }))
            }
        }
    };
}
recorder!(RepositoryDelivery, DeliveryKind::Repository);
recorder!(OrganizationDelivery, DeliveryKind::Organization);

// Deliberately withhold SQL projections. Authority and dispatch must not depend
// on rows appearing; actual projection invariants have shared SQLx/D1 suites.
struct InvitationProjection;
#[restate_sdk::object]
impl InvitationProjection {
    #[handler]
    async fn apply_transition(
        &self,
        _ctx: ObjectContext<'_>,
        _input: Json<ghinvite_core::storage::projection::ProjectionEnvelope>,
    ) -> Result<(), TerminalError> {
        Ok(())
    }
    #[handler]
    async fn apply_revocation(
        &self,
        _ctx: ObjectContext<'_>,
        _input: Json<ghinvite_core::revocation::RevocationProjection>,
    ) -> Result<(), TerminalError> {
        Ok(())
    }
}
struct RequestProjection;
#[restate_sdk::object]
impl RequestProjection {
    #[handler]
    async fn apply(
        &self,
        _ctx: ObjectContext<'_>,
        _input: Json<ghinvite_core::storage::projection::RequestProjectionEnvelope>,
    ) -> Result<(), TerminalError> {
        Ok(())
    }
}
async fn call(
    h: &ghinvite_e2e::Harness,
    service: &str,
    key: impl ToString,
    method: &str,
    input: &Value,
) -> Value {
    let url = format!(
        "{}/{}/{}/{}",
        h.restate.ingress_url(),
        service,
        key.to_string(),
        method
    );
    let request = h.client.post(url);
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
    assert!(status.is_success(), "{service}/{method}: {status} {text}");
    if text.is_empty() {
        Value::Null
    } else {
        serde_json::from_str(&text).unwrap()
    }
}

#[tokio::test]
async fn mixed_dispatch_retains_app_identities_recovers_and_cascades_numeric_collisions() {
    let h = ghinvite_e2e::Harness::start().await;
    let faults = Arc::new(ghinvite_workflows::request_owner::Faults::default());
    *faults.stage.lock().unwrap() = Some("after-organization-send".into());
    let builder = ghinvite_workflows::admission::bind_with_capabilities(
        restate_sdk::endpoint::Endpoint::builder(),
        true,
        true,
    );
    let builder = ghinvite_workflows::request_owner::bind_with_faults_and_organization_targets(
        builder,
        faults.clone(),
        true,
    );
    h.restate
        .deploy(
            builder
                .bind(RepositoryDelivery)
                .bind(OrganizationDelivery)
                .bind(InvitationProjection)
                .bind(RequestProjection)
                .build(),
        )
        .await;
    let link = InvitationLinkId::new();
    let create = json!({"secret_sha256":"2a".repeat(32),"link_id":link,"account_id":100,"installation_id":1,"organization_installation_id":2,"budget_share_pct":17,"admin":{"account_id":100,"user_id":7},"description":"Mixed dispatch","max_uses":10,"approval_required":false,"targets":{"organization":{"org_id":100,"org_login":"acme","grant":"member"},"teams":[{"team_id":50,"team_slug":"devs","grant":"member"}],"repos":[{"repo_id":100,"repo_full_name":"acme/api","grant":"maintain"}]}});
    assert!(
        call(&h, "InvitationLink", link, "create", &create)
            .await
            .get("Ok")
            .is_some()
    );
    let attempt = json!({"scope_evidence":[],"secret_sha256":"2a".repeat(32),"secret_generation":1,"link_id":link,"operation_id":RequestId::new(),"requester_id":8,"justification":null});
    let admitted = call(&h, "InvitationLink", link, "admit", &attempt).await;
    assert_eq!(admitted["result"]["kind"], "accepted");
    let request = admitted["result"]["request_id"].as_str().unwrap();
    let query = json!({"request_id":request,"link_id":link,"requester_id":8});
    tokio::time::timeout(Duration::from_secs(10), async {
        while faults.reached.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let plan = call(&h, "InvitationRequest", request, "approved_plan", &query).await;
    assert_eq!(plan["commands"].as_array().unwrap().len(), 1);
    assert_eq!(plan["commands"][0]["installation_id"], 1);
    assert_eq!(plan["commands"][0]["permission"], "maintain");
    assert_eq!(plan["organization_command"]["installation_id"], 2);
    assert_eq!(plan["organization_command"]["budget_share_pct"], 17);
    assert_eq!(
        plan["organization_command"]["teams"],
        create["targets"]["teams"]
    );
    let repository: ghinvite_core::delivery::CreateCommand =
        serde_json::from_value(plan["commands"][0].clone()).unwrap();
    let organization: ghinvite_core::org_delivery::OrganizationCreateCommand =
        serde_json::from_value(plan["organization_command"].clone()).unwrap();
    assert!(repository.invitation_id.is_none());
    assert!(organization.invitation_id.is_none());
    // The numeric targets deliberately collide. Service ownership and the
    // organization storage namespace separate these ordinary obligations;
    // neither command invents a legacy invitation identity.
    assert_eq!(repository.delivery_key(), organization.delivery_key());
    assert_ne!(repository.delivery_key(), organization.storage_id());
    // Send is retained but its local submitted marker has not yet been written.
    let organization_progress = call(
        &h,
        "InvitationRequest",
        request,
        "organization_delivery_progress",
        &query,
    )
    .await;
    assert_eq!(organization_progress["delivery_generation"], 1);
    assert_eq!(organization_progress["stage"], "planned");
    *faults.stage.lock().unwrap() = None;
    let key = format!("{request}:100:1");
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if call(
                &h,
                "InvitationRequest",
                request,
                "organization_delivery_progress",
                &query,
            )
            .await["stage"]
                == "submitted"
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    for service in ["RepositoryDelivery", "OrganizationDelivery"] {
        let observed = call(&h, service, &key, "inspect", &Value::Null).await;
        assert_eq!(
            observed["creates"], 1,
            "interrupted dispatch must not resend"
        );
    }
    let recovered = call(&h, "InvitationRequest", request, "recover", &query).await;
    assert_eq!(recovered["plan"], plan);
    assert_eq!(
        recovered["submitted_organization"]["command"],
        plan["organization_command"]
    );
    assert_eq!(
        call(&h, "InvitationLink", link, "admit", &attempt).await,
        admitted
    );
    let revoke = json!({"link_id":link,"admin":{"account_id":100,"user_id":7},"operation_id":RequestId::new(),"cascade":{"decline_pending_requests":false,"cancel_pending_invitations":true,"remove_granted_access":false}});
    let receipt = call(&h, "InvitationLink", link, "revoke", &revoke).await;
    assert_eq!(
        receipt,
        json!({"Ok":{"addressed_requests":0,"addressed_deliveries":2}})
    );
    let admin = json!({"link_id":link,"admin":{"account_id":100,"user_id":7}});
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let progress = call(&h, "InvitationLink", link, "cascade_progress", &admin).await;
            assert_eq!(progress["addressed_deliveries"], 2);
            if progress["done_deliveries"] == 2 {
                assert_eq!(progress["unavailable"], 0);
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        call(&h, "InvitationLink", link, "revoke", &revoke).await,
        receipt
    );
    assert_eq!(
        call(&h, "InvitationRequest", request, "approved_plan", &query).await,
        plan
    );
    for service in ["RepositoryDelivery", "OrganizationDelivery"] {
        assert_eq!(
            call(&h, service, &key, "inspect", &Value::Null).await["creates"],
            2,
            "explicit recovery alone resends the same command"
        );
    }
    let denied = h
        .client
        .post(format!(
            "{}/InvitationRequest/{request}/organization_delivery_progress",
            h.restate.ingress_url()
        ))
        .json(&json!({"request_id":request,"link_id":link,"requester_id":9}))
        .send()
        .await
        .unwrap();
    assert_eq!(
        denied.status(),
        404,
        "another requester cannot read dispatch progress"
    );

    let org_link = InvitationLinkId::new();
    let mut org_create = create.clone();
    org_create["link_id"] = json!(org_link);
    org_create["installation_id"] = json!(2);
    org_create
        .as_object_mut()
        .unwrap()
        .remove("organization_installation_id");
    org_create["targets"]["repos"] = json!([]);
    let created = call(&h, "InvitationLink", org_link, "create", &org_create).await;
    assert!(created.get("Ok").is_some(), "{created}");
    let org_attempt = json!({"scope_evidence":[],"secret_sha256":"2a".repeat(32),"secret_generation":1,"link_id":org_link,"operation_id":RequestId::new(),"requester_id":9,"justification":null});
    let org_admitted = call(&h, "InvitationLink", org_link, "admit", &org_attempt).await;
    let org_request = org_admitted["result"]["request_id"].as_str().unwrap();
    let org_query = json!({"request_id":org_request,"link_id":org_link,"requester_id":9});
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if call(
                &h,
                "InvitationRequest",
                org_request,
                "organization_delivery_progress",
                &org_query,
            )
            .await["stage"]
                == "submitted"
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let org_plan = call(
        &h,
        "InvitationRequest",
        org_request,
        "approved_plan",
        &org_query,
    )
    .await;
    assert_eq!(org_plan["commands"], json!([]));
    assert_eq!(org_plan["organization_command"]["installation_id"], 2);
    assert_eq!(
        call(
            &h,
            "InvitationRequest",
            org_request,
            "delivery_progress",
            &org_query
        )
        .await,
        json!([])
    );
    let manifest = call(
        &h,
        "InvitationRequest",
        org_request,
        "cascade_manifest",
        &json!({"request_id":org_request,"link_id":org_link}),
    )
    .await;
    assert_eq!(manifest["deliveries"].as_array().unwrap().len(), 1);
    assert_eq!(manifest["deliveries"][0]["kind"], "organization");
    h.finish().await;
}
