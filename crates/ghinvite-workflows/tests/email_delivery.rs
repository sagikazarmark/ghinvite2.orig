//! Actual Restate with standard opt-in composition, encrypted recovery and real budget.
//!
//! GitHub is the octoemu emulator: acme (without repositories, so it can be
//! renamed) with the organization App installed as 2. Email invitations are
//! the emulator's own (their addresses are checked in its state, since its
//! call log never records an address); a refused or unanswered POST is a
//! fault rule. The fake's per-request checks run over the call log.
#![cfg(feature = "integration")]
use chrono::Utc;
use ghinvite_core::{
    bulk::{BatchRef, BulkBatchId},
    email_delivery::{EmailCreateCommand, EmailRef},
    storage::{
        email_delivery::EmailDeliveryStorage,
        email_vault::{EmailVaultBinding, EmailVaultStorage},
    },
    targets::{MemberGrant, OrganizationTarget},
};
use ghinvite_e2e::{Call, Harness, journal::JournalAudit};
use octoemu::faults::{Fault, FaultAction, Reply};
use serde_json::{Value, json};
use std::sync::Arc;
#[path = "email_delivery/audit.rs"]
mod audit;
#[path = "email_delivery/bulk_cap.rs"]
mod bulk_cap;
#[path = "email_delivery/bulk_handoff.rs"]
mod bulk_handoff;
#[path = "email_delivery/journal.rs"]
mod journal;
#[path = "email_delivery/telemetry.rs"]
mod telemetry;
const MAIL: &str = "EMAIL_RUN_PRIVACY_MARKER+tag@example.com";
/// The emails the batch fixtures stage: `MAIL` and 60 numbered rows.
fn expected_email(email: &str) -> bool {
    email == MAIL
        || email
            .strip_prefix("BULK60_EMAIL_PRIVACY_MARKER+")
            .and_then(|s| s.strip_suffix("@example.com"))
            .and_then(|s| s.parse::<u64>().ok())
            .is_some_and(|row| (1..=60).contains(&row))
}
struct Fixture {
    h: Harness,
    storage: Arc<ghinvite_storage_sqlx::SqlxStorage>,
    vault: Arc<ghinvite_email_vault::EmailVault>,
}
impl Fixture {
    async fn new(status: u16) -> Self {
        Self::with_audit_fault(status, None).await
    }
    async fn with_audit_fault(
        status: u16,
        faults: Option<Arc<ghinvite_workflows::email_delivery::EmailAuditFaults>>,
    ) -> Self {
        // `invitee` holds the run's address (verified), so they can answer
        // its invitation.
        let mut world = ghinvite_e2e::github::seed().user(
            octoemu::seed::User::new(50, "invitee").email_address(
                octoemu::seed::Email::new(MAIL).primary(octoemu::seed::EmailVisibility::Private),
            ),
        );
        world.repos.clear();
        let h = Harness::start_with_emulator(ghinvite_e2e::github_emulator_with(world).await).await;
        // GitHub refuses (422) or does not answer (503) every invitation.
        if status != 201 {
            let reply = Reply::new(status);
            let reply = if status == 422 {
                reply.json(json!({"message":"Validation Failed","errors":[{"resource":"OrganizationInvitation","code":"custom","field":"email","message":"fixture refusal"}],"documentation_url":"https://docs.github.com/rest/orgs/members#create-an-organization-invitation"}))
            } else {
                reply.text("")
            };
            h.github().rule(
                &format!("invitations answer {status}"),
                Fault::on(
                    "POST",
                    "/orgs/acme/invitations",
                    FaultAction::Respond(reply),
                ),
            );
        }
        let storage = Arc::new(
            ghinvite_storage_sqlx::SqlxStorage::in_memory()
                .await
                .unwrap(),
        );
        let ring = Arc::new(
            kekring::Keyring::builder("p1-email")
                .generation(1, [7; 32])
                .current(1)
                .build()
                .unwrap(),
        );
        let vault = Arc::new(ghinvite_email_vault::EmailVault::new(storage.clone(), ring));
        let github_transport = ghinvite_e2e::github_transport(h.client.clone());
        let mut state = ghinvite_workflows::AppState::new(storage.clone())
            .with_email_deliveries(storage.clone(), vault.clone())
            .with_organization_delivery(storage.clone())
            .with_bulk_invites(storage.clone())
            .with_github_apps(Arc::new(
                ghinvite_workflows::RepositoryGithub::new(
                    github_transport,
                    &h.github_url,
                    ghinvite_e2e::installed_apps(&h.github_url, true),
                )
                .unwrap(),
            ));
        state.email_audit_faults = faults;
        h.restate
            .deploy(ghinvite_workflows::build_endpoint(state, storage.clone(), None).unwrap())
            .await;
        JournalAudit::retain(
            &h,
            &[
                "BulkInviteBatch",
                "BulkProjection",
                "EmailInviteDelivery",
                "EmailDeliveryProjection",
                "InviteBudget",
                "InviteBudgetProjection",
            ],
        )
        .await;
        let mut setup = ghinvite_e2e::setup_command(
            json!({"installation_id":2,"account_id":100,"account_login":"acme","account_type":"Organization","selected_repos":[],"installed_at":"2026-01-01T00:00:00Z"}),
        );
        setup["verified"]["app"] = json!("org");
        setup["verified"]["permissions"] = json!({"members":"write","metadata":"read"});
        let result = h
            .restate
            .invoke(
                &Call::object("Installation", "org:2", "onboard"),
                Some(&setup),
                None,
            )
            .await;
        assert_eq!(result.status, 200, "{result:?}");
        Self { h, storage, vault }
    }
    async fn command(&self, active: bool) -> EmailCreateCommand {
        let command = EmailCreateCommand {
            batch: BatchRef {
                batch_id: BulkBatchId::new(),
                row: 1,
            },
            account_id: 100,
            installation_id: 2,
            organization: OrganizationTarget {
                org_id: 100,
                org_login: "acme".into(),
                grant: MemberGrant::Member,
            },
            teams: vec![],
            email_ref: EmailRef::from_bytes([8; 32]),
            approved_by: 7,
            approved_at: Utc::now(),
        };
        let b = EmailVaultBinding {
            email_ref: command.email_ref,
            batch: command.batch,
            account_id: 100,
            installation_id: 2,
        };
        self.vault
            .stage(
                &b,
                &ghinvite_email_vault::Address::parse(MAIL).unwrap(),
                command.approved_at.timestamp(),
            )
            .await
            .unwrap();
        if active {
            assert!(
                self.storage
                    .activate_email(&b, command.approved_at.timestamp())
                    .await
                    .unwrap()
            );
        }
        command
    }
    async fn create(&self, c: &EmailCreateCommand) -> Value {
        let r = self
            .h
            .restate
            .invoke(
                &Call::object("EmailInviteDelivery", &c.delivery_key(), "create"),
                Some(&serde_json::to_value(c).unwrap()),
                None,
            )
            .await;
        assert_eq!(r.status, 200, "{r:?}");
        let journal = self.h.restate.admin().journal(r.invocation_id()).await;
        assert!(!journal.is_empty());
        for entry in journal {
            assert!(!entry.raw_contains(MAIL));
            assert!(!entry.raw_contains("ghs_"));
        }
        r.body
    }
    /// The invitation POSTs, after checking every call as the fake did:
    /// only the routes email delivery uses, each token with one of the
    /// organization App's profiles, each POST asking for a direct member
    /// without teams, and every invitation addressed to a staged email.
    fn posts(&self) -> usize {
        let calls = self.h.emulator().calls();
        for call in &calls {
            let path = call.path.split('?').next().unwrap_or_default();
            let fields = call.body_fields.clone().unwrap_or_default();
            match (call.method.as_str(), path) {
                ("POST", "/app/installations/2/access_tokens") => assert!(
                    [
                        json!({"members":"read","metadata":"read"}),
                        json!({"members":"write","metadata":"read"})
                    ]
                    .contains(&fields["permissions"]),
                    "{fields:?}"
                ),
                ("POST", "/orgs/acme/invitations") => {
                    assert_eq!(fields.get("role"), Some(&json!("direct_member")));
                    assert_eq!(fields.get("team_ids"), Some(&json!([])));
                    assert!(fields.get("invitee_id").is_none());
                }
                ("GET", "/app/installations/2" | "/orgs/acme" | "/orgs/renamed") => {}
                (
                    "GET",
                    "/orgs/acme/invitations"
                    | "/orgs/acme/failed_invitations"
                    | "/orgs/acme/teams"
                    | "/orgs/renamed/invitations"
                    | "/orgs/renamed/failed_invitations"
                    | "/orgs/renamed/teams",
                ) => {}
                (method, path) => {
                    panic!("forbidden or unexpected email delivery HTTP call {method} {path}")
                }
            }
        }
        let state = self.h.emulator().state();
        for invitation in state["world"]["org_invitations"]
            .as_array()
            .into_iter()
            .flatten()
        {
            let email = invitation["email"].as_str().unwrap_or_default();
            assert!(expected_email(email), "an invitation to {email}");
        }
        calls
            .iter()
            .filter(|call| call.method == "POST" && call.path == "/orgs/acme/invitations")
            .count()
    }
    /// The open invitation GitHub holds for `email`.
    fn invitation(&self, email: &str) -> Option<u64> {
        let state = self.h.emulator().state();
        state["world"]["org_invitations"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|invitation| invitation["email"] == email)
            .and_then(|invitation| invitation["id"].as_u64())
    }
    /// acme changes its login.
    async fn rename(&self, to: &str) {
        let admin = self.h.emulator().admin_base().to_owned();
        self.h
            .client
            .post(format!("{admin}/orgs/acme/rename"))
            .json(&json!({"login": to}))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap();
    }
    async fn budget(&self) -> Value {
        let r = self
            .h
            .restate
            .invoke(
                &Call::object("InviteBudget", "org:100", "snapshot"),
                None::<&Value>,
                None,
            )
            .await;
        assert_eq!(r.status, 200);
        r.body
    }
}
#[tokio::test]
async fn confirmed_email_create_replays_one_post_without_requester_or_plaintext_journal() {
    let f = Fixture::new(201).await;
    let c = f.command(true).await;
    let first = f.create(&c).await;
    assert_eq!(first["outcome"]["kind"], "created");
    assert_eq!(
        first["outcome"]["upstream_id"].as_u64(),
        f.invitation(MAIL),
        "the invitation GitHub holds for the address"
    );
    for _ in 0..3 {
        assert_eq!(f.create(&c).await, first);
    }
    assert_eq!(f.posts(), 1);
    let mut changed = c.clone();
    changed.approved_by = 9;
    let conflict =
        f.h.restate
            .invoke(
                &Call::object("EmailInviteDelivery", &c.delivery_key(), "create"),
                Some(&serde_json::to_value(&changed).unwrap()),
                None,
            )
            .await;
    assert_eq!(conflict.status, 409);
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            if f.storage
                .email_delivery(&c.storage_id())
                .await
                .unwrap()
                .is_some_and(|s| {
                    matches!(
                        s.outcome,
                        ghinvite_core::email_delivery::EmailCreateOutcome::Created { .. }
                    )
                })
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(f.budget().await["content_used_hour"], 1);
    let rows = f
        .storage
        .batch_email_deliveries(100, c.batch.batch_id)
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert!(!serde_json::to_string(&rows).unwrap().contains(MAIL));
    // The invitee declines: the invitation leaves GitHub's pending list.
    let admin = f.h.emulator().admin_base().to_owned();
    f.h.client
        .post(format!("{admin}/act/invitee/orgs/acme/decline"))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    assert_eq!(f.invitation(MAIL), None);
    let target = json!({"account":{"app":"org","account_id":100},"target_id":100,"deliveries":[c.storage_id()]});
    let observed =
        f.h.restate
            .invoke(
                &Call::service("OrganizationReconcile", "target"),
                Some(&target),
                None,
            )
            .await;
    assert_eq!(observed.status, 200, "{observed:?}");
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let status =
                f.h.restate
                    .invoke(
                        &Call::object("EmailInviteDelivery", &c.delivery_key(), "status"),
                        None::<&Value>,
                        None,
                    )
                    .await;
            if status.body["settlement"]["kind"] == "no_longer_pending" {
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
#[tokio::test]
async fn ambiguous_post_stays_unknown_charged_and_never_retries_even_after_list_absence() {
    let f = Fixture::new(503).await;
    let c = f.command(true).await;
    let first = f.create(&c).await;
    assert_eq!(first["outcome"]["kind"], "outcome_unknown");
    let unknown: chrono::DateTime<Utc> =
        serde_json::from_value(first["unknown_since"].clone()).unwrap();
    assert!(unknown >= c.approved_at && unknown <= Utc::now());
    assert!(first.get("last_observed_at").is_none());
    for _ in 0..3 {
        assert_eq!(f.create(&c).await, first);
    }
    assert_eq!(f.posts(), 1);
    assert_eq!(f.budget().await["content_used_hour"], 1);
    let at = Utc::now();
    let observation = ghinvite_core::org_observation::OrganizationObservation {
        account_id: 100,
        installation_id: 2,
        org_id: 100,
        org_login: "acme".into(),
        observation_started_at: at,
        observed_at: at,
        pending: vec![],
        failed: vec![],
    };
    let r =
        f.h.restate
            .invoke(
                &Call::object("EmailInviteDelivery", &c.delivery_key(), "reconcile"),
                Some(&serde_json::to_value(&observation).unwrap()),
                None,
            )
            .await;
    assert_eq!(r.status, 200);
    assert_eq!(f.create(&c).await, first);
    assert_eq!(f.posts(), 1);
    f.h.finish().await;
}
#[tokio::test]
async fn staged_address_without_activation_blocks_and_refunds_unused_slot() {
    let f = Fixture::new(201).await;
    let c = f.command(false).await;
    let first = f.create(&c).await;
    assert_eq!(
        first["outcome"],
        json!({"kind":"blocked","reason":"email_recovery_unavailable"})
    );
    assert_eq!(f.posts(), 0);
    assert_eq!(f.budget().await["content_used_hour"], 0);
    f.h.finish().await;
}

#[tokio::test]
async fn preinitialization_cancel_fences_later_create_and_does_not_reserve_or_post() {
    let f = Fixture::new(201).await;
    let c = f.command(true).await;
    let r =
        f.h.restate
            .invoke(
                &Call::object("EmailInviteDelivery", &c.delivery_key(), "cancel"),
                Some(&serde_json::to_value(&c).unwrap()),
                None,
            )
            .await;
    assert_eq!(r.status, 200, "{r:?}");
    assert_eq!(
        r.body["outcome"],
        json!({"kind":"aborted","reason":"batch_cancelled"})
    );
    assert_eq!(f.create(&c).await, r.body);
    assert_eq!(f.posts(), 0);
    assert_eq!(f.budget().await["content_used_hour"], 0);
    f.h.finish().await;
}
#[tokio::test]
async fn positive_rejection_releases_fence_and_unused_budget_without_repeating_failed_receipt() {
    let f = Fixture::new(422).await;
    let c = f.command(true).await;
    let r = f.create(&c).await;
    assert_eq!(r["outcome"]["kind"], "failed");
    assert_eq!(f.create(&c).await, r);
    assert_eq!(f.posts(), 1);
    assert_eq!(f.budget().await["content_used_hour"], 0);
    assert!(
        !f.storage
            .email_attempt_exists(&ghinvite_core::email_delivery::EmailAttempt { create: c })
            .await
            .unwrap()
    );
    f.h.finish().await;
}

#[path = "email_delivery/bulk_journal.rs"]
mod bulk_journal;
