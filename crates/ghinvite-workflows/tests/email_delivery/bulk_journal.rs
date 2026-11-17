//! Real encrypted email Submit branch, identity-only progress and fixed Bulk inventory.
use super::*;
use ghinvite_core::{
    bulk::*, bulk_commands::*, invitation_link::AccountAdmin, storage::RecordStorage,
    targets::Targets,
};
use ghinvite_e2e::RunPath;
const SERVICES: &[&str] = &["BulkInviteBatch", "BulkProjection"];
const PATHS: &[RunPath] = &[
    RunPath::new("BulkInviteBatch", "create", &["bulk_create_time"]),
    RunPath::new("BulkInviteBatch", "remove_rows", &[]),
    RunPath::new(
        "BulkInviteBatch",
        "submit",
        &[
            "bulk_submit_time",
            "batch_email_activation_time",
            "batch_email_activation",
        ],
    ),
    RunPath::new("BulkInviteBatch", "submit", &[]),
    RunPath::new("BulkInviteBatch", "cancel_remaining", &["bulk_cancel_time"]),
    RunPath::new("BulkInviteBatch", "on_item_progress", &[]),
    RunPath::new("BulkInviteBatch", "review", &[]),
    RunPath::new("BulkInviteBatch", "progress", &[]),
    RunPath::new("BulkInviteBatch", "progress_page", &[]),
    RunPath::new("BulkProjection", "apply", &["apply_bulk_projection"]),
];
fn op() -> ghinvite_core::request_lifecycle::LifecycleOperationId {
    ghinvite_core::RequestId::new()
        .to_string()
        .try_into()
        .unwrap()
}
async fn call(
    a: &mut JournalAudit,
    f: &Fixture,
    service: &str,
    key: &str,
    handler: &str,
    input: Value,
) -> Value {
    let reply = a.invoke(&f.h, service, key, handler, &input).await;
    assert_eq!(reply.status, 200, "{reply:?}");
    reply.body
}
#[tokio::test]
async fn bulk_email_submit_callback_uses_actual_owner_and_fixed_all_handler_inventory() {
    let f = Fixture::new(201).await;
    f.storage
        .upsert_user(&ghinvite_core::User {
            user_id: 7,
            login: "creator".into(),
            avatar_url: None,
            github_created_at: None,
            last_seen_at: Utc::now(),
        })
        .await
        .unwrap();
    let c = f.command(false).await;
    let batch = c.batch.batch_id;
    let key = batch.to_string();
    let q = BatchQuery {
        batch_id: batch,
        admin: AccountAdmin {
            account_id: 100,
            user_id: 7,
        },
    };
    let review = Review::new(
        Manifest {
            batch_id: batch,
            admin: q.admin.clone(),
            installation_id: 2,
            organization_installation_id: None,
            targets: Targets {
                organization: Some(c.organization.clone()),
                ..Targets::default()
            },
            access_lifetime_seconds: None,
            entries: vec![Entry::Email {
                email_ref: c.email_ref,
            }],
        },
        vec![RowEvidence {
            previews: vec![],
            row: 1,
            resolution: Resolution::Email,
            all_targets_satisfied: false,
            observed_at: c.approved_at,
            next_budget_slot: None,
        }],
        c.approved_at,
    )
    .unwrap();
    let mut a = JournalAudit::default();
    call(
        &mut a,
        &f,
        "BulkInviteBatch",
        &key,
        "create",
        json!(CreateBatch {
            operation_id: op(),
            review
        }),
    )
    .await;
    call(
        &mut a,
        &f,
        "BulkInviteBatch",
        &key,
        "remove_rows",
        json!(EditBatch {
            query: q.clone(),
            operation_id: op(),
            expected_revision: 1,
            remove: Default::default(),
            personal_link_rows: Default::default()
        }),
    )
    .await;
    let submit = SubmitBatch {
        query: q.clone(),
        operation_id: op(),
        expected_revision: 2,
        submission: Submission {
            submitted_by: 7,
            requests: vec![],
            emails: vec![c.clone()],
            personal_links: vec![],
            submitted_at: c.approved_at,
        },
    };
    f.storage
        .debug_set_email_retirement_failure(true)
        .await
        .unwrap();
    let review = call(&mut a, &f, "BulkInviteBatch", &key, "submit", json!(submit)).await;
    call(&mut a, &f, "BulkInviteBatch", &key, "submit", json!(submit)).await;
    let snapshot = tokio::time::timeout(std::time::Duration::from_secs(15), async {
        loop {
            let r =
                f.h.restate
                    .invoke(
                        &Call::object("EmailInviteDelivery", &c.delivery_key(), "status"),
                        None::<&Value>,
                        None,
                    )
                    .await;
            if r.body["outcome"]["kind"] == "created" {
                break r.body;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    // The producer's actual one-way notification must complete; do not replace
    // this evidence with only a directly invoked callback.
    let producer_id=tokio::time::timeout(std::time::Duration::from_secs(15),async{
        loop{
            let rows=f.h.restate.admin().sql_or_panic(&format!("SELECT id FROM sys_invocation WHERE target_service_name='BulkInviteBatch' AND target_service_key='{key}' AND target_handler_name='on_item_progress' AND status='completed'")).await;
            if let Some(id)=rows.first().and_then(|r|r["id"].as_str()){break id.to_owned()}
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    }).await.unwrap();
    a.capture(&f.h, &producer_id).await;
    assert!(
        f.storage
            .email_record(c.email_ref)
            .await
            .unwrap()
            .unwrap()
            .retired_at
            .is_none()
    );
    let interrupted=tokio::time::timeout(std::time::Duration::from_secs(15),async {
        loop {
            let rows=f.h.restate.admin().sql_or_panic(&format!("SELECT id FROM sys_invocation WHERE target_service_name='EmailInviteDelivery' AND target_service_key='{}' AND target_handler_name='create' AND status!='completed'",c.delivery_key())).await;
            if let Some(id)=rows.first().and_then(|r|r["id"].as_str()) {
                let journal=f.h.restate.admin().journal(id).await;
                if journal.iter().any(|e|e.name.as_deref()==Some("email_retire_recovery")){break id.to_owned()}
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    }).await.unwrap();
    f.h.restate.admin().kill(&interrupted).await;
    f.h.restate
        .admin()
        .await_status(&interrupted, &["completed"])
        .await;
    f.storage
        .debug_set_email_retirement_failure(false)
        .await
        .unwrap();
    let repaired =
        f.h.restate
            .invoke(
                &Call::object("EmailInviteDelivery", &c.delivery_key(), "cancel"),
                Some(&json!(c)),
                None,
            )
            .await;
    assert_eq!(repaired.status, 200, "{repaired:?}");
    assert_eq!(repaired.body, snapshot);
    let runs =
        f.h.restate
            .admin()
            .journal(repaired.invocation_id())
            .await
            .into_iter()
            .filter(|e| e.is_run())
            .map(|e| e.name.unwrap())
            .collect::<Vec<_>>();
    assert_eq!(runs, vec!["email_retire_recovery"]);
    let original = chrono::DateTime::parse_from_rfc3339(snapshot["confirmed_at"].as_str().unwrap())
        .unwrap()
        .timestamp();
    let record = f.storage.email_record(c.email_ref).await.unwrap().unwrap();
    assert_eq!(record.retired_at, Some(original));
    assert_eq!(record.purge_after, Some(original + 86400));

    for _ in 0..3 {
        call(
            &mut a,
            &f,
            "BulkInviteBatch",
            &key,
            "on_item_progress",
            json!(ItemProgressNotice::Email { source: c.batch }),
        )
        .await;
    }
    let rows=f.h.restate.admin().sql_or_panic(&format!("SELECT value_utf8 FROM state WHERE service_name='BulkInviteBatch' AND service_key='{key}' AND key='batch/item/1'")).await;
    let value: Value = serde_json::from_str(rows[0]["value_utf8"].as_str().unwrap()).unwrap();
    assert_eq!(value["Email"], snapshot);
    assert!(
        value["Email"]["settlement"].is_null(),
        "created email is not acceptance"
    );
    for h in ["review", "progress"] {
        call(&mut a, &f, "BulkInviteBatch", &key, h, json!(q)).await;
    }
    call(
        &mut a,
        &f,
        "BulkInviteBatch",
        &key,
        "progress_page",
        json!(ProgressPageQuery {
            query: q.clone(),
            cursor: None
        }),
    )
    .await;
    call(
        &mut a,
        &f,
        "BulkInviteBatch",
        &key,
        "cancel_remaining",
        json!(CancelBatch {
            query: q,
            operation_id: op()
        }),
    )
    .await;
    call(
        &mut a,
        &f,
        "BulkProjection",
        &key,
        "apply",
        json!({"review":review,"events":[]}),
    )
    .await;
    let walked = a.check(&f.h, SERVICES, PATHS).await;
    assert_eq!(walked.handlers, 9);
    assert_eq!(walked.paths, PATHS.len());
    eprintln!("Bulk email fixed journal table: {walked}");
    let journals=f.h.restate.admin().sql_or_panic("SELECT id FROM sys_invocation WHERE target_service_name='BulkInviteBatch' AND status='completed'").await;
    for row in journals {
        for entry in
            f.h.restate
                .admin()
                .journal(row["id"].as_str().unwrap())
                .await
        {
            assert!(!entry.raw_contains(MAIL));
            assert!(!entry.raw_contains("ghs_"));
        }
    }
    assert_eq!(f.posts(), 1);
    f.h.finish().await;
}
