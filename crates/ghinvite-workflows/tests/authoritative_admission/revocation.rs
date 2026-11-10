use super::*;

#[tokio::test]
async fn revoke_cascade_freezes_receipts_counts_and_only_cancels_pending_requests() {
    let rt = Runtime::start().await;
    let id = rt.create(Some(20)).await;
    let first_input = attempt(id, 8);
    let first = rt.ok("InvitationLink", id, "admit", &first_input).await;
    let second = rt.ok("InvitationLink", id, "admit", &attempt(id, 9)).await;
    let approved_id = first["result"]["request_id"].as_str().unwrap();
    let pending_id = second["result"]["request_id"].as_str().unwrap();
    let approval_input = decision(id, &first, json!({"kind":"approve"}));
    let approval = rt
        .ok("InvitationRequest", approved_id, "decide", &approval_input)
        .await;
    let mut command = revoke_command(id);
    command["cascade"]["decline_pending_requests"] = json!(true);
    let accepted = rt.ok("InvitationLink", id, "revoke", &command).await;
    assert_eq!(
        accepted,
        json!({"Ok":{"addressed_requests":2,"addressed_deliveries":0}})
    );
    assert_eq!(
        rt.ok("InvitationLink", id, "revoke_status", &command).await,
        accepted
    );
    let admin = json!({"link_id":id,"admin":{"account_id":100,"user_id":7}});
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let status = rt
                .ok(
                    "InvitationRequest",
                    pending_id,
                    "request_status",
                    &query(id, &second, 9),
                )
                .await;
            if status["state"] == "cancelled" {
                assert_eq!(status["decision"]["cancellation_reason"], "link_revoked");
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    for _ in 0..3 {
        assert_eq!(
            rt.ok("InvitationLink", id, "revoke", &command).await,
            accepted
        );
    }
    let progress = rt
        .ok("InvitationLink", id, "cascade_progress", &admin)
        .await;
    assert_eq!(progress["addressed_requests"], 2);
    assert_eq!(progress["done_requests"], 2);
    assert_eq!(progress["addressed_deliveries"], 0);
    assert_eq!(progress["waiting_on_github"], 0);
    assert_eq!(progress["unavailable"], 0);
    rt.request_faults
        .unavailable
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let unavailable = rt
        .ok("InvitationLink", id, "cascade_progress", &admin)
        .await;
    assert_eq!(unavailable["addressed_requests"], 2);
    assert_eq!(unavailable["done_requests"], 0);
    assert_eq!(unavailable["unavailable"], 2);
    rt.request_faults
        .unavailable
        .store(false, std::sync::atomic::Ordering::SeqCst);
    assert_eq!(
        rt.ok("InvitationLink", id, "cascade_progress", &admin)
            .await,
        progress
    );
    assert_eq!(
        rt.ok("InvitationRequest", approved_id, "decide", &approval_input)
            .await,
        approval
    );
    assert_eq!(
        rt.ok("InvitationLink", id, "admit", &first_input).await,
        first
    );
    assert_eq!(rt.uses(id).await, 2);
    assert_eq!(
        rt.ok(
            "InvitationRequest",
            pending_id,
            "decide",
            &decision(id, &second, json!({"kind":"approve"}))
        )
        .await["outcome"],
        "not_pending"
    );
    let mut changed = command.clone();
    changed["cascade"]["decline_pending_requests"] = json!(false);
    assert_eq!(
        rt.call("InvitationLink", id, "revoke", &changed)
            .send()
            .await
            .unwrap()
            .status(),
        409
    );
    let other = revoke_command(id);
    assert_eq!(
        rt.ok("InvitationLink", id, "revoke_status", &other).await,
        Value::Null,
        "current revoked state does not imply another operation succeeded"
    );
    assert_eq!(
        rt.ok("InvitationLink", id, "revoke", &other).await,
        json!({"Err":"revoked"})
    );
    let untouched = rt.create(Some(2)).await;
    for flag in ["cancel_pending_invitations", "remove_granted_access"] {
        let mut unsupported = revoke_command(untouched);
        unsupported["cascade"][flag] = json!(true);
        assert_eq!(
            rt.ok("InvitationLink", untouched, "revoke", &unsupported)
                .await,
            json!({"Err":"unsupported_cascade"})
        );
        assert_eq!(
            rt.ok("InvitationLink", untouched, "revoke", &unsupported)
                .await,
            json!({"Err":"unsupported_cascade"})
        );
        assert!(
            rt.ok(
                "InvitationLink",
                untouched,
                "link_status",
                &json!({"link_id":untouched,"admin":{"account_id":100,"user_id":7}})
            )
            .await["revoked_at"]
                .is_null()
        );
    }
    rt.harness.finish().await;
}

#[tokio::test]
async fn explicit_m1_composition_revoke_reaches_created_delivery_and_fenced_delete() {
    let rt = Runtime::start_mode(true).await;
    let id = rt.create(Some(20)).await;
    let input = attempt(id, 8);
    let admitted = rt.ok("InvitationLink", id, "admit", &input).await;
    let request = admitted["result"]["request_id"].as_str().unwrap();
    let approved = rt
        .ok(
            "InvitationRequest",
            request,
            "decide",
            &decision(id, &admitted, json!({"kind":"approve"})),
        )
        .await;
    assert_eq!(approved["request"]["state"], "approved");
    let delivery = format!("{request}:10:1");
    let created = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let status = rt
                .ok("RepositoryDelivery", &delivery, "status", &Value::Null)
                .await;
            if status["create"]["outcome"]["kind"] == "created" {
                break status;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("created invitation");
    let mut revoke = revoke_command(id);
    revoke["cascade"]["cancel_pending_invitations"] = json!(true);
    let receipt = rt.ok("InvitationLink", id, "revoke", &revoke).await;
    assert_eq!(
        receipt,
        json!({"Ok":{"addressed_requests":0,"addressed_deliveries":1}})
    );
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let status = rt
                .ok("RepositoryDelivery", &delivery, "status", &Value::Null)
                .await;
            if status["settlement"]["state"] == "cancelled" {
                assert_eq!(status["create"], created["create"]);
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("fenced cancellation after full link cascade");
    for _ in 0..3 {
        assert_eq!(
            rt.ok("InvitationLink", id, "revoke", &revoke).await,
            receipt
        );
    }
    let progress = rt
        .ok(
            "InvitationLink",
            id,
            "cascade_progress",
            &json!({"link_id":id,"admin":{"account_id":100,"user_id":7}}),
        )
        .await;
    assert_eq!(progress["addressed_deliveries"], 1);
    assert_eq!(progress["done_deliveries"], 1);
    assert_eq!(progress["waiting_on_github"], 0);
    assert_eq!(progress["unavailable"], 0);
    let trace: Value = rt
        .client
        .get(format!("{}/calls", rt.harness.github_url))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    for method in ["PUT", "DELETE"] {
        assert_eq!(
            trace["requests"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|v| v["method"] == method)
                .count(),
            1,
            "{method}: {trace}"
        );
    }
    rt.harness.finish().await;
}
