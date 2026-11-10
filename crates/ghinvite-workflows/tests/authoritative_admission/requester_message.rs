use super::*;
use ghinvite_e2e::{Call, journal::JournalAudit};

async fn requester(r: &Runtime, id: &str, user: u64, status: u16, runs: &[&str]) -> Value {
    let reply = r
        .harness
        .restate
        .invoke(
            &Call::object("InvitationRequest", id, "requester_status"),
            Some(&json!({"request_id":id,"requester_id":user})),
            None,
        )
        .await;
    assert_eq!(reply.status, status, "{reply:?}");
    let journal = r
        .harness
        .restate
        .admin()
        .journal(reply.invocation_id())
        .await;
    assert!(!journal.is_empty());
    assert_eq!(
        journal
            .iter()
            .filter(|e| e.is_run())
            .map(|e| e.name.as_deref().expect("named run"))
            .collect::<Vec<_>>(),
        runs
    );
    reply.body
}

#[tokio::test]
async fn explicit_decline_message_preserves_private_history_and_exact_owner_receipts() {
    let r = Runtime::start().await;
    JournalAudit::retain(&r.harness, &["InvitationRequest"]).await;
    let link = r.create(None).await;
    let accepted = r
        .ok("InvitationLink", link, "admit", &attempt(link, 8))
        .await;
    let rid = accepted["result"]["request_id"].as_str().unwrap();
    assert_eq!(
        requester(&r, rid, 8, 200, &["evaluation_time"]).await["request"]["state"],
        "pending"
    );
    requester(&r, rid, 9, 404, &[]).await;
    requester(&r, &RequestId::new().to_string(), 8, 404, &[]).await;
    let legacy = decision(
        link,
        &accepted,
        json!({"kind":"decline","reason":"PRIVATE_LEGACY_REASON"}),
    );
    let original = r.ok("InvitationRequest", rid, "decide", &legacy).await;
    assert!(
        original["request"]["decision"]
            .get("requester_message")
            .is_none()
    );
    let safe = requester(&r, rid, 8, 200, &["evaluation_time"]).await;
    assert_eq!(safe["request"]["state"], "declined");
    assert!(!safe.to_string().contains("PRIVATE_LEGACY_REASON"));
    assert!(
        safe["request"]["decision"]
            .get("requester_message")
            .is_none()
    );
    assert_eq!(
        r.ok("InvitationRequest", rid, "decide", &legacy).await,
        original
    );

    let link = r.create(None).await;
    let accepted = r
        .ok("InvitationLink", link, "admit", &attempt(link, 8))
        .await;
    let rid = accepted["result"]["request_id"].as_str().unwrap();
    let explicit = decision(
        link,
        &accepted,
        json!({"kind":"decline","reason":"PRIVATE_NEW_REASON","requester_message":"  Please contact\r\nthe team.  "}),
    );
    let mut invalid = explicit.clone();
    invalid["action"]["requester_message"] = json!("x".repeat(501));
    assert_eq!(
        r.call("InvitationRequest", rid, "decide", &invalid)
            .send()
            .await
            .unwrap()
            .status(),
        400
    );
    let mut denied = explicit.clone();
    denied["operation_id"] = json!(RequestId::new());
    denied["admin"]["user_id"] = json!(9);
    assert_eq!(
        r.ok("InvitationRequest", rid, "decide", &denied).await["outcome"],
        "not_authorized"
    );
    let applied = r.ok("InvitationRequest", rid, "decide", &explicit).await;
    assert_eq!(
        applied["request"]["decision"]["requester_message"],
        "Please contact\nthe team."
    );
    assert_eq!(
        r.ok("InvitationRequest", rid, "decide", &explicit).await,
        applied
    );
    let mut changed = explicit.clone();
    changed["action"]["requester_message"] = json!("different");
    assert_eq!(
        r.call("InvitationRequest", rid, "decide", &changed)
            .send()
            .await
            .unwrap()
            .status(),
        409
    );
    changed["operation_id"] = json!(RequestId::new());
    assert_eq!(
        r.ok("InvitationRequest", rid, "decide", &changed).await["outcome"],
        "incompatible"
    );
    let safe = requester(&r, rid, 8, 200, &["evaluation_time"]).await;
    assert_eq!(
        safe["request"]["decision"]["requester_message"],
        "Please contact\nthe team."
    );
    assert!(!safe.to_string().contains("PRIVATE_NEW_REASON"));
    requester(&r, rid, 9, 404, &[]).await;
    let joined=r.ok("InvitationLink",link,"requester_page",&json!({"link_id":link,"secret_sha256":"2a".repeat(32),"secret_generation":1,"requester_id":8})).await;
    assert_eq!(
        joined["request"]["decision"]["requester_message"],
        "Please contact\nthe team."
    );
    assert!(!joined.to_string().contains("PRIVATE_NEW_REASON"));
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(row) = r
                .storage
                .get_invitation_request(rid.parse().unwrap())
                .await
                .unwrap()
                && row.requester_message.as_deref() == Some("Please contact\nthe team.")
            {
                assert_eq!(row.decline_reason.as_deref(), Some("PRIVATE_NEW_REASON"));
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();

    let link = r.create(None).await;
    let accepted = r
        .ok("InvitationLink", link, "admit", &attempt(link, 8))
        .await;
    let rid = accepted["result"]["request_id"].as_str().unwrap();
    *r.request_faults.clock.lock().unwrap() = Some(chrono::Utc::now() + chrono::Duration::days(8));
    assert_eq!(
        requester(&r, rid, 8, 200, &["evaluation_time"]).await["request"]["state"],
        "expired"
    );
}
