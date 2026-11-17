//! Actual retained handoff after later auto-hold/release controls reached the inbox.
use super::{Duration, InvitationLinkId, RequestId, Runtime, Value, attempt, decision, json};
use std::sync::atomic::Ordering;

async fn state(r: &Runtime, service: &str, object: &str, key: &str) -> Option<Value> {
    // All values below are local constants or generated ULIDs. Escape anyway so
    // the observation helper remains independent of object/key spelling.
    let quote = |value: &str| format!("'{}'", value.replace('\'', "''"));
    let rows = r
        .harness
        .restate
        .admin()
        .sql_or_panic(&format!(
            "SELECT value_utf8 FROM state WHERE service_name={} AND service_key={} AND key={}",
            quote(service),
            quote(object),
            quote(key)
        ))
        .await;
    assert!(rows.len() <= 1, "retained state key must be unique");
    rows.first()
        .map(|row| serde_json::from_str(row["value_utf8"].as_str().unwrap()).unwrap())
}

async fn await_control(r: &Runtime, request: &str, revision: u64) -> Value {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(current) =
                state(r, "InvitationRequest", request, "link_control/current").await
                && current["revision"] == revision
            {
                return current;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("one-way full control reaches the retained request inbox")
}

#[tokio::test]
async fn failed_initialization_replays_original_handoff_after_auto_hold_and_resume() {
    let r = Runtime::start().await;
    let link = InvitationLinkId::new();
    let command = json!({
        "link_id":link,"secret_sha256":"2a".repeat(32),
        "admin":{"account_id":100,"user_id":7},"account_id":100,"installation_id":1,
        "description":"Retained control handoff","approval_required":true,"max_uses":null,
        "abuse_threshold_per_hour":1,"invitation_lifetime_seconds":604800,
        "targets":{"organization":null,"teams":[],"repos":[{"repo_id":10,"repo_full_name":"acme/api","grant":"write"}]}
    });
    r.ok("InvitationLink", link, "create", &command).await;
    let original = attempt(link, 8);
    r.request_faults.unavailable.store(true, Ordering::SeqCst);
    let failed = r
        .call("InvitationLink", link, "admit", &original)
        .send()
        .await
        .unwrap();
    assert_eq!(failed.status(), 503);
    assert_eq!(r.uses(link).await, 1);

    let handoff_key = format!("handoff/{}", original["operation_id"].as_str().unwrap());
    let pending = state(&r, "InvitationLink", &link.to_string(), &handoff_key)
        .await
        .unwrap();
    let receipt = pending["operation"]["receipt"].clone();
    assert_eq!(receipt["result"]["kind"], "accepted");
    assert_eq!(pending["hold_epoch"], 0);
    let request = receipt["result"]["request_id"].as_str().unwrap();
    let initial_control =
        await_control(&r, request, pending["link"]["revision"].as_u64().unwrap()).await;
    assert_eq!(initial_control["control"]["kind"], "active");
    assert_eq!(initial_control["hold_epoch"], 0);
    let original_control_key = format!(
        "link_control/operation/{}",
        initial_control["operation_id"].as_str().unwrap()
    );
    let original_control_record = state(&r, "InvitationRequest", request, &original_control_key)
        .await
        .unwrap();
    assert_eq!(original_control_record["input"], initial_control);
    assert!(
        state(&r, "InvitationRequest", request, "input")
            .await
            .is_none(),
        "failed initialize left only the control inbox"
    );

    // A different fresh attempt reaches the threshold despite initialization
    // remaining unavailable; it must not consume another use or need a request.
    let crossing = attempt(link, 9);
    let refused = r.ok("InvitationLink", link, "admit", &crossing).await;
    assert_eq!(refused["result"]["reason"], "paused");
    assert_eq!(r.uses(link).await, 1);
    let admin = json!({"link_id":link,"admin":{"account_id":100,"user_id":7}});
    let paused = r.ok("InvitationLink", link, "link_status", &admin).await;
    let epoch = paused["pause"]["epoch"].as_u64().unwrap();
    let held = await_control(&r, request, paused["revision"].as_u64().unwrap()).await;
    assert_eq!(held["hold_epoch"], epoch);
    assert_eq!(held["control"]["kind"], "auto_pause");

    let resume = json!({"link_id":link,"admin":{"account_id":100,"user_id":7},"operation_id":RequestId::new()});
    let resumed = r.ok("InvitationLink", link, "resume", &resume).await;
    let released = await_control(&r, request, resumed["Ok"]["revision"].as_u64().unwrap()).await;
    assert_eq!(released["control"]["kind"], "active");
    assert_eq!(
        released["hold_epoch"], epoch,
        "release retains the greatest hold epoch"
    );
    assert_eq!(
        state(&r, "InvitationLink", &link.to_string(), &handoff_key)
            .await
            .unwrap(),
        pending,
        "later lifecycle changes never rewrite the pending admission handoff"
    );

    // The retry uses the handoff's original revision/epoch, not the link's
    // current epoch. The older exact control receipt must not replace current.
    r.request_faults.unavailable.store(false, Ordering::SeqCst);
    assert_eq!(
        r.ok("InvitationLink", link, "admit", &original).await,
        receipt
    );
    assert_eq!(
        r.ok("InvitationLink", link, "admit", &original).await,
        receipt
    );
    assert_eq!(
        r.ok("InvitationLink", link, "admit", &crossing).await,
        refused
    );
    assert_eq!(r.uses(link).await, 1);
    assert_eq!(
        state(&r, "InvitationRequest", request, "link_control/current")
            .await
            .unwrap(),
        released
    );
    assert_eq!(
        state(&r, "InvitationRequest", request, &original_control_key)
            .await
            .unwrap(),
        original_control_record
    );
    assert!(
        state(&r, "InvitationLink", &link.to_string(), &handoff_key)
            .await
            .is_none()
    );
    let times = state(&r, "InvitationLink", &link.to_string(), "admission_times")
        .await
        .unwrap();
    assert_eq!(
        times,
        json!([pending["initialization"]["request"]["admitted_at"]])
    );
    let initialized = state(&r, "InvitationRequest", request, "input")
        .await
        .unwrap();
    assert_eq!(initialized, pending["initialization"]);

    // An approval after recovered initialization dispatches under the latest
    // released snapshot, rather than parking forever under the older hold.
    let approved = r
        .ok(
            "InvitationRequest",
            request,
            "decide",
            &decision(link, &receipt, json!({"kind":"approve"})),
        )
        .await;
    assert_eq!(approved["request"]["state"], "approved");
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let delivery = r
                .ok(
                    "RepositoryDelivery",
                    format!("{request}:10:1"),
                    "status",
                    &Value::Null,
                )
                .await;
            if delivery["create"]["outcome"]["kind"] == "created" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("recovered request dispatches its released delivery");
    assert_eq!(r.uses(link).await, 1);
    assert_eq!(
        r.harness.github().writes(),
        1,
        "handoff and control replays dispatch exactly one GitHub write"
    );
    r.harness.finish().await;
}
