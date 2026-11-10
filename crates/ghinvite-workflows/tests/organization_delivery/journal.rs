//! Fixed source-reviewed paths, never generated from the journals under test.
use super::*;
use ghinvite_e2e::RunPath;
const SERVICES: &[&str] = &[
    "OrganizationDelivery",
    "OrganizationEligibility",
    "OrganizationProjection",
    "OrganizationReconcile",
];
const PATHS: &[RunPath] = &[
    RunPath::new("OrganizationDelivery", "prepare_reissue", &[]),
    RunPath::new(
        "OrganizationDelivery",
        "cancel_unsent",
        &[
            "organization_batch_cancel_time",
            "organization_batch_cancel_fence",
            "organization_batch_cancel_fence",
        ],
    ),
    RunPath::new("OrganizationDelivery", "cancel_unsent", &[]),
    RunPath::new("OrganizationDelivery", "create", &[]),
    RunPath::new(
        "OrganizationDelivery",
        "create",
        &[
            "organization_delivery_time",
            "organization_control_fence",
            "organization_read_stage",
            "organization_write_stage",
            "organization_write_completed_at",
            "organization_post_write_control_time",
        ],
    ),
    RunPath::new(
        "OrganizationDelivery",
        "create",
        &["organization_delivery_time"],
    ),
    RunPath::new(
        "OrganizationDelivery",
        "create",
        &[
            "organization_delivery_time",
            "organization_control_fence",
            "organization_read_stage",
            "organization_audit_time",
            "organization_post_write_control_time",
            "organization_control_fence",
            "organization_wake_time",
        ],
    ),
    RunPath::new(
        "OrganizationDelivery",
        "control",
        &["organization_control_time", "organization_wake_time"],
    ),
    RunPath::new("OrganizationDelivery", "control", &[]),
    RunPath::new("OrganizationDelivery", "recheck", &[]),
    RunPath::new(
        "OrganizationDelivery",
        "recheck",
        &["organization_delivery_time"],
    ),
    RunPath::new("OrganizationDelivery", "capacity_changed", &[]),
    RunPath::new(
        "OrganizationDelivery",
        "capacity_changed",
        &[
            "organization_delivery_time",
            "organization_control_fence",
            "organization_read_stage",
            "organization_write_stage",
            "organization_write_completed_at",
            "organization_post_write_control_time",
        ],
    ),
    RunPath::new(
        "OrganizationDelivery",
        "observe_listed",
        &[
            "organization_observation_time",
            "organization_observe_memberships",
            "organization_observation_completed",
        ],
    ),
    RunPath::new("OrganizationDelivery", "observe_listed", &[]),
    RunPath::new(
        "OrganizationDelivery",
        "refresh",
        &["organization_refresh_time"],
    ),
    RunPath::new("OrganizationDelivery", "refresh", &[]),
    RunPath::new("OrganizationDelivery", "on_webhook", &[]),
    RunPath::new("OrganizationDelivery", "drive_cancellation", &[]),
    RunPath::new(
        "OrganizationDelivery",
        "drive_cancellation",
        &[
            "organization_cancel_read_time",
            "organization_cancel_read",
            "organization_cancel_after_read_time",
            "organization_cancel_write",
            "organization_cancel_completed_at",
            "organization_cancel_control_time",
        ],
    ),
    RunPath::new(
        "OrganizationDelivery",
        "tick_own_expiry",
        &["organization_expiry_tick_time"],
    ),
    RunPath::new(
        "OrganizationDelivery",
        "tick_own_expiry",
        &[
            "organization_expiry_tick_time",
            "organization_cancel_read_time",
            "organization_cancel_read",
            "organization_expiry_evidence_time",
        ],
    ),
    RunPath::new("OrganizationDelivery", "status", &[]),
    RunPath::new("OrganizationDelivery", "cascade_progress", &[]),
    RunPath::new(
        "OrganizationEligibility",
        "facts",
        &["organization_facts_time", "organization_facts_read"],
    ),
    RunPath::new(
        "OrganizationEligibility",
        "facts",
        &["organization_facts_time"],
    ),
    RunPath::new(
        "OrganizationEligibility",
        "refresh",
        &["organization_facts_time", "organization_facts_read"],
    ),
    RunPath::new("OrganizationEligibility", "refresh", &[]),
    RunPath::new(
        "OrganizationEligibility",
        "member_removed",
        &["organization_capacity_time"],
    ),
    RunPath::new("OrganizationEligibility", "wake_next", &[]),
    RunPath::new(
        "OrganizationProjection",
        "apply",
        &["project_organization_delivery"],
    ),
    RunPath::new(
        "OrganizationProjection",
        "telemetry",
        &["project_organization_telemetry"],
    ),
    RunPath::new(
        "OrganizationReconcile",
        "target",
        &["list_organization_invitations"],
    ),
    RunPath::new("OrganizationReconcile", "observe_next", &[]),
];
async fn invoke(
    a: &mut JournalAudit,
    f: &Fixture,
    service: &str,
    key: &str,
    handler: &str,
    input: Value,
) -> Value {
    let reply = a.invoke(&f.h, service, key, handler, &input).await;
    assert_eq!(reply.status, 200, "{service}/{handler}: {reply:?}");
    reply.body
}
fn envelope(
    c: &OrganizationCreateCommand,
    revision: u64,
    control: ghinvite_core::delivery_control::LinkControl,
) -> Value {
    json!({"envelope":{"identity":{"link_id":c.link_id,"request_id":c.request_id,"kind":"organization","target_id":100,"delivery_generation":1},"operation_id":ghinvite_core::link_lifecycle::control_operation_id(c.link_id.expect("linked fixture"),revision),"revision":revision,"hold_epoch":0,"at":Utc::now(),"control":control},"admitted_at":c.approved_at,"invitation_lifetime_seconds":604800})
}
fn hint() -> Value {
    json!({"account_id":100,"installation_id":2,"removed_user_id":999,"payload_sha256":vec![42;32]})
}
async fn capture_named(a: &mut JournalAudit, f: &Fixture, key: &str, handler: &str, name: &str) {
    assert!(["drive_cancellation", "recheck", "tick_own_expiry"].contains(&handler));
    let id=tokio::time::timeout(std::time::Duration::from_secs(15),async {
        loop {
            let rows=f.h.restate.admin().sql_or_panic(&format!("SELECT id FROM sys_invocation WHERE target_service_name='OrganizationDelivery' AND target_service_key='{key}' AND target_handler_name='{handler}' AND status='completed'")).await;
            for row in rows {
                let id=row["id"].as_str().unwrap();
                let journal=f.h.restate.admin().journal(id).await;
                if journal.iter().any(|entry|entry.is_run() && entry.name.as_deref()==Some(name)) {
                    return id.to_owned();
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    }).await.expect("actual one-way handler must complete with retained journal");
    a.capture(&f.h, &id).await;
}

#[tokio::test]
async fn fixed_organization_journal_inventory_walks_every_deployed_handler() {
    let f = Fixture::start_options(Default::default(), true, true, 604800).await;
    JournalAudit::retain(&f.h, SERVICES).await;
    let mut a = JournalAudit::default();
    let c = f.command(&[]).await;
    let key = c.delivery_key();
    let created = invoke(&mut a, &f, "OrganizationDelivery", &key, "create", json!(c)).await;
    assert_eq!(created["create"]["outcome"]["kind"], "created");
    // A settled-free old delivery is not eligible for reissue; the read-only
    // refusal has no external work. Positive removal paths live in reissue.rs.
    let reissue = invoke(
        &mut a,
        &f,
        "OrganizationDelivery",
        &key,
        "prepare_reissue",
        json!({"operation_id":RequestId::new(),"command":c}),
    )
    .await;
    assert_eq!(reissue, "pending");
    invoke(&mut a, &f, "OrganizationDelivery", &key, "create", json!(c)).await;
    invoke(
        &mut a,
        &f,
        "OrganizationDelivery",
        &key,
        "status",
        Value::Null,
    )
    .await;
    invoke(
        &mut a,
        &f,
        "OrganizationDelivery",
        &key,
        "drive_cancellation",
        Value::Null,
    )
    .await;
    invoke(
        &mut a,
        &f,
        "OrganizationDelivery",
        &key,
        "tick_own_expiry",
        json!({"generation":9,"upstream_id":99}),
    )
    .await;
    invoke(
        &mut a,
        &f,
        "OrganizationDelivery",
        &key,
        "recheck",
        json!({"generation":999}),
    )
    .await;
    invoke(
        &mut a,
        &f,
        "OrganizationDelivery",
        &key,
        "capacity_changed",
        hint(),
    )
    .await;
    invoke(&mut a,&f,"OrganizationDelivery",&key,"cascade_progress",json!({"identity":{"link_id":c.link_id,"request_id":c.request_id,"kind":"organization","target_id":100,"delivery_generation":1},"required_control_revision":1})).await;
    invoke(
        &mut a,
        &f,
        "OrganizationDelivery",
        &key,
        "observe_listed",
        observed(&c, Utc::now(), true),
    )
    .await;
    let absent = format!("{}:100:1", RequestId::new());
    invoke(
        &mut a,
        &f,
        "OrganizationDelivery",
        &absent,
        "observe_listed",
        observed(&c, Utc::now(), true),
    )
    .await;
    invoke(
        &mut a,
        &f,
        "OrganizationDelivery",
        &absent,
        "refresh",
        Value::Null,
    )
    .await;
    invoke(
        &mut a,
        &f,
        "OrganizationDelivery",
        &key,
        "refresh",
        Value::Null,
    )
    .await;
    invoke(
        &mut a,
        &f,
        "OrganizationDelivery",
        &key,
        "refresh",
        Value::Null,
    )
    .await;
    invoke(&mut a,&f,"OrganizationDelivery",&key,"on_webhook",json!({"identity":{"link_id":c.link_id,"request_id":c.request_id,"kind":"organization","target_id":100,"delivery_generation":1},"account_id":100,"installation_id":2,"requester_id":8,"team_id":null,"source":"organization_member_added","payload_sha256":vec![7;32],"received_at":Utc::now()})).await;
    invoke(
        &mut a,
        &f,
        "OrganizationProjection",
        &key,
        "apply",
        created.clone(),
    )
    .await;
    invoke(&mut a,&f,"OrganizationProjection",&key,"telemetry",json!({"request_id":c.request_id,"org_id":100,"delivery_generation":1,"account_id":100,"step":{"kind":"organization"},"unknown_since":null,"last_observed_at":Utc::now()})).await;
    // Production onboarding may prewarm facts. A serialized refresh then a
    // distinct authenticated capacity hint establishes a deterministic cold read.
    invoke(
        &mut a,
        &f,
        "OrganizationEligibility",
        "org:100",
        "refresh",
        json!({"account_id":100,"installation_id":2}),
    )
    .await;
    invoke(
        &mut a,
        &f,
        "OrganizationEligibility",
        "org:100",
        "member_removed",
        hint(),
    )
    .await;
    invoke(
        &mut a,
        &f,
        "OrganizationEligibility",
        "org:100",
        "facts",
        Value::Null,
    )
    .await;
    invoke(
        &mut a,
        &f,
        "OrganizationEligibility",
        "org:100",
        "facts",
        Value::Null,
    )
    .await;
    invoke(
        &mut a,
        &f,
        "OrganizationEligibility",
        "org:100",
        "refresh",
        json!({"account_id":100,"installation_id":2}),
    )
    .await;
    invoke(
        &mut a,
        &f,
        "OrganizationEligibility",
        "org:100",
        "refresh",
        json!({"account_id":100,"installation_id":3}),
    )
    .await;
    invoke(
        &mut a,
        &f,
        "OrganizationEligibility",
        "org:100",
        "member_removed",
        hint(),
    )
    .await;
    invoke(
        &mut a,
        &f,
        "OrganizationEligibility",
        "org:100",
        "member_removed",
        hint(),
    )
    .await;
    invoke(
        &mut a,
        &f,
        "OrganizationEligibility",
        "org:100",
        "wake_next",
        json!({"event":"unseen","next":0}),
    )
    .await;
    let target =
        json!({"account":{"app":"org","account_id":100},"target_id":100,"deliveries":[key]});
    let reply = a
        .invoke_service(&f.h, "OrganizationReconcile", "target", &target)
        .await;
    assert_eq!(reply.status, 200, "{reply:?}");
    let reply=a.invoke_service(&f.h,"OrganizationReconcile","observe_next",&json!({"target":{"account":{"app":"org","account_id":100},"target_id":100,"deliveries":[]},"observation":observed(&c,Utc::now(),true)})).await;
    assert_eq!(reply.status, 200, "{reply:?}");
    let control = envelope(&c, 2, revoke());
    invoke(
        &mut a,
        &f,
        "OrganizationDelivery",
        &key,
        "control",
        control.clone(),
    )
    .await;
    invoke(&mut a, &f, "OrganizationDelivery", &key, "control", control).await;
    terminal(&f, &c).await;
    // Shared status can expose settlement before the one-way handlers finish.
    capture_named(
        &mut a,
        &f,
        &key,
        "drive_cancellation",
        "organization_cancel_write",
    )
    .await;
    capture_named(&mut a, &f, &key, "recheck", "organization_delivery_time").await;
    // The first account consumed a create and cancel content slot. Use an
    // independent real budget to exercise capacity recovery rather than timing
    // this path around that account's content cooldown.
    let capacity = Fixture::start_options(
        Scenario {
            seats_full: true,
            ..Default::default()
        },
        true,
        true,
        604800,
    )
    .await;
    JournalAudit::retain(&capacity.h, SERVICES).await;
    let blocked = capacity.command(&[]).await;
    let blocked_key = blocked.delivery_key();
    let value = invoke(
        &mut a,
        &capacity,
        "OrganizationDelivery",
        &blocked_key,
        "create",
        json!(blocked),
    )
    .await;
    assert_eq!(value["create"]["outcome"]["reason"], "no_seat");
    capacity.github.0.lock().unwrap().seats_full = false;
    invoke(
        &mut a,
        &capacity,
        "OrganizationDelivery",
        &blocked_key,
        "capacity_changed",
        hint(),
    )
    .await;
    let recovered = standard_call(
        &capacity,
        "OrganizationDelivery",
        &blocked_key,
        "status",
        None,
    )
    .await;
    assert_eq!(recovered["create"]["outcome"]["kind"], "created");
    let expiry = Fixture::start_options(Default::default(), true, true, 3600).await;
    JournalAudit::retain(&expiry.h, SERVICES).await;
    let upstream_at = Utc::now() - chrono::Duration::seconds(3596);
    expiry.github.0.lock().unwrap().created_at = Some(upstream_at);
    let expires = expiry
        .command_at(&[], upstream_at - chrono::Duration::seconds(1))
        .await;
    expiry.create(&expires).await;
    let expired = terminal(&expiry, &expires).await;
    assert_eq!(
        expired.settlement.unwrap().event.metadata["reason"],
        "own_expiry"
    );
    capture_named(
        &mut a,
        &expiry,
        &expires.delivery_key(),
        "tick_own_expiry",
        "organization_expiry_evidence_time",
    )
    .await;
    let batch_command = super::cancel_unsent::command(&capacity).await;
    let stop = json!({"operation_id":RequestId::new(),"admin":{"account_id":100,"user_id":7},"command":batch_command});
    assert_eq!(
        invoke(
            &mut a,
            &capacity,
            "OrganizationDelivery",
            &batch_command.delivery_key(),
            "cancel_unsent",
            stop.clone()
        )
        .await,
        "aborted"
    );
    assert_eq!(
        invoke(
            &mut a,
            &capacity,
            "OrganizationDelivery",
            &batch_command.delivery_key(),
            "cancel_unsent",
            stop
        )
        .await,
        "aborted"
    );
    let stopped = invoke(
        &mut a,
        &capacity,
        "OrganizationDelivery",
        &batch_command.delivery_key(),
        "create",
        json!(batch_command),
    )
    .await;
    assert_eq!(stopped["create"]["outcome"]["kind"], "aborted");
    let walked = a.check(&f.h, SERVICES, PATHS).await;
    let other = a.check(&capacity.h, SERVICES, PATHS).await;
    assert_eq!(other.handlers, walked.handlers);
    assert_eq!(walked.handlers, 21);
    assert_eq!(walked.paths, PATHS.len());
    eprintln!("Organization fixed journal table: {walked}");
}
