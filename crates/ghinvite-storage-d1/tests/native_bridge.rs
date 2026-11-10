#![cfg(feature = "bridge-test")]
use ghinvite_core::storage::{
    DeliveryStorage, delivery_attempts,
    invite_budget::{self, InviteBudgetStorage},
};
use ghinvite_storage_d1::{
    D1Storage,
    native::{AuthorityManifest, BridgeConfig, D1Database, JsValue},
};
use serde_json::{Value, json};
#[derive(serde::Deserialize)]
struct Fixture {
    url: String,
    token: String,
    manifest: AuthorityManifest,
}
fn control() -> String {
    std::env::var("BRIDGE_FIXTURE").expect("run tests/native-d1-bridge/runtime.mjs")
}
async fn fresh() -> (D1Storage, D1Database) {
    let f: Fixture = reqwest::Client::new()
        .post(format!("{}/control/new", control()))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let config = BridgeConfig::local_for_test(&f.url, f.token, f.manifest).unwrap();
    let db = D1Database::connect(config).await.unwrap();
    (D1Storage::new(db.clone()), db)
}
async fn worker(batch: Value) -> Value {
    reqwest::Client::new()
        .post(format!("{}/control/worker", control()))
        .json(&json!({"batch":batch}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}
fn command() -> ghinvite_core::delivery::CreateCommand {
    serde_json::from_value(json!({"invitation_id":ghinvite_core::GithubInvitationId::new(),"link_id":ghinvite_core::InvitationLinkId::new(),"request_id":ghinvite_core::RequestId::new(),"approval_id":"original-approval","account_id":100,"installation_id":1,"requester_id":8,"repo_id":10,"repo_full_name":"acme/api","permission":"pull","approved_at":"2026-05-04T12:30:00Z"})).unwrap()
}
#[tokio::test]
async fn full_shared_storage_suite_over_actual_tcp_and_d1() {
    for scenario in ghinvite_core::storage::test_suite::SCENARIOS {
        let (storage, _) = fresh().await;
        assert!(ghinvite_core::storage::test_suite::run_scenario(scenario, storage).await);
        println!("PASS {scenario}");
    }
}
#[tokio::test]
async fn retained_worker_and_native_share_fences_lost_ack_and_budget_receipts() {
    let (storage, db) = fresh().await;
    let original = command();
    let encoded = delivery_attempts::encode(&original).unwrap();
    let key = original.delivery_key();
    let claim = json!([{"sql":delivery_attempts::CLAIM,"params":[key,encoded]}]);
    let first = worker(claim.clone()).await;
    assert_eq!(first[0]["results"][0]["generation"], 1);
    assert_eq!(
        storage.claim_delivery_attempt(&original).await.unwrap(),
        None
    );
    storage.reject_delivery_attempt(&original, 1).await.unwrap();
    assert_eq!(
        worker(claim.clone()).await[0]["results"][0]["generation"],
        2
    );
    storage.reject_delivery_attempt(&original, 1).await.unwrap();
    assert!(
        worker(claim.clone()).await[0]["results"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let lost = command();
    reqwest::Client::new()
        .post(format!("{}/control/drop", control()))
        .send()
        .await
        .unwrap();
    assert!(storage.claim_delivery_attempt(&lost).await.is_err());
    assert_eq!(storage.claim_delivery_attempt(&lost).await.unwrap(), None);
    assert!(storage.delivery_attempt_exists(&lost).await.unwrap());
    let now = "2026-05-04T12:30:00Z";
    let snapshot:ghinvite_core::budget::BudgetSnapshot=serde_json::from_value(json!({"app":"repo","account_id":100,"revision":2,"observed_at":now,"confirmed_established":true,"buckets":[],"content_used_minute":1,"content_used_hour":1,"next_content_write_at":null})).unwrap();
    storage.project_invite_budget(&snapshot).await.unwrap();
    let mut old = snapshot.clone();
    old.revision = 1;
    old.content_used_hour = 0;
    old.content_used_minute = 0;
    let batch = |s: &ghinvite_core::budget::BudgetSnapshot| {
        json!(
            invite_budget::APPLY
                .iter()
                .map(|sql| json!({"sql":sql,"params":[invite_budget::encode(s).unwrap()]}))
                .collect::<Vec<_>>()
        )
    };
    worker(batch(&old)).await;
    assert_eq!(
        storage.get_invite_budget(snapshot.owner()).await.unwrap(),
        Some(snapshot.clone())
    );
    // Replay of the original immutable producer receipt does not roll back current state.
    worker(batch(&snapshot)).await;
    assert_eq!(
        storage.get_invite_budget(snapshot.owner()).await.unwrap(),
        Some(snapshot)
    );
    let typed = db
        .prepare(
            "SELECT typeof(?1) AS n,typeof(CAST(?1 AS INTEGER)) AS i,typeof(?2) AS z,?1 AS value",
        )
        .bind(&[JsValue::from_f64(9007199254740991.0), JsValue::NULL])
        .unwrap()
        .first::<Value>(None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        typed,
        json!({"n":"real","i":"integer","z":"null","value":9007199254740991_u64})
    );
}
#[tokio::test]
async fn failed_native_batch_rolls_back_authority_and_no_ddl_is_available() {
    let (_storage, db) = fresh().await;
    let original = command();
    let key = original.delivery_key();
    let encoded = delivery_attempts::encode(&original).unwrap();
    let statements = vec![
        db.prepare(delivery_attempts::CLAIM)
            .bind(&[JsValue::from_str(&key), JsValue::from_str(&encoded)])
            .unwrap(),
        db.prepare("INSERT INTO projection_assertions(invariant) VALUES(0)"),
    ];
    assert!(db.batch(statements).await.is_err());
    assert!(
        db.prepare(delivery_attempts::FENCED)
            .bind(&[JsValue::from_str(&key)])
            .unwrap()
            .first::<Value>(None)
            .await
            .unwrap()
            .is_none()
    );
    let ledger =
        worker(json!([{"sql":"SELECT * FROM d1_migrations ORDER BY id","params":[]}])).await;
    for sql in [
        "DELETE FROM 'd1_migrations'",
        "INSERT INTO 'd1_migrations'(name) VALUES('unexpected')",
        "UPDATE 'sqlite_sequence' SET seq=999",
        "CREATE TABLE unexpected(x)",
        "DELETE FROM d1_migrations",
        "SELECT 1; DELETE FROM delivery_attempts",
        "PRAGMA writable_schema=1",
    ] {
        assert!(db.prepare(sql).run().await.is_err());
    }
    let after =
        worker(json!([{"sql":"SELECT * FROM d1_migrations ORDER BY id","params":[]}])).await;
    assert_eq!(ledger[0]["success"], true);
    assert_eq!(after[0]["success"], true);
    assert_eq!(after[0]["results"], ledger[0]["results"]);
}

#[test]
fn native_bridge_configuration_refuses_unsafe_or_unbound_origins() {
    let manifest = AuthorityManifest {
        protocol: 1,
        database_id: "00000000-0000-4000-8000-000000000001".into(),
        migrations: vec![ghinvite_storage_d1::native::Migration {
            name: "0001_init.sql".into(),
            sha256: "a".repeat(64),
        }],
        schema_sha256: "b".repeat(64),
    };
    for url in [
        "http://example.test/v1/d1",
        "https://user:pass@example.test/v1/d1",
        "https://example.test/v1/d1?token=secret",
        "https://example.test/other",
        "https://example.test/v1/d1#fragment",
    ] {
        assert!(BridgeConfig::new(url, "a".repeat(64), manifest.clone()).is_err());
    }
    assert!(
        BridgeConfig::new(
            "https://example.test/v1/d1",
            "short".into(),
            manifest.clone()
        )
        .is_err()
    );
    let mut invalid = manifest.clone();
    invalid.migrations.clear();
    assert!(BridgeConfig::new("https://example.test/v1/d1", "a".repeat(64), invalid).is_err());
    assert!(BridgeConfig::new("https://example.test/v1/d1", "a".repeat(64), manifest).is_ok());
}

#[tokio::test]
async fn near_cap_continuation_page_and_cas_preserve_epoch_and_expiry() {
    use ghinvite_core::storage::{
        ContinuationStorage, continuation_rotation::MAX_CIPHERTEXT_BYTES,
    };
    let (storage, _) = fresh().await;
    // D1 imposes its own 2,000,000-byte row/string cap below the generic parser cap.
    // Positive refusal must not mutate the rotation epoch.
    let above_d1 = "z".repeat(MAX_CIPHERTEXT_BYTES - 64);
    let before = storage.continuation_rotation_epoch().await.unwrap();
    assert!(
        storage
            .retain_attempt_continuation(
                "native-boundary",
                "too-large",
                "binding-too-large",
                &above_d1,
                10000,
                1
            )
            .await
            .is_err()
    );
    assert_eq!(storage.continuation_rotation_epoch().await.unwrap(), before);
    let old = "a".repeat(1_999_000);
    let new = "b".repeat(1_999_000);
    for i in 0..5 {
        storage
            .retain_attempt_continuation(
                "native-boundary",
                &i.to_string(),
                &format!("binding-{i}"),
                &old,
                10000,
                1,
            )
            .await
            .unwrap();
    }
    let rows = storage.continuation_rotation_page(None).await.unwrap();
    assert_eq!(rows.len(), 5);
    assert!(
        rows.iter()
            .all(|r| r.payload.as_deref() == Some(old.as_str())
                && r.expires_at == Some(10000)
                && r.oversized == 0)
    );
    let epoch = storage.continuation_rotation_epoch().await.unwrap();
    assert!(
        storage
            .rewrap_attempt_continuation(&rows[0], &new, epoch)
            .await
            .unwrap()
    );
    assert!(
        !storage
            .rewrap_attempt_continuation(&rows[0], &new, epoch)
            .await
            .unwrap()
    );
    assert_eq!(
        storage.continuation_rotation_epoch().await.unwrap(),
        epoch + 1
    );
    let changed = storage.continuation_rotation_page(None).await.unwrap();
    assert_eq!(changed[0].payload.as_deref(), Some(new.as_str()));
    assert_eq!(changed[0].expires_at, Some(10000));
    assert!(
        changed
            .iter()
            .skip(1)
            .all(|r| r.payload.as_deref() == Some(old.as_str()) && r.expires_at == Some(10000))
    );
}
