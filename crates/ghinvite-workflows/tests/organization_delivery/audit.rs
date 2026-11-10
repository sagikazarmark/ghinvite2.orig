use super::*;
use ghinvite_core::{
    audit::{AuditEvent, EventType},
    delivery_control::{HoldWindow, LinkControl},
    storage::{ConsoleStorage, audit_read::AuditPosition},
};

async fn facts(f: &Fixture, c: &OrganizationCreateCommand, kind: EventType) -> Vec<AuditEvent> {
    let page = f
        .storage
        .list_audit_events(c.account_id, Some(kind), AuditPosition::Latest)
        .await
        .unwrap();
    assert!(
        !page.has_older,
        "fixture must inspect the complete bounded result"
    );
    page.events
        .into_iter()
        .filter(|e| e.request_id.as_deref() == Some(&c.request_id.to_string()))
        .collect()
}
pub(super) async fn wait_facts(
    f: &Fixture,
    c: &OrganizationCreateCommand,
    kind: EventType,
    count: usize,
) -> Vec<AuditEvent> {
    let result = tokio::time::timeout(std::time::Duration::from_secs(20), async {
        loop {
            let rows = facts(f, c, kind).await;
            if rows.len() >= count {
                return rows;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await;
    match result {
        Ok(rows) => rows,
        Err(_) => {
            let invocations = f.h.restate.admin().sql_or_panic("SELECT target,status,last_failure,completion_failure FROM sys_invocation WHERE target_service_name='OrganizationProjection'").await;
            panic!(
                "durable audit projection must converge; facts={:?}; projector={invocations:?}",
                facts(f, c, kind).await
            );
        }
    }
}
async fn control(
    f: &Fixture,
    c: &OrganizationCreateCommand,
    revision: u64,
    epoch: u64,
    control: LinkControl,
) {
    f.call(c,"control",json!({"envelope":{"identity":{"link_id":c.link_id,"request_id":c.request_id,"kind":"organization","target_id":100,"delivery_generation":1},"operation_id":ghinvite_core::link_lifecycle::control_operation_id(c.link_id.expect("linked fixture"),revision),"revision":revision,"hold_epoch":epoch,"at":Utc::now(),"control":control},"admitted_at":c.approved_at,"invitation_lifetime_seconds":f.lifetime})).await;
}
fn hold(c: &OrganizationCreateCommand, epoch: u64) -> LinkControl {
    LinkControl::AutoPause {
        window: HoldWindow {
            epoch,
            window_start: c.approved_at - chrono::Duration::seconds(1),
            paused_at: Utc::now(),
        },
    }
}

#[tokio::test]
async fn organization_unknown_facts_survive_projection_outage_ack_loss_and_confirmation() {
    let f = Fixture::new(Scenario {
        writes: VecDeque::from([502]),
        ..Default::default()
    })
    .await;
    f.faults
        .projection_unavailable
        .store(true, Ordering::SeqCst);
    let c = f.command(&[5, 6]).await;
    let uncertain = f.create(&c).await;
    assert_eq!(
        uncertain.create.outcome,
        OrganizationCreateOutcome::OutcomeUnknown
    );
    assert!(
        uncertain
            .teams
            .values()
            .all(|t| t.create.outcome == TeamCreateOutcome::OutcomeUnknown)
    );
    for _ in 0..3 {
        assert_eq!(f.create(&c).await, uncertain);
    }
    assert!(
        facts(&f, &c, EventType::DeliveryOutcomeUnknown)
            .await
            .is_empty()
    );
    let before_recovery = Utc::now();
    {
        let mut github = f.github.0.lock().unwrap();
        github.pending = true;
        github.org = Some(("member", "pending"));
        github.attached = vec![5, 6];
        github.created_at = Some(Utc::now());
    }
    let confirmed = f.create(&c).await;
    assert!(matches!(
        confirmed.create.outcome,
        OrganizationCreateOutcome::Created {
            upstream_id: 99,
            ..
        }
    ));
    f.faults.lose_projection_ack.store(true, Ordering::SeqCst);
    f.faults
        .projection_unavailable
        .store(false, Ordering::SeqCst);
    let rows = wait_facts(&f, &c, EventType::DeliveryOutcomeUnknown, 3).await;
    assert_eq!(
        rows.len(),
        3,
        "one unknown transition per org and attached team"
    );
    assert!(rows.iter().all(|e| e.occurred_at <= before_recovery));
    assert_eq!(
        rows.iter()
            .map(|e| e.metadata["step"].clone())
            .collect::<std::collections::HashSet<_>>()
            .len(),
        3
    );
    for _ in 0..2 {
        assert_eq!(f.create(&c).await, confirmed);
    }
    assert_eq!(facts(&f, &c, EventType::DeliveryOutcomeUnknown).await, rows);
    assert_eq!(f.writes("POST").len(), 1);
    tokio::time::timeout(std::time::Duration::from_secs(20), async {
        loop {
            if f.storage
                .get_organization_delivery(&c.storage_id())
                .await
                .unwrap()
                .is_some_and(|s| s == confirmed)
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("latest receipt must survive delayed projection");
}

#[tokio::test]
async fn team_audit_hold_epochs_preserve_confirmed_steps_and_release_only_current_epoch() {
    let f = Fixture::new(Scenario {
        org: Some(("member", "active")),
        teams: BTreeMap::from([(5, ("maintainer", "active"))]),
        writes: VecDeque::from([403, 200]),
        ..Default::default()
    })
    .await;
    let c = f.command(&[5, 6]).await;
    let initial = f.create(&c).await;
    assert!(matches!(
        initial.teams[&6].create.outcome,
        TeamCreateOutcome::Blocked { .. }
    ));
    control(&f, &c, 2, 2, hold(&c, 2)).await;
    let held = f.create(&c).await;
    control(&f, &c, 3, 3, hold(&c, 3)).await;
    let renewed = f.create(&c).await;
    assert!(
        renewed.revision > held.revision,
        "new epoch is a distinct snapshot transition"
    );
    assert_eq!(
        renewed.teams[&6].create, held.teams[&6].create,
        "hold receipt stays immutable while epoch changes"
    );
    for _ in 0..2 {
        assert_eq!(f.create(&c).await, renewed);
    }
    control(&f, &c, 4, 3, LinkControl::Active).await;
    let resumed = f.create(&c).await;
    assert_eq!(
        resumed.teams[&6].create.outcome,
        TeamCreateOutcome::Upgraded
    );
    assert_eq!(resumed.create, initial.create);
    assert_eq!(resumed.teams[&5], initial.teams[&5]);
    let held_rows = wait_facts(&f, &c, EventType::DeliveryHeld, 2).await;
    let released = wait_facts(&f, &c, EventType::DeliveryReleased, 1).await;
    assert_eq!(held_rows.len(), 2);
    assert_ne!(
        held_rows[0].metadata["transition_revision"],
        held_rows[1].metadata["transition_revision"]
    );
    assert_eq!(released.len(), 1);
    assert_eq!(released[0].metadata["transition"]["hold_epoch"], 3);
    assert!(
        held_rows
            .iter()
            .chain(&released)
            .all(|e| e.target_id.ends_with("/team:6"))
    );
    assert_eq!(
        wait_facts(&f, &c, EventType::DeliveryBlocked, 1)
            .await
            .len(),
        1
    );
    assert_eq!(f.writes("PUT").len(), 2);
}

#[tokio::test]
async fn held_org_and_teams_aborted_by_revoke_do_not_emit_release() {
    let f = Fixture::new(Scenario::default()).await;
    let c = f.command(&[5, 6]).await;
    let window = HoldWindow {
        epoch: 2,
        window_start: c.approved_at - chrono::Duration::seconds(1),
        paused_at: Utc::now(),
    };
    control(
        &f,
        &c,
        2,
        2,
        LinkControl::AutoPause {
            window: window.clone(),
        },
    )
    .await;
    let held = f.create(&c).await;
    assert!(matches!(
        held.create.outcome,
        OrganizationCreateOutcome::Held { .. }
    ));
    control(
        &f,
        &c,
        3,
        2,
        LinkControl::Revoked {
            cancel_pending_invitations: false,
            decline_pending_requests: false,
            abort_hold: Some(window),
        },
    )
    .await;
    let aborted = f.create(&c).await;
    assert!(matches!(
        aborted.create.outcome,
        OrganizationCreateOutcome::Aborted { .. }
    ));
    assert_eq!(
        wait_facts(&f, &c, EventType::DeliveryHeld, 3).await.len(),
        3
    );
    assert!(facts(&f, &c, EventType::DeliveryReleased).await.is_empty());
    assert!(f.writes("POST").is_empty());
    assert!(f.writes("PUT").is_empty());
}

#[tokio::test]
async fn killed_after_atomic_receipt_keeps_original_fact_when_later_control_overwrites_outcome() {
    let f = Fixture::new(Scenario {
        writes: VecDeque::from([403]),
        ..Default::default()
    })
    .await;
    let c = f.command(&[]).await;
    f.faults.pause_after_audit.store(true, Ordering::SeqCst);
    let sent =
        f.h.restate
            .invoke(
                &Call::object("OrganizationDelivery", &c.delivery_key(), "create").send(),
                Some(&json!(c)),
                None,
            )
            .await;
    assert_eq!(sent.status, 202);
    tokio::time::timeout(std::time::Duration::from_secs(15), async {
        while f.faults.audit_interruptions.load(Ordering::SeqCst) < 2 {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let first = tokio::time::timeout(std::time::Duration::from_secs(15), async {
        loop {
            let value: OrganizationDeliverySnapshot = serde_json::from_value(
                standard_call(
                    &f,
                    "OrganizationDelivery",
                    &c.delivery_key(),
                    "status",
                    None,
                )
                .await,
            )
            .unwrap();
            if matches!(
                value.create.outcome,
                OrganizationCreateOutcome::Blocked { .. }
            ) {
                break value;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("observe durable atomic receipt before killing its invocation");
    assert!(matches!(
        first.create.outcome,
        OrganizationCreateOutcome::Blocked { .. }
    ));
    let before_kill = Utc::now();
    assert!(facts(&f, &c, EventType::DeliveryBlocked).await.is_empty());
    f.h.restate.admin().kill(sent.invocation_id()).await;
    f.h.restate
        .admin()
        .await_status(sent.invocation_id(), &["completed"])
        .await;
    f.faults.pause_after_audit.store(false, Ordering::SeqCst);
    f.faults.lose_projection_ack.store(true, Ordering::SeqCst);
    control(&f, &c, 2, 2, hold(&c, 2)).await;
    let held = f.create(&c).await;
    assert!(matches!(
        held.create.outcome,
        OrganizationCreateOutcome::Held { .. }
    ));
    let rows = wait_facts(&f, &c, EventType::DeliveryBlocked, 1).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].metadata["transition_revision"], first.revision);
    assert!(rows[0].occurred_at <= before_kill);
    assert_eq!(
        wait_facts(&f, &c, EventType::DeliveryHeld, 1).await.len(),
        1
    );
    assert_eq!(f.create(&c).await, held);
    assert_eq!(facts(&f, &c, EventType::DeliveryBlocked).await, rows);
    assert_eq!(f.writes("POST").len(), 1);
}

#[tokio::test]
async fn canonical_owned_invitation_and_attached_acceptance_have_distinct_events() {
    let f = Fixture::new(Default::default()).await;
    let c = f.command(&[5]).await;
    let created = f.create(&c).await;
    assert_eq!(
        created.create.audit_version,
        ghinvite_core::delivery_audit::DeliveryAuditVersion::CanonicalV1
    );
    let sent = wait_facts(&f, &c, EventType::OrgInvitationSent, 1).await;
    assert_eq!(sent[0].target_id, c.storage_id());
    assert_eq!(
        facts(&f, &c, EventType::InvitationSent).await.len(),
        1,
        "attached team pending fact is not a membership grant"
    );
    {
        let mut gh = f.github.0.lock().unwrap();
        gh.pending = false;
        gh.org = Some(("member", "active"));
    }
    f.call(&c, "observe_listed", observed(&c, Utc::now(), false))
        .await;
    let accepted = terminal(&f, &c).await;
    assert_eq!(accepted.create, created.create);
    assert_eq!(accepted.teams[&5].create, created.teams[&5].create);
    let org = wait_facts(&f, &c, EventType::OrgInvitationAccepted, 1).await;
    let team = wait_facts(&f, &c, EventType::TeamMembershipAdded, 1).await;
    assert_ne!(org[0].id, sent[0].id);
    assert_eq!(team[0].metadata["membership_state"], "active");
    assert_eq!(team[0].target_id, format!("{}/team:5", c.storage_id()));
    f.call(&c, "observe_listed", observed(&c, Utc::now(), false))
        .await;
    assert_eq!(facts(&f, &c, EventType::OrgInvitationAccepted).await, org);
    assert_eq!(facts(&f, &c, EventType::TeamMembershipAdded).await, team);
}

#[tokio::test]
async fn canonical_independent_pending_membership_is_explicitly_pending() {
    let f = Fixture::new(Scenario {
        org: Some(("admin", "pending")),
        pending: true,
        ..Default::default()
    })
    .await;
    let c = f.command(&[5]).await;
    let created = f.create(&c).await;
    let team = wait_facts(&f, &c, EventType::TeamMembershipAdded, 1).await;
    assert_eq!(team[0].metadata["membership_state"], "pending");
    assert!(facts(&f, &c, EventType::OrgInvitationSent).await.is_empty());
    assert_eq!(f.create(&c).await, created);
    assert_eq!(facts(&f, &c, EventType::TeamMembershipAdded).await, team);
}
