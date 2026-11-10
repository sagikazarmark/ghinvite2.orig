use super::*;

#[tokio::test]
async fn requester_two_factor_notice_uses_bound_facts_without_gating_admission() {
    let f = Fixture::start(
        Scenario {
            two_factor_required: Some(true),
            ..Default::default()
        },
        true,
    )
    .await;
    let link = InvitationLinkId::new();
    let key = link.to_string();
    let create = json!({"secret_sha256":"2a".repeat(32),"link_id":link,"account_id":100,"installation_id":2,"admin":{"account_id":100,"user_id":7},"description":"Two-factor advisory","max_uses":10,"approval_required":true,"targets":{"organization":{"org_id":100,"org_login":"acme","grant":"member"},"teams":[],"repos":[]}});
    assert!(
        standard_call(&f, "InvitationLink", &key, "create", Some(create))
            .await
            .get("Ok")
            .is_some()
    );
    let query = json!({"link_id":link,"secret_sha256":"2a".repeat(32),"secret_generation":1,"requester_id":8});
    let page = standard_call(
        &f,
        "InvitationLink",
        &key,
        "requester_page",
        Some(query.clone()),
    )
    .await;
    assert_eq!(page["two_factor_required"], true);
    assert_eq!(page["can_start_fresh"], true);
    for private in [
        "seats",
        "base_permission",
        "installation_id",
        "internal_note",
    ] {
        assert!(
            page.get(private).is_none(),
            "notice must not disclose {private}"
        );
    }
    let reads = f.github.0.lock().unwrap().org_reads;
    let repeated = standard_call(
        &f,
        "InvitationLink",
        &key,
        "requester_page",
        Some(query.clone()),
    )
    .await;
    assert_eq!(repeated["two_factor_required"], true);
    assert_eq!(
        f.github.0.lock().unwrap().org_reads,
        reads,
        "page reuses the bounded facts cache"
    );

    let mut invalid = query.clone();
    invalid["secret_sha256"] = json!("ff".repeat(32));
    let trace = f.github.0.lock().unwrap().trace.len();
    let denied =
        f.h.restate
            .invoke(
                &Call::object("InvitationLink", &key, "requester_page"),
                Some(&invalid),
                None,
            )
            .await;
    assert_eq!(denied.status, 404);
    assert_eq!(f.github.0.lock().unwrap().trace.len(), trace);

    let input = json!({"scope_evidence":[],"secret_sha256":"2a".repeat(32),"secret_generation":1,"link_id":link,"operation_id":RequestId::new(),"requester_id":8,"justification":null});
    let admitted = standard_call(&f, "InvitationLink", &key, "admit", Some(input.clone())).await;
    assert_eq!(
        admitted["result"]["kind"], "accepted",
        "2FA requirement is not an admission blocker"
    );
    assert_eq!(
        standard_call(&f, "InvitationLink", &key, "admit", Some(input)).await,
        admitted
    );

    for requirement in [Some(false), None] {
        f.github.0.lock().unwrap().two_factor_required = requirement;
        standard_call(
            &f,
            "OrganizationEligibility",
            "org:100",
            "refresh",
            Some(json!({"account_id":100,"installation_id":2})),
        )
        .await;
        let page = standard_call(
            &f,
            "InvitationLink",
            &key,
            "requester_page",
            Some(query.clone()),
        )
        .await;
        assert_eq!(
            page.get("two_factor_required"),
            requirement.map(Value::Bool).as_ref()
        );
    }
    {
        let mut github = f.github.0.lock().unwrap();
        github.two_factor_required = Some(true);
        github.org_id_override = Some(999);
    }
    standard_call(
        &f,
        "OrganizationEligibility",
        "org:100",
        "refresh",
        Some(json!({"account_id":100,"installation_id":2})),
    )
    .await;
    let unknown = standard_call(
        &f,
        "InvitationLink",
        &key,
        "requester_page",
        Some(query.clone()),
    )
    .await;
    assert!(
        unknown.get("two_factor_required").is_none(),
        "a wrong organization cannot supply a notice"
    );

    f.github.0.lock().unwrap().org_id_override = None;
    standard_call(
        &f,
        "OrganizationEligibility",
        "org:100",
        "refresh",
        Some(json!({"account_id":100,"installation_id":2})),
    )
    .await;
    let at = Utc::now();
    f.github.0.lock().unwrap().suspended = true;
    let event = json!({"delivery_id":"cd".repeat(32),"action":"suspend","verified":{"app":"org","account":account(),"suspended_at":at,"permissions":{"members":"write","metadata":"read"}},"sender_id":7,"sender_login":"owner","updated_at":at});
    standard_call(
        &f,
        "AccountInstallation",
        "org:100",
        "onboard_webhook",
        Some(event),
    )
    .await;
    let reads = f.github.0.lock().unwrap().trace.len();
    let unknown = standard_call(&f, "InvitationLink", &key, "requester_page", Some(query)).await;
    assert!(
        unknown.get("two_factor_required").is_none(),
        "suspension suppresses old cached notice before asynchronous refresh"
    );
    assert_eq!(f.github.0.lock().unwrap().trace.len(), reads);
    for method in ["POST", "PUT", "DELETE"] {
        assert!(
            f.writes(method).is_empty(),
            "advisory reads never mutate membership"
        );
    }
}
