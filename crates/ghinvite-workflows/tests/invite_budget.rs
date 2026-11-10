//! Preactivation runtime proof. Binds the real retained budget and SQL projector
//! explicitly; no production endpoint or GitHub write is activated by this test.
#![cfg(feature = "integration")]
use ghinvite_core::{
    InvitationLinkId,
    account::{AccountInstallationRef, App},
    budget::BudgetSnapshot,
    storage::invite_budget::InviteBudgetStorage,
};
use ghinvite_e2e::{Call, Harness, journal::JournalAudit};
use serde_json::{Value, json};
use std::sync::{
    Arc,
    atomic::{AtomicI64, Ordering},
};
async fn call(h: &Harness, key: &str, handler: &str, input: Option<&Value>) -> Value {
    let result = h
        .restate
        .invoke(&Call::object("InviteBudget", key, handler), input, None)
        .await;
    assert_eq!(result.status, 200, "{handler}: {}", result.body);
    result.body
}
fn reserve(key: &str, link: Option<InvitationLinkId>) -> Value {
    json!({"request_key":key,"kind":{"kind":"org_invite"},"link_id":link})
}
fn set(clock: &AtomicI64, seconds: i64) {
    clock.store(seconds * 1000, Ordering::SeqCst);
}
#[tokio::test]
async fn real_retained_budget_enforces_share_spacing_caps_closure_and_exact_replay() {
    let h = Harness::start().await;
    let storage = Arc::new(
        ghinvite_storage_sqlx::SqlxStorage::in_memory()
            .await
            .unwrap(),
    );
    let base = 1_800_000_000;
    let clock = Arc::new(AtomicI64::new(base * 1000));
    let budget = ghinvite_workflows::invite_budget::InviteBudget {
        clock: Some(clock.clone()),
    };
    h.restate
        .deploy(
            ghinvite_workflows::invite_budget::bind_pre_activation(
                restate_sdk::endpoint::Endpoint::builder(),
                storage.clone(),
                budget,
            )
            .build(),
        )
        .await;
    JournalAudit::retain(&h, &["InviteBudget", "InviteBudgetProjection"]).await;
    let link = InvitationLinkId::new();
    let first = reserve("delivery:1:attempt:1", Some(link));
    let path = Call::object("InviteBudget", "org:7", "reserve");
    let original = h
        .restate
        .invoke(&path, Some(&first), Some("http-replay-proof"))
        .await;
    assert_eq!(original.status, 200);
    assert_eq!(original.body["kind"], "granted");
    let retained = h
        .restate
        .invoke(&path, Some(&first), Some("http-replay-proof"))
        .await;
    assert_eq!(original.invocation_id(), retained.invocation_id());
    assert_eq!(original.body, retained.body);
    assert!(
        !h.restate
            .admin()
            .journal(original.invocation_id())
            .await
            .is_empty()
    );
    assert_eq!(
        call(&h, "org:7", "reserve", Some(&first)).await,
        original.body
    );
    let spacing = call(&h, "org:7", "reserve", Some(&reserve("spacing", None))).await;
    assert_eq!(spacing["why"], "spacing");
    set(&clock, base + 1);
    assert_eq!(
        call(&h, "org:7", "reserve", Some(&reserve("spacing", None))).await,
        spacing
    );
    for i in 1..25 {
        set(&clock, base + i);
        assert_eq!(
            call(
                &h,
                "org:7",
                "reserve",
                Some(&reserve(&format!("link-{i}"), Some(link)))
            )
            .await["kind"],
            "granted"
        );
    }
    set(&clock, base + 25);
    let shared = call(&h, "org:7", "reserve", Some(&reserve("link26", Some(link)))).await;
    assert_eq!(shared["why"], "link_share");
    assert_eq!(
        call(&h, "org:7", "reserve", Some(&reserve("admin", None))).await["kind"],
        "granted"
    );
    for i in 26..50 {
        set(&clock, base + i);
        assert_eq!(
            call(
                &h,
                "org:7",
                "reserve",
                Some(&reserve(&format!("admin-{i}"), None))
            )
            .await["kind"],
            "granted"
        );
    }
    set(&clock, base + 50);
    assert_eq!(
        call(&h, "org:7", "reserve", Some(&reserve("cap50", None))).await["why"],
        "bucket"
    );
    let config =
        json!({"request_key":"admin-confirmed","confirmed_established":true,"link_shares":[]});
    call(&h, "org:7", "configure", Some(&config)).await;
    assert_eq!(
        call(&h, "org:7", "reserve", Some(&reserve("cap500", None))).await["kind"],
        "granted"
    );
    let view: BudgetSnapshot =
        serde_json::from_value(call(&h, "org:7", "snapshot", None).await).unwrap();
    assert_eq!(view.buckets[0].cap, 500);
    assert_eq!(view.buckets[0].used_24h, 51);
    call(&h, "org:7", "release", Some(&original.body["slot_id"])).await;
    assert_eq!(
        call(&h, "org:7", "reserve", Some(&first)).await,
        original.body
    );
    assert_eq!(
        call(&h, "org:7", "snapshot", None).await["buckets"][0]["used_24h"],
        50
    );
    let closed = json!({"bucket":{"kind":"org"},"until":null});
    call(&h, "org:7", "record_limit", Some(&closed)).await;
    assert_eq!(
        call(&h, "org:7", "reserve", Some(&reserve("spammed", None))).await["why"],
        "closed"
    );
    set(&clock, base + 86450);
    let replacement = call(&h, "org:7", "reserve", Some(&first)).await;
    assert_eq!(replacement["kind"], "granted");
    assert_ne!(replacement["slot_id"], original.body["slot_id"]);
    call(&h, "org:7", "release", Some(&original.body["slot_id"])).await;
    let view: BudgetSnapshot =
        serde_json::from_value(call(&h, "org:7", "snapshot", None).await).unwrap();
    assert_eq!(view.buckets[0].used_24h, 1);
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            if storage
                .get_invite_budget(view.owner())
                .await
                .unwrap()
                .is_some_and(|s| s.revision == view.revision && s.buckets == view.buckets)
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        storage
            .list_invite_budget_buckets(view.owner())
            .await
            .unwrap(),
        view.buckets
    );
    assert!(
        storage
            .get_invite_budget(AccountInstallationRef {
                app: App::Repo,
                account_id: 7
            })
            .await
            .unwrap()
            .is_none()
    );
}
#[tokio::test]
async fn concurrent_same_operation_has_one_charge_and_conflicting_reuse_is_refused() {
    let h = Harness::start().await;
    let storage = Arc::new(
        ghinvite_storage_sqlx::SqlxStorage::in_memory()
            .await
            .unwrap(),
    );
    let clock = Arc::new(AtomicI64::new(1_800_000_000_000));
    h.restate
        .deploy(
            ghinvite_workflows::invite_budget::bind_pre_activation(
                restate_sdk::endpoint::Endpoint::builder(),
                storage,
                ghinvite_workflows::invite_budget::InviteBudget { clock: Some(clock) },
            )
            .build(),
        )
        .await;
    let input = reserve("same-op", None);
    let (a, b) = tokio::join!(
        call(&h, "org:9", "reserve", Some(&input)),
        call(&h, "org:9", "reserve", Some(&input))
    );
    assert_eq!(a, b);
    assert_eq!(a["kind"], "granted");
    assert_eq!(
        call(&h, "org:9", "snapshot", None).await["buckets"][0]["used_24h"],
        1
    );
    let changed = reserve("same-op", Some(InvitationLinkId::new()));
    let conflict = h
        .restate
        .invoke(
            &Call::object("InviteBudget", "org:9", "reserve"),
            Some(&changed),
            None,
        )
        .await;
    assert_eq!(conflict.status, 409);
    assert_eq!(
        call(&h, "org:9", "snapshot", None).await["buckets"][0]["used_24h"],
        1
    );
    call(&h, "org:9", "release", Some(&a["slot_id"])).await;
    call(&h, "org:9", "release", Some(&a["slot_id"])).await;
    call(&h, "org:9", "release", Some(&json!(900))).await;
    assert_eq!(
        call(&h, "org:9", "snapshot", None).await["content_used_hour"],
        0
    );
}

struct LostProjectionAck {
    inner: Arc<ghinvite_storage_sqlx::SqlxStorage>,
    attempts: std::sync::atomic::AtomicUsize,
}
#[async_trait::async_trait]
impl InviteBudgetStorage for LostProjectionAck {
    async fn share_limited_links(
        &self,
        owner: AccountInstallationRef,
        link_id: Option<ghinvite_core::InvitationLinkId>,
    ) -> ghinvite_core::storage::Result<Vec<ghinvite_core::storage::invite_budget::ShareLimitedLink>>
    {
        self.inner.share_limited_links(owner, link_id).await
    }
    async fn project_invite_budget(
        &self,
        snapshot: &BudgetSnapshot,
    ) -> ghinvite_core::storage::Result<()> {
        self.inner.project_invite_budget(snapshot).await?;
        if self.attempts.fetch_add(1, Ordering::SeqCst) == 0 {
            return Err(ghinvite_core::storage::Error::Database(
                "injected lost budget projection acknowledgement".into(),
            ));
        }
        Ok(())
    }
    async fn get_invite_budget(
        &self,
        owner: AccountInstallationRef,
    ) -> ghinvite_core::storage::Result<Option<BudgetSnapshot>> {
        self.inner.get_invite_budget(owner).await
    }
    async fn list_invite_budget_buckets(
        &self,
        owner: AccountInstallationRef,
    ) -> ghinvite_core::storage::Result<Vec<ghinvite_core::budget::BucketSnapshot>> {
        self.inner.list_invite_budget_buckets(owner).await
    }
}
#[tokio::test]
async fn lost_projection_ack_retries_the_same_snapshot_without_charging_twice() {
    let h = Harness::start().await;
    let storage = Arc::new(LostProjectionAck {
        inner: Arc::new(
            ghinvite_storage_sqlx::SqlxStorage::in_memory()
                .await
                .unwrap(),
        ),
        attempts: std::sync::atomic::AtomicUsize::new(0),
    });
    let clock = Arc::new(AtomicI64::new(1_800_000_000_000));
    h.restate
        .deploy(
            ghinvite_workflows::invite_budget::bind_pre_activation(
                restate_sdk::endpoint::Endpoint::builder(),
                storage.clone(),
                ghinvite_workflows::invite_budget::InviteBudget { clock: Some(clock) },
            )
            .build(),
        )
        .await;
    let result = call(&h, "org:55", "reserve", Some(&reserve("one-write", None))).await;
    assert_eq!(result["kind"], "granted");
    tokio::time::timeout(std::time::Duration::from_secs(15), async {
        loop {
            if storage.attempts.load(Ordering::SeqCst) >= 2 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    let owner = AccountInstallationRef {
        app: App::Org,
        account_id: 55,
    };
    assert_eq!(
        storage
            .get_invite_budget(owner)
            .await
            .unwrap()
            .unwrap()
            .buckets[0]
            .used_24h,
        1
    );
    assert_eq!(
        storage.list_invite_budget_buckets(owner).await.unwrap()[0].used_24h,
        1
    );
    assert_eq!(
        call(&h, "org:55", "snapshot", None).await["buckets"][0]["used_24h"],
        1
    );
}

#[tokio::test]
async fn put204_refund_retains_content_and_replay_identity_through_real_projection() {
    let h = Harness::start().await;
    let storage = Arc::new(
        ghinvite_storage_sqlx::SqlxStorage::in_memory()
            .await
            .unwrap(),
    );
    let base = 1_800_000_000;
    let clock = Arc::new(AtomicI64::new(base * 1000));
    h.restate
        .deploy(
            ghinvite_workflows::invite_budget::bind_pre_activation(
                restate_sdk::endpoint::Endpoint::builder(),
                storage.clone(),
                ghinvite_workflows::invite_budget::InviteBudget {
                    clock: Some(clock.clone()),
                },
            )
            .build(),
        )
        .await;
    JournalAudit::retain(&h, &["InviteBudget", "InviteBudgetProjection"]).await;
    let original =
        json!({"request_key":"put204", "kind":{"kind":"repo_invite","repo_id":10}, "link_id":null});
    let granted = call(&h, "repo:77", "reserve", Some(&original)).await;
    let refund = json!({"slot_id":granted["slot_id"], "reservation":original});
    let path = Call::object("InviteBudget", "repo:77", "refund_invitation_after_put204");
    for field in ["slot", "link", "target", "kind"] {
        let mut wrong = refund.clone();
        match field {
            "slot" => wrong["slot_id"] = json!(999),
            "link" => wrong["reservation"]["link_id"] = json!(InvitationLinkId::new()),
            "target" => wrong["reservation"]["kind"]["repo_id"] = json!(11),
            _ => wrong["reservation"]["kind"] = json!({"kind":"other_write"}),
        }
        assert_eq!(
            h.restate.invoke(&path, Some(&wrong), None).await.status,
            409
        );
    }
    let first = h
        .restate
        .invoke(&path, Some(&refund), Some("refund-http-replay"))
        .await;
    assert_eq!(first.status, 200);
    assert_eq!(first.body, "refunded");
    let replay = h
        .restate
        .invoke(&path, Some(&refund), Some("refund-http-replay"))
        .await;
    assert_eq!(first.invocation_id(), replay.invocation_id());
    assert_eq!(first.body, replay.body);
    assert!(
        !h.restate
            .admin()
            .journal(first.invocation_id())
            .await
            .is_empty()
    );
    assert_eq!(
        call(
            &h,
            "repo:77",
            "refund_invitation_after_put204",
            Some(&refund)
        )
        .await,
        "refunded"
    );
    call(&h, "repo:77", "release", Some(&granted["slot_id"])).await;
    assert_eq!(
        call(&h, "repo:77", "reserve", Some(&original)).await,
        granted
    );
    let mut spacing = original.clone();
    spacing["request_key"] = json!("spacing");
    assert_eq!(
        call(&h, "repo:77", "reserve", Some(&spacing)).await["why"],
        "spacing"
    );
    assert_eq!(
        call(
            &h,
            "repo:77",
            "refund_invitation_after_put204",
            Some(&json!({"slot_id":granted["slot_id"], "reservation":spacing}))
        )
        .await,
        "not_applicable"
    );
    let view: BudgetSnapshot =
        serde_json::from_value(call(&h, "repo:77", "snapshot", None).await).unwrap();
    assert_eq!(view.content_used_hour, 1);
    assert_eq!(view.content_used_minute, 1);
    assert_eq!(view.next_content_write_at.unwrap().timestamp(), base + 1);
    assert!(view.buckets.iter().all(|b| b.used_24h == 0));
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            if storage
                .get_invite_budget(view.owner())
                .await
                .unwrap()
                .as_ref()
                .is_some_and(|v| v.revision == view.revision)
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        storage.get_invite_budget(view.owner()).await.unwrap(),
        Some(view)
    );
    set(&clock, base + 86400);
    assert_eq!(
        call(
            &h,
            "repo:77",
            "refund_invitation_after_put204",
            Some(&refund)
        )
        .await,
        "not_applicable"
    );
    let fresh = call(&h, "repo:77", "reserve", Some(&original)).await;
    assert_ne!(fresh["slot_id"], granted["slot_id"]);
    assert_eq!(
        h.restate.invoke(&path, Some(&refund), None).await.status,
        409
    );
    call(&h, "repo:77", "release", Some(&granted["slot_id"])).await;
    assert_eq!(
        call(&h, "repo:77", "snapshot", None).await["content_used_hour"],
        1
    );
}

#[tokio::test]
async fn immutable_link_share_keeps_other_links_cap_and_admin_capacity() {
    let h = Harness::start().await;
    let storage = Arc::new(
        ghinvite_storage_sqlx::SqlxStorage::in_memory()
            .await
            .unwrap(),
    );
    let base = 1_800_000_000;
    let clock = Arc::new(AtomicI64::new(base * 1000));
    h.restate
        .deploy(
            ghinvite_workflows::invite_budget::bind_pre_activation(
                restate_sdk::endpoint::Endpoint::builder(),
                storage,
                ghinvite_workflows::invite_budget::InviteBudget {
                    clock: Some(clock.clone()),
                },
            )
            .build(),
        )
        .await;
    let owner = "org:700";
    call(
        &h,
        owner,
        "configure",
        Some(&json!({"request_key":"initial-cap","confirmed_established":true,"link_shares":[]})),
    )
    .await;
    let a = InvitationLinkId::new();
    let b = InvitationLinkId::new();
    for (link, percent) in [(a, 1), (b, 100), (a, 1)] {
        call(
            &h,
            owner,
            "set_link_share",
            Some(&json!({"link_id":link,"percent":percent})),
        )
        .await;
    }
    let changed = h
        .restate
        .invoke(
            &Call::object("InviteBudget", owner, "set_link_share"),
            Some(&json!({"link_id":a,"percent":2})),
            None,
        )
        .await;
    assert_eq!(changed.status, 409);
    for percent in [0, 101] {
        let invalid = h
            .restate
            .invoke(
                &Call::object("InviteBudget", owner, "set_link_share"),
                Some(&json!({"link_id":a,"percent":percent})),
                None,
            )
            .await;
        assert_eq!(invalid.status, 400);
    }
    let overwrite = h
        .restate
        .invoke(
            &Call::object("InviteBudget", owner, "configure"),
            Some(
                &json!({"request_key":"overwrite","confirmed_established":false,"link_shares":[]}),
            ),
            None,
        )
        .await;
    assert_eq!(overwrite.status, 409);
    for i in 0..5 {
        set(&clock, base + i);
        assert_eq!(
            call(
                &h,
                owner,
                "reserve",
                Some(&reserve(&format!("a-{i}"), Some(a)))
            )
            .await["kind"],
            "granted"
        );
    }
    set(&clock, base + 5);
    assert_eq!(
        call(&h, owner, "reserve", Some(&reserve("a-over", Some(a)))).await["why"],
        "link_share"
    );
    assert_eq!(
        call(&h, owner, "reserve", Some(&reserve("b", Some(b)))).await["kind"],
        "granted"
    );
    set(&clock, base + 6);
    assert_eq!(
        call(&h, owner, "reserve", Some(&reserve("admin", None))).await["kind"],
        "granted"
    );
    let snapshot = call(&h, owner, "snapshot", None).await;
    assert_eq!(snapshot["confirmed_established"], true);
    let bucket = snapshot["buckets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["bucket"]["kind"] == "org")
        .unwrap();
    assert_eq!(bucket["cap"], 500);
    assert_eq!(bucket["used_24h"], 7);
    // Minimum-one rounding for a repository cap50 at1%; independent owner.
    call(
        &h,
        "repo:700",
        "set_link_share",
        Some(&json!({"link_id":a,"percent":1})),
    )
    .await;
    for (i, expected) in [(0, "granted"), (1, "retry_at")] {
        set(&clock, base + 7 + i);
        let result=call(&h,"repo:700","reserve",Some(&json!({"request_key":format!("repo-{i}"),"kind":{"kind":"repo_invite","repo_id":10},"link_id":a}))).await;
        assert_eq!(result["kind"], expected);
        if i == 1 {
            assert_eq!(result["why"], "link_share");
        }
    }
}
