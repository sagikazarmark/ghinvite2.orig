//! Hash-only capability authority under projection outage, using real Restate.
#![cfg(feature = "integration")]
#[path = "common/installation_context.rs"]
mod installation_context;
use ghinvite_core::storage::projection::{
    ProjectionEnvelope, ProjectionStorage, RequestProjectionEnvelope,
};
use ghinvite_core::{InvitationLinkId, RequestId};
use ghinvite_e2e::{Call, Harness};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

#[derive(Default)]
struct PausedProjection(AtomicUsize);
#[async_trait::async_trait]
impl ProjectionStorage for PausedProjection {
    async fn apply_reissue(
        &self,
        _: &ghinvite_core::reissue_projection::RequestReissueProjection,
    ) -> ghinvite_core::storage::Result<()> {
        Err(ghinvite_core::storage::Error::ProjectionDependency)
    }

    async fn apply_revocation(
        &self,
        _: &ghinvite_core::revocation::RevocationProjection,
    ) -> ghinvite_core::storage::Result<()> {
        Err(ghinvite_core::storage::Error::ProjectionDependency)
    }

    async fn apply_transition(&self, _: &ProjectionEnvelope) -> ghinvite_core::storage::Result<()> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Err(ghinvite_core::storage::Error::ProjectionDependency)
    }
    async fn apply_request(
        &self,
        _: &RequestProjectionEnvelope,
    ) -> ghinvite_core::storage::Result<()> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Err(ghinvite_core::storage::Error::ProjectionDependency)
    }
}
fn hash(secret: &[u8]) -> String {
    ghinvite_core::invitation_link::SecretHash::from_bytes(Sha256::digest(secret).into()).as_hex()
}
async fn invoke(
    h: &Harness,
    id: InvitationLinkId,
    handler: &str,
    body: &Value,
    expected: u16,
    ids: &mut Vec<String>,
) -> Value {
    let reply = h
        .restate
        .invoke(
            &Call::object("InvitationLink", &id.to_string(), handler),
            Some(body),
            None,
        )
        .await;
    assert_eq!(reply.status, expected, "{handler}: {}", reply.body);
    ids.push(reply.invocation_id().into());
    reply.body
}
fn attempt(id: InvitationLinkId, hash: &str, generation: u32, user: u64) -> Value {
    json!({"link_id":id,"operation_id":RequestId::new(),"requester_id":user,"justification":null,
        "secret_sha256":hash,"secret_generation":generation,"scope_evidence":[]})
}
#[tokio::test]
async fn rotation_and_admission_are_authoritative_without_projection_or_plaintext() {
    let h = Harness::start().await;
    let context = installation_context::InstallationContext::start(
        Arc::new(
            ghinvite_storage_sqlx::SqlxStorage::in_memory()
                .await
                .unwrap(),
        ),
        h.client.clone(),
    )
    .await;
    let projection = Arc::new(PausedProjection::default());
    let builder = ghinvite_workflows::projection::bind(
        restate_sdk::endpoint::Endpoint::builder(),
        projection.clone(),
    );
    let builder = ghinvite_workflows::request_owner::bind(builder);
    h.restate
        .deploy(ghinvite_workflows::admission::bind(context.bind(builder)).build())
        .await;
    context.seed(&h.client, h.restate.ingress_url()).await;
    ghinvite_e2e::journal::JournalAudit::retain(&h, &["InvitationLink", "InvitationRequest"]).await;
    let marker = b"LINKSECRET_MARKER_original_32bytes";
    let next_marker = b"LINKSECRET_MARKER_rotated_32bytes!";
    let old_hash = hash(marker);
    let new_hash = hash(next_marker);
    let id = InvitationLinkId::new();
    let admin = json!({"account_id":100,"user_id":7});
    let create = json!({"link_id":id,"account_id":100,"installation_id":1,"admin":admin,"description":"Capability proof",
        "approval_required":true,"max_uses":2,"budget_share_pct":17,"secret_sha256":old_hash,
        "targets":{"organization":null,"teams":[],"repos":[{"repo_id":10,"repo_full_name":"acme/api","grant":"read"},{"repo_id":11,"repo_full_name":"acme/web","grant":"write"}]}});
    let mut ids = vec![];
    let unguarded_id = InvitationLinkId::new();
    let mut unguarded = create.clone();
    unguarded["link_id"] = json!(unguarded_id);
    unguarded["approval_required"] = json!(false);
    unguarded["max_uses"] = Value::Null;
    let refusal = invoke(&h, unguarded_id, "create", &unguarded, 200, &mut ids).await;
    assert_eq!(refusal, json!({"Err":{"kind":"no_guardrail"}}));
    assert_eq!(
        invoke(&h, unguarded_id, "create", &unguarded, 200, &mut ids).await,
        refusal
    );
    let mut canonical_replay = unguarded.clone();
    canonical_replay["description"] = json!("  Capability proof  ");
    assert_eq!(
        invoke(&h, unguarded_id, "create", &canonical_replay, 200, &mut ids).await,
        refusal
    );
    let mut corrected = unguarded.clone();
    corrected["max_uses"] = json!(1);
    invoke(&h, unguarded_id, "create", &corrected, 409, &mut ids).await;
    invoke(
        &h,
        unguarded_id,
        "link_status",
        &json!({"link_id":unguarded_id,"admin":admin}),
        404,
        &mut ids,
    )
    .await;
    let empty_id = InvitationLinkId::new();
    let mut no_targets = create.clone();
    no_targets["link_id"] = json!(empty_id);
    no_targets["targets"]["repos"] = json!([]);
    let refusal = invoke(&h, empty_id, "create", &no_targets, 200, &mut ids).await;
    assert_eq!(
        refusal,
        json!({"Err":{"kind":"invalid_targets","reason":"empty"}})
    );
    let initial = invoke(&h, id, "create", &create, 200, &mut ids).await;
    assert_eq!(initial, json!({"Ok":{"link_id":id,"secret_generation":1}}));
    let mut changed_share = create.clone();
    changed_share["budget_share_pct"] = json!(18);
    invoke(&h, id, "create", &changed_share, 409, &mut ids).await;
    let accepted_input = attempt(id, &old_hash, 1, 8);
    let accepted = invoke(&h, id, "admit", &accepted_input, 200, &mut ids).await;
    assert_eq!(accepted["result"]["kind"], "accepted");
    let rotation =
        json!({"link_id":id,"operation_id":RequestId::new(),"admin":admin,"new_sha256":new_hash});
    let rotated = invoke(&h, id, "rotate_secret", &rotation, 200, &mut ids).await;
    assert_eq!(rotated, json!({"secret_generation":2}));
    assert_eq!(
        invoke(&h, id, "rotate_secret", &rotation, 200, &mut ids).await,
        rotated
    );
    let mut conflicting = rotation.clone();
    conflicting["new_sha256"] = json!(old_hash);
    invoke(&h, id, "rotate_secret", &conflicting, 409, &mut ids).await;
    let denied = invoke(
        &h,
        id,
        "admit",
        &attempt(id, &old_hash, 1, 9),
        200,
        &mut ids,
    )
    .await;
    assert_eq!(
        denied["result"],
        json!({"kind":"rejected","reason":"not_available"})
    );
    let unknown = InvitationLinkId::new();
    let absent = invoke(
        &h,
        unknown,
        "admit",
        &attempt(unknown, &old_hash, 1, 9),
        200,
        &mut ids,
    )
    .await;
    assert_eq!(absent["result"], denied["result"]);
    let wrong_generation = invoke(
        &h,
        id,
        "admit",
        &attempt(id, &new_hash, 1, 9),
        200,
        &mut ids,
    )
    .await;
    assert_eq!(wrong_generation["result"], denied["result"]);
    invoke(
        &h,
        id,
        "requester_page",
        &json!({"link_id":id,"requester_id":8,"secret_sha256":old_hash,"secret_generation":1}),
        404,
        &mut ids,
    )
    .await;
    assert_eq!(
        invoke(&h, id, "admit", &accepted_input, 200, &mut ids).await,
        accepted
    );
    let fresh = invoke(
        &h,
        id,
        "admit",
        &attempt(id, &new_hash, 2, 9),
        200,
        &mut ids,
    )
    .await;
    assert_eq!(fresh["result"]["kind"], "accepted");
    let exhausted = invoke(
        &h,
        id,
        "admit",
        &attempt(id, &new_hash, 2, 10),
        200,
        &mut ids,
    )
    .await;
    assert_eq!(exhausted["result"], denied["result"]);
    let status_input = json!({"link_id":id,"admin":admin});
    let status = invoke(&h, id, "link_status", &status_input, 200, &mut ids).await;
    assert_eq!(status["uses"], 2);
    assert_eq!(status["secret"]["secret_generation"], 2);
    assert_eq!(status["creation"]["targets"]["repos"][0]["grant"], "read");
    assert_eq!(status["creation"]["targets"]["repos"][1]["grant"], "write");
    let revocation = json!({"link_id":id,"admin":admin,"operation_id":RequestId::new(),"cascade":{
        "decline_pending_requests":false,"cancel_pending_invitations":false,"remove_granted_access":false
    }});
    let revoked = invoke(&h, id, "revoke", &revocation, 200, &mut ids).await;
    assert_eq!(
        revoked,
        json!({"Ok":{"addressed_requests":0,"addressed_deliveries":0}})
    );
    assert_eq!(
        invoke(&h, id, "revoke", &revocation, 200, &mut ids).await,
        revoked
    );
    assert_eq!(
        invoke(&h, id, "admit", &accepted_input, 200, &mut ids).await,
        accepted
    );
    assert_eq!(
        invoke(&h, id, "rotate_secret", &rotation, 200, &mut ids).await,
        rotated
    );
    // Approval freezes each requested grant, rather than a link-wide maximum.
    let request_id = accepted["result"]["request_id"].as_str().unwrap();
    let decided = h.restate.invoke(&Call::object("InvitationRequest", request_id, "decide"), Some(&json!({
        "request_id":request_id,"link_id":id,"operation_id":RequestId::new(),"admin":admin,"action":{"kind":"approve"}
    })), None).await;
    assert_eq!(decided.status, 200, "{}", decided.body);
    ids.push(decided.invocation_id().into());
    let plan = h
        .restate
        .invoke(
            &Call::object("InvitationRequest", request_id, "approved_plan"),
            Some(&json!({
                "request_id":request_id,"link_id":id,"requester_id":8
            })),
            None,
        )
        .await;
    assert_eq!(plan.status, 200, "{}", plan.body);
    assert_eq!(plan.body["commands"][0]["permission"], "pull");
    assert_eq!(plan.body["commands"][1]["permission"], "push");
    assert_eq!(plan.body["input"]["budget_share_pct"], 17);
    for command in plan.body["commands"].as_array().unwrap() {
        assert_eq!(command["budget_share_pct"], 17);
    }
    ids.push(plan.invocation_id().into());
    // All invocation inputs, outputs, run results and state writes are scanned
    // as raw journal bytes, not JSON integer-array rendering.
    let mut scanned = 0;
    for invocation in ids {
        let journal = h.restate.admin().journal(&invocation).await;
        assert!(!journal.is_empty());
        for entry in journal {
            assert!(
                !entry
                    .raw
                    .windows(b"LINKSECRET_MARKER".len())
                    .any(|w| w == b"LINKSECRET_MARKER")
            );
            scanned += 1;
        }
    }
    assert!(projection.0.load(Ordering::SeqCst) > 0);
    eprintln!(
        "capability proof scanned {scanned} real journal entries; projection remained unavailable"
    );
    h.finish().await;
}
