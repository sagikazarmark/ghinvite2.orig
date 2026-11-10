use super::*;
use ghinvite_core::access_grant::*;
async fn accepted(
    f: &Fixture,
) -> (
    OrganizationCreateCommand,
    OrganizationDeliverySnapshot,
    Vec<AccessGrantCommand>,
) {
    let c = f.command(&[5]).await;
    f.create(&c).await;
    {
        let mut gh = f.github.0.lock().unwrap();
        gh.pending = false;
        gh.org = Some(("member", "active"));
        gh.teams.insert(5, ("member", "active"));
    }
    standard_call(
        f,
        "OrganizationDelivery",
        &c.delivery_key(),
        "observe_listed",
        Some(observed(&c, Utc::now(), false)),
    )
    .await;
    let snapshot = terminal(f, &c).await;
    assert_eq!(
        snapshot.settlement.as_ref().unwrap().state,
        ghinvite_core::InvitationState::Accepted
    );
    let grants = accepted_organization(&snapshot).unwrap();
    assert_eq!(grants.len(), 2);
    for grant in &grants {
        standard_call(
            f,
            "AccessGrant",
            &grant.key(),
            "schedule",
            Some(json!(grant)),
        )
        .await;
    }
    (c, snapshot, grants)
}
async fn remove(f: &Fixture, grant: &AccessGrantCommand) -> AccessGrantSnapshot {
    standard_call(
        f,
        "AccessGrant",
        &grant.key(),
        "remove_now",
        Some(json!(RemovalCommand {
            grant: grant.clone(),
            reason: RemovalReason::LinkRevoked
        })),
    )
    .await;
    serde_json::from_value(
        standard_call(
            f,
            "AccessGrant",
            &grant.key(),
            "status",
            Some(json!(RemovalBarrierQuery {
                request_id: grant.request_id,
                target: grant.target.clone(),
                delivery_generation: grant.delivery_generation
            })),
        )
        .await,
    )
    .unwrap()
}
#[tokio::test]
async fn owned_accepted_org_and_attached_team_are_removed_independently_with_immutable_receipts() {
    let f = Fixture::access(Default::default()).await;
    let (c, original, grants) = accepted(&f).await;
    for grant in grants.iter().rev() {
        tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
        assert!(matches!(
            remove(&f, grant).await.outcome,
            RemovalOutcome::Removed { .. }
        ));
        assert!(matches!(
            remove(&f, grant).await.outcome,
            RemovalOutcome::Removed { .. }
        ));
    }
    assert_eq!(f.writes("DELETE").len(), 2);
    assert_eq!(
        standard_call(&f, "InviteBudget", "org:100", "snapshot", None).await["content_used_hour"],
        3
    );
    assert_eq!(
        serde_json::from_value::<OrganizationDeliverySnapshot>(
            standard_call(
                &f,
                "OrganizationDelivery",
                &c.delivery_key(),
                "status",
                None
            )
            .await
        )
        .unwrap(),
        original
    );
}
#[tokio::test]
async fn promoted_org_owner_and_team_maintainer_are_skipped_without_delete_or_charge() {
    let f = Fixture::access(Default::default()).await;
    let (c, original, grants) = accepted(&f).await;
    {
        let mut gh = f.github.0.lock().unwrap();
        gh.org = Some(("admin", "active"));
        gh.teams.insert(5, ("maintainer", "active"));
    }
    for grant in &grants {
        assert!(matches!(
            remove(&f, grant).await.outcome,
            RemovalOutcome::RemovalSkipped {
                reason: RemovalSkip::RoleChanged { .. },
                ..
            }
        ));
    }
    assert!(f.writes("DELETE").is_empty());
    assert_eq!(
        standard_call(&f, "InviteBudget", "org:100", "snapshot", None).await["content_used_hour"],
        1
    );
    assert_eq!(
        serde_json::from_value::<OrganizationDeliverySnapshot>(
            standard_call(
                &f,
                "OrganizationDelivery",
                &c.delivery_key(),
                "status",
                None
            )
            .await
        )
        .unwrap(),
        original
    );
}
#[tokio::test]
async fn lost_team_removal_ack_stays_unknown_and_never_repeats_on_absence() {
    let f = Fixture::access(Scenario {
        deletes: VecDeque::from([502]),
        ..Default::default()
    })
    .await;
    let (_, _, grants) = accepted(&f).await;
    let grant = grants
        .iter()
        .find(|g| matches!(g.target, GrantTarget::Team { .. }))
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    for _ in 0..3 {
        assert_eq!(
            remove(&f, grant).await.outcome,
            RemovalOutcome::OutcomeUnknown
        );
    }
    assert_eq!(f.writes("DELETE").len(), 1);
    assert_eq!(
        standard_call(&f, "InviteBudget", "org:100", "snapshot", None).await["content_used_hour"],
        2
    );
}

async fn standard_created(f: &Fixture) -> (InvitationLinkId, OrganizationDeliverySnapshot) {
    let link = InvitationLinkId::new();
    assert!(standard_call(f,"InvitationLink",&link.to_string(),"create",Some(json!({
        "secret_sha256":"2a".repeat(32),"link_id":link,"account_id":100,"installation_id":2,
        "admin":{"account_id":100,"user_id":7},"description":"Removal capture","max_uses":10,
        "approval_required":false,"targets":{"organization":{"org_id":100,"org_login":"acme","grant":"member"},
        "teams":[{"team_id":5,"team_slug":"team-5","grant":"member"}],"repos":[]}
    }))).await.get("Ok").is_some());
    let admission=standard_call(f,"InvitationLink",&link.to_string(),"admit",Some(json!({
        "scope_evidence":[],"secret_sha256":"2a".repeat(32),"secret_generation":1,"link_id":link,
        "operation_id":RequestId::new(),"requester_id":8,"justification":null
    }))).await;
    let request = admission["result"]["request_id"].as_str().unwrap();
    let key = format!("{request}:100:1");
    let snapshot = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let value = standard_call(f, "OrganizationDelivery", &key, "status", None).await;
            if value["create"]["outcome"]["kind"] == "created" {
                break serde_json::from_value(value).unwrap();
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    (link, snapshot)
}
async fn accept_standard(f: &Fixture, snapshot: &OrganizationDeliverySnapshot) {
    {
        let mut gh = f.github.0.lock().unwrap();
        gh.pending = false;
        gh.org = Some(("member", "active"));
        gh.teams.insert(5, ("member", "active"));
    }
    standard_call(
        f,
        "OrganizationDelivery",
        &snapshot.create.command.delivery_key(),
        "observe_listed",
        Some(observed(&snapshot.create.command, Utc::now(), false)),
    )
    .await;
    assert_eq!(
        terminal(f, &snapshot.create.command)
            .await
            .settlement
            .unwrap()
            .state,
        ghinvite_core::InvitationState::Accepted
    );
}
#[tokio::test]
async fn revoke_uses_retained_preview_and_role_change_skips_without_changing_acceptance() {
    let f = Fixture::start_all(Default::default(), true, false, 604800, true).await;
    ghinvite_e2e::journal::JournalAudit::retain(&f.h, &["InvitationLink"]).await;
    let (link, created) = standard_created(&f).await;
    accept_standard(&f, &created).await;
    let accepted = terminal(&f, &created.create.command).await;
    let operation = RequestId::new();
    let prepare =
        json!({"link_id":link,"admin":{"account_id":100,"user_id":7},"operation_id":operation});
    let reply =
        f.h.restate
            .invoke(
                &Call::object(
                    "InvitationLink",
                    &link.to_string(),
                    "prepare_access_removal",
                ),
                Some(&prepare),
                None,
            )
            .await;
    assert_eq!(reply.status, 200, "{reply:?}");
    let journal = f.h.restate.admin().journal(reply.invocation_id()).await;
    assert_eq!(
        journal
            .iter()
            .filter(|entry| entry.is_run())
            .map(|entry| entry.name.as_deref().unwrap())
            .collect::<Vec<_>>(),
        vec!["access_preview_time"]
    );
    let preview = reply.body;
    assert_eq!(preview["grants"].as_array().unwrap().len(), 2);
    assert!(f.writes("DELETE").is_empty());
    {
        f.github.0.lock().unwrap().org = Some(("admin", "active"));
    }
    assert_eq!(
        standard_call(
            &f,
            "InvitationLink",
            &link.to_string(),
            "prepare_access_removal",
            Some(prepare)
        )
        .await,
        preview
    );
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    let revoke = json!({"link_id":link,"admin":{"account_id":100,"user_id":7},"operation_id":operation,
        "cascade":{"decline_pending_requests":false,"cancel_pending_invitations":false,"remove_granted_access":true}});
    let receipt = standard_call(
        &f,
        "InvitationLink",
        &link.to_string(),
        "revoke",
        Some(revoke.clone()),
    )
    .await;
    assert_eq!(receipt["Ok"]["addressed_access_grants"], 2);
    let progress = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let value = standard_call(
                &f,
                "InvitationLink",
                &link.to_string(),
                "cascade_progress",
                Some(json!({"link_id":link,"admin":{"account_id":100,"user_id":7}})),
            )
            .await;
            if value["done_access_grants"] == 2 {
                break value;
            }
            tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(progress["addressed_access_grants"], 2);
    assert_eq!(f.writes("DELETE").len(), 1);
    assert_eq!(
        standard_call(
            &f,
            "InvitationLink",
            &link.to_string(),
            "revoke",
            Some(revoke.clone())
        )
        .await,
        receipt
    );
    let mut changed = revoke;
    changed["cascade"]["remove_granted_access"] = json!(false);
    assert_eq!(
        f.h.restate
            .invoke(
                &Call::object("InvitationLink", &link.to_string(), "revoke"),
                Some(&changed),
                None
            )
            .await
            .status,
        409
    );
    assert_eq!(terminal(&f, &created.create.command).await, accepted);
}
#[tokio::test]
async fn acceptance_after_preview_is_excluded_from_exact_removal_capture() {
    let f = Fixture::start_all(Default::default(), true, false, 604800, true).await;
    let (link, created) = standard_created(&f).await;
    let operation = RequestId::new();
    let missing_preview = json!({"link_id":link,"admin":{"account_id":100,"user_id":7},"operation_id":operation,
        "cascade":{"decline_pending_requests":false,"cancel_pending_invitations":false,"remove_granted_access":true}});
    let refused =
        f.h.restate
            .invoke(
                &Call::object("InvitationLink", &link.to_string(), "revoke"),
                Some(&missing_preview),
                None,
            )
            .await;
    assert_eq!(refused.status, 503);
    let before = standard_call(
        &f,
        "InvitationLink",
        &link.to_string(),
        "link_status",
        Some(json!({"link_id":link,"admin":{"account_id":100,"user_id":7}})),
    )
    .await;
    assert!(before["revoked_at"].is_null());
    assert!(f.writes("DELETE").is_empty());
    let prepare =
        json!({"link_id":link,"admin":{"account_id":100,"user_id":7},"operation_id":operation});
    let preview = standard_call(
        &f,
        "InvitationLink",
        &link.to_string(),
        "prepare_access_removal",
        Some(prepare.clone()),
    )
    .await;
    assert!(preview["grants"].as_array().unwrap().is_empty());
    accept_standard(&f, &created).await;
    assert_eq!(
        standard_call(
            &f,
            "InvitationLink",
            &link.to_string(),
            "prepare_access_removal",
            Some(prepare)
        )
        .await,
        preview
    );
    let receipt=standard_call(&f,"InvitationLink",&link.to_string(),"revoke",Some(json!({
        "link_id":link,"admin":{"account_id":100,"user_id":7},"operation_id":operation,
        "cascade":{"decline_pending_requests":false,"cancel_pending_invitations":false,"remove_granted_access":true}
    }))).await;
    assert!(receipt.get("Ok").is_some());
    assert_eq!(
        receipt["Ok"]["addressed_access_grants"]
            .as_u64()
            .unwrap_or(0),
        0
    );
    assert!(f.writes("DELETE").is_empty());
}

#[tokio::test]
async fn reissue_stop_after_preview_prevents_old_capture_from_removing_replacement_access() {
    let f = Fixture::start_all(Default::default(), true, false, 604800, true).await;
    let (link, created) = standard_created(&f).await;
    accept_standard(&f, &created).await;
    let input = json!({"link_id":link,"admin":{"account_id":100,"user_id":7},"operation_id":RequestId::new()});
    let preview: ghinvite_core::revocation::AccessRemovalPreview = serde_json::from_value(
        standard_call(
            &f,
            "InvitationLink",
            &link.to_string(),
            "prepare_access_removal",
            Some(input.clone()),
        )
        .await,
    )
    .unwrap();
    assert_eq!(preview.grants.len(), 2);
    for grant in &preview.grants {
        assert_eq!(
            standard_call(
                &f,
                "AccessGrant",
                &grant.key(),
                "stop_for_reissue",
                Some(json!(RemovalBarrierQuery {
                    request_id: grant.request_id,
                    target: grant.target.clone(),
                    delivery_generation: grant.delivery_generation
                }))
            )
            .await,
            "definitively_finished"
        );
    }
    let mut revoke = input.clone();
    revoke["cascade"] = json!({"decline_pending_requests":false,"cancel_pending_invitations":false,"remove_granted_access":true});
    assert_eq!(
        standard_call(
            &f,
            "InvitationLink",
            &link.to_string(),
            "revoke",
            Some(revoke)
        )
        .await["Ok"]["addressed_access_grants"],
        2
    );
    let progress = standard_call(
        &f,
        "InvitationLink",
        &link.to_string(),
        "cascade_progress",
        Some(json!({"link_id":link,"admin":input["admin"]})),
    )
    .await;
    assert_eq!(progress["done_access_grants"], 2);
    assert!(f.writes("DELETE").is_empty());
    for grant in preview.grants {
        assert!(matches!(
            remove(&f, &grant).await.outcome,
            RemovalOutcome::RemovalSkipped {
                reason: RemovalSkip::SupersededGeneration,
                ..
            }
        ));
    }
    assert!(f.writes("DELETE").is_empty());
}
