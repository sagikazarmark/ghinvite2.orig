//! Operational clocks are retained owner facts, never inferred from approval.
use super::*;
use ghinvite_core::{
    email_delivery::EmailDeliverySnapshot,
    org_delivery::OrganizationInvitationRole,
    org_observation::{ListedOrganizationInvitation, OrganizationObservation},
};
async fn read(f: &Fixture, c: &EmailCreateCommand) -> EmailDeliverySnapshot {
    let reply =
        f.h.restate
            .invoke(
                &Call::object("EmailInviteDelivery", &c.delivery_key(), "status"),
                None,
                None,
            )
            .await;
    assert_eq!(reply.status, 200);
    serde_json::from_value(reply.body).unwrap()
}
async fn observe(f: &Fixture, c: &EmailCreateCommand, input: &OrganizationObservation) {
    let reply =
        f.h.restate
            .invoke(
                &Call::object("EmailInviteDelivery", &c.delivery_key(), "reconcile"),
                Some(&json!(input)),
                None,
            )
            .await;
    assert_eq!(reply.status, 200, "{reply:?}");
}
#[tokio::test]
async fn pending_complete_observation_advances_clock_and_rename_preserves_receipt() {
    let f = Fixture::new(201).await;
    let c = f.command(true).await;
    let created: EmailDeliverySnapshot = serde_json::from_value(f.create(&c).await).unwrap();
    assert_eq!(created.last_observed_at, None);
    let at = Utc::now();
    let mut input = OrganizationObservation {
        account_id: 100,
        installation_id: 2,
        org_id: 100,
        org_login: "acme".into(),
        observation_started_at: at,
        observed_at: at,
        pending: vec![ListedOrganizationInvitation {
            upstream_id: f.invitation(MAIL).expect("GitHub holds the invitation"),
            login: None,
            role: OrganizationInvitationRole::DirectMember,
            created_at: created.confirmed_at.unwrap(),
            team_count: 0,
        }],
        failed: vec![],
    };
    observe(&f, &c, &input).await;
    let pending = read(&f, &c).await;
    assert_eq!(pending.last_observed_at, Some(at));
    assert_eq!(pending.outcome, created.outcome);
    assert_eq!(pending.confirmed_at, created.confirmed_at);
    assert_eq!(pending.settlement, None);
    observe(&f, &c, &input).await;
    assert_eq!(read(&f, &c).await, pending);
    let mut stale = input.clone();
    stale.observation_started_at = created.confirmed_at.unwrap();
    stale.pending.clear();
    observe(&f, &c, &stale).await;
    assert_eq!(
        read(&f, &c).await,
        pending,
        "older absence cannot settle newer pending evidence"
    );

    f.rename("renamed").await;
    let mut setup = ghinvite_e2e::setup_command(json!({"installation_id":2,"account_id":100,
        "account_login":"renamed","account_type":"Organization","selected_repos":[],
        "installed_at":"2026-01-01T00:00:00Z"}));
    setup["verified"]["app"] = json!("org");
    setup["verified"]["permissions"] = json!({"members":"write","metadata":"read"});
    let result =
        f.h.restate
            .invoke(
                &Call::object("Installation", "org:2", "onboard"),
                Some(&setup),
                None,
            )
            .await;
    assert_eq!(result.status, 200, "{result:?}");
    input.org_login = "renamed".into();
    input.observation_started_at = Utc::now();
    input.observed_at = input.observation_started_at;
    observe(&f, &c, &input).await;
    let renamed = read(&f, &c).await;
    assert!(renamed.last_observed_at > pending.last_observed_at);
    assert_eq!(renamed.command, c);
    assert_eq!(renamed.outcome, created.outcome);
    input.pending.clear();
    input.observation_started_at = Utc::now();
    input.observed_at = input.observation_started_at;
    observe(&f, &c, &input).await;
    let terminal = read(&f, &c).await;
    assert_eq!(
        terminal.settlement,
        Some(ghinvite_core::email_delivery::EmailSettlement::NoLongerPending)
    );
    assert_eq!(terminal.confirmed_at, created.confirmed_at);
    assert_eq!(terminal.last_observed_at, Some(input.observed_at));
    assert_eq!(terminal.settled_at, Some(input.observed_at));
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            if f.storage
                .email_delivery(&c.storage_id())
                .await
                .unwrap()
                .as_ref()
                == Some(&terminal)
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(f.posts(), 1);
    f.h.finish().await;
}
