//! Source-reviewed fixed paths; never inferred from the journals being checked.
use super::*;
use ghinvite_e2e::RunPath;
const SERVICES: &[&str] = &["EmailInviteDelivery", "EmailDeliveryProjection"];
const PATHS: &[RunPath] = &[
    RunPath::new("EmailInviteDelivery", "create", &[]),
    RunPath::new("EmailInviteDelivery", "create", &["email_delivery_time"]),
    RunPath::new(
        "EmailInviteDelivery",
        "create",
        &[
            "email_delivery_time",
            "email_write_stage",
            "email_write_completed_at",
        ],
    ),
    RunPath::new(
        "EmailInviteDelivery",
        "create",
        &[
            "email_delivery_time",
            "email_write_stage",
            "email_write_completed_at",
            "email_retire_recovery",
        ],
    ),
    RunPath::new(
        "EmailInviteDelivery",
        "create",
        &[
            "email_delivery_time",
            "email_write_stage",
            "email_write_completed_at",
            "email_release_rejected_attempt",
            "email_retire_recovery",
        ],
    ),
    RunPath::new("EmailInviteDelivery", "cancel", &[]),
    RunPath::new(
        "EmailInviteDelivery",
        "cancel",
        &["email_cancel_fence", "email_unknown_time"],
    ),
    RunPath::new(
        "EmailInviteDelivery",
        "cancel",
        &[
            "email_cancel_fence",
            "email_cancel_time",
            "email_retire_recovery",
        ],
    ),
    RunPath::new("EmailInviteDelivery", "recheck", &[]),
    RunPath::new("EmailInviteDelivery", "recheck", &["email_delivery_time"]),
    RunPath::new(
        "EmailInviteDelivery",
        "recheck",
        &[
            "email_delivery_time",
            "email_write_stage",
            "email_write_completed_at",
            "email_retire_recovery",
        ],
    ),
    RunPath::new("EmailInviteDelivery", "reconcile", &[]),
    RunPath::new(
        "EmailInviteDelivery",
        "reconcile",
        &["email_observation_time"],
    ),
    RunPath::new(
        "EmailInviteDelivery",
        "reconcile",
        &["email_observation_time", "email_retire_recovery"],
    ),
    RunPath::new("EmailInviteDelivery", "status", &[]),
    RunPath::new(
        "EmailDeliveryProjection",
        "apply",
        &["project_email_delivery"],
    ),
];
async fn invoke(
    audit: &mut JournalAudit,
    f: &Fixture,
    service: &str,
    key: &str,
    handler: &str,
    body: Value,
) -> Value {
    let reply = audit.invoke(&f.h, service, key, handler, &body).await;
    assert_eq!(reply.status, 200, "{service}/{handler}: {reply:?}");
    reply.body
}
async fn stage(f: &Fixture, base: &EmailCreateCommand, marker: u8) -> EmailCreateCommand {
    let mut c = base.clone();
    c.batch.batch_id = BulkBatchId::new();
    c.email_ref = EmailRef::from_bytes([marker; 32]);
    let binding = EmailVaultBinding {
        email_ref: c.email_ref,
        batch: c.batch,
        account_id: c.account_id,
        installation_id: c.installation_id,
    };
    f.vault
        .stage(
            &binding,
            &ghinvite_email_vault::Address::parse(MAIL).unwrap(),
            c.approved_at.timestamp(),
        )
        .await
        .unwrap();
    assert!(
        f.storage
            .activate_email(&binding, c.approved_at.timestamp())
            .await
            .unwrap()
    );
    c
}
fn observation() -> Value {
    let now = Utc::now();
    json!({"account_id":100,"installation_id":2,"org_id":100,"org_login":"acme","observation_started_at":now,"observed_at":now,"pending":[],"failed":[]})
}

#[tokio::test]
async fn fixed_email_journal_paths_cover_every_deployed_handler() {
    let mut audit = JournalAudit::default();
    let f = Fixture::new(201).await;
    let c = f.command(true).await;
    let key = c.delivery_key();
    let body = serde_json::to_value(&c).unwrap();
    let created = invoke(
        &mut audit,
        &f,
        "EmailInviteDelivery",
        &key,
        "create",
        body.clone(),
    )
    .await;
    assert_eq!(created["outcome"]["kind"], "created");
    assert_eq!(
        invoke(
            &mut audit,
            &f,
            "EmailInviteDelivery",
            &key,
            "create",
            body.clone()
        )
        .await,
        created
    );
    assert_eq!(
        invoke(&mut audit, &f, "EmailInviteDelivery", &key, "cancel", body).await,
        created
    );
    assert_eq!(
        invoke(
            &mut audit,
            &f,
            "EmailInviteDelivery",
            &key,
            "status",
            Value::Null
        )
        .await,
        created
    );
    invoke(
        &mut audit,
        &f,
        "EmailDeliveryProjection",
        &key,
        "apply",
        created,
    )
    .await;
    invoke(
        &mut audit,
        &f,
        "EmailInviteDelivery",
        &key,
        "recheck",
        json!({"generation":99}),
    )
    .await;
    invoke(
        &mut audit,
        &f,
        "EmailInviteDelivery",
        &key,
        "reconcile",
        observation(),
    )
    .await;
    // Exact already-settled replay has no pending drain; the first observation
    // advanced the revision and retired using the original confirmation clock.
    invoke(
        &mut audit,
        &f,
        "EmailInviteDelivery",
        &key,
        "reconcile",
        observation(),
    )
    .await;
    let stopped = stage(&f, &c, 9).await;
    let stopped_key = stopped.delivery_key();
    invoke(
        &mut audit,
        &f,
        "EmailInviteDelivery",
        &stopped_key,
        "reconcile",
        observation(),
    )
    .await;
    let aborted = invoke(
        &mut audit,
        &f,
        "EmailInviteDelivery",
        &stopped_key,
        "cancel",
        json!(stopped),
    )
    .await;
    assert_eq!(aborted["outcome"]["kind"], "aborted");
    let fenced = stage(&f, &c, 10).await;
    assert_eq!(
        f.storage
            .claim_email_attempt(&ghinvite_core::email_delivery::EmailAttempt {
                create: fenced.clone()
            })
            .await
            .unwrap(),
        Some(1)
    );
    let unknown = invoke(
        &mut audit,
        &f,
        "EmailInviteDelivery",
        &fenced.delivery_key(),
        "cancel",
        json!(fenced),
    )
    .await;
    assert_eq!(unknown["outcome"]["kind"], "outcome_unknown");
    let throttled = stage(&f, &c, 11).await;
    let limit = f
        .h
        .restate
        .invoke(
            &Call::object("InviteBudget", "org:100", "record_limit"),
            Some(
                &json!({"bucket":{"kind":"org"},"until":Utc::now()+chrono::Duration::seconds(60)}),
            ),
            None,
        )
        .await;
    assert_eq!(limit.status, 200);
    let result = invoke(
        &mut audit,
        &f,
        "EmailInviteDelivery",
        &throttled.delivery_key(),
        "create",
        json!(throttled),
    )
    .await;
    assert_eq!(result["outcome"]["kind"], "throttled");
    invoke(
        &mut audit,
        &f,
        "EmailInviteDelivery",
        &throttled.delivery_key(),
        "recheck",
        json!({"generation":1}),
    )
    .await;
    assert_eq!(f.posts(), 1);
    f.h.finish().await;

    for status in [503, 422] {
        let f = Fixture::new(status).await;
        let c = f.command(true).await;
        let result = invoke(
            &mut audit,
            &f,
            "EmailInviteDelivery",
            &c.delivery_key(),
            "create",
            json!(c),
        )
        .await;
        assert_eq!(
            result["outcome"]["kind"],
            if status == 503 {
                "outcome_unknown"
            } else {
                "failed"
            }
        );
        assert_eq!(f.posts(), 1);
        f.h.finish().await;
    }

    let f = Fixture::new(201).await;
    let c = f.command(false).await;
    let first = invoke(
        &mut audit,
        &f,
        "EmailInviteDelivery",
        &c.delivery_key(),
        "create",
        json!(c),
    )
    .await;
    assert_eq!(first["outcome"]["kind"], "blocked");
    let binding = EmailVaultBinding {
        email_ref: c.email_ref,
        batch: c.batch,
        account_id: c.account_id,
        installation_id: c.installation_id,
    };
    assert!(
        f.storage
            .activate_email(&binding, Utc::now().timestamp())
            .await
            .unwrap()
    );
    invoke(
        &mut audit,
        &f,
        "EmailInviteDelivery",
        &c.delivery_key(),
        "recheck",
        json!({"generation":1}),
    )
    .await;
    assert_eq!(f.posts(), 1);
    let walked = audit.check(&f.h, SERVICES, PATHS).await;
    assert_eq!(walked.handlers, 6);
    assert_eq!(walked.paths, 16);
    eprintln!("email fixed journal inventory: {walked}");
    f.h.finish().await;
}
