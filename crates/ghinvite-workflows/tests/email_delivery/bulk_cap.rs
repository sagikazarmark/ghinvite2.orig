//! AC-BULK1 through the real batch owner, email owner and paced org budget.
use super::*;
use ghinvite_core::{
    bulk::{Entry, Manifest, Resolution, Review, RowEvidence, Submission},
    bulk_commands::{BatchProgress, BatchQuery, CreateBatch, Observation, SubmitBatch},
    email_delivery::EmailCreateOutcome,
    invitation_link::AccountAdmin,
    targets::Targets,
};

fn operation() -> ghinvite_core::request_lifecycle::LifecycleOperationId {
    ghinvite_core::RequestId::new()
        .to_string()
        .try_into()
        .unwrap()
}
async fn call<T: serde::Serialize>(
    f: &Fixture,
    batch: BulkBatchId,
    handler: &str,
    input: &T,
) -> Value {
    let r =
        f.h.restate
            .invoke(
                &Call::object("BulkInviteBatch", &batch.to_string(), handler),
                Some(&serde_json::to_value(input).unwrap()),
                None,
            )
            .await;
    assert_eq!(r.status, 200, "{r:?}");
    r.body
}

#[tokio::test]
async fn sixty_bulk_email_rows_obey_real_fifty_org_cap_without_link_uses_or_plaintext_journals() {
    let f = Fixture::new(201).await;
    use ghinvite_core::storage::RecordStorage;
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
    let batch_id = BulkBatchId::new();
    let at = Utc::now();
    let organization = OrganizationTarget {
        org_id: 100,
        org_login: "acme".into(),
        grant: MemberGrant::Member,
    };
    let admin = AccountAdmin {
        account_id: 100,
        user_id: 7,
    };
    let query = BatchQuery {
        batch_id,
        admin: admin.clone(),
    };
    let mut emails = Vec::new();
    for row in 1..=60u16 {
        let email_ref = EmailRef::from_bytes([row as u8; 32]);
        let batch = BatchRef { batch_id, row };
        let binding = EmailVaultBinding {
            email_ref,
            batch,
            account_id: 100,
            installation_id: 2,
        };
        let address = ghinvite_email_vault::Address::parse(&format!(
            "BULK60_EMAIL_PRIVACY_MARKER+{row}@example.com"
        ))
        .unwrap();
        f.vault
            .stage(&binding, &address, at.timestamp())
            .await
            .unwrap();
        emails.push(EmailCreateCommand {
            batch,
            account_id: 100,
            installation_id: 2,
            organization: organization.clone(),
            teams: vec![],
            email_ref,
            approved_by: 7,
            approved_at: at,
        });
    }
    let review = Review::new(
        Manifest {
            batch_id,
            admin,
            installation_id: 2,
            organization_installation_id: None,
            targets: Targets {
                organization: Some(organization),
                ..Targets::default()
            },
            access_lifetime_seconds: None,
            entries: emails
                .iter()
                .map(|c| Entry::Email {
                    email_ref: c.email_ref,
                })
                .collect(),
        },
        (1..=60)
            .map(|row| RowEvidence {
                previews: vec![],
                row,
                resolution: Resolution::Email,
                all_targets_satisfied: false,
                observed_at: at,
                next_budget_slot: None,
            })
            .collect(),
        at,
    )
    .unwrap();
    let create = CreateBatch {
        operation_id: operation(),
        review,
    };
    let created = call(&f, batch_id, "create", &create).await;
    assert_eq!(f.posts(), 0, "review must not write upstream");
    let submit = SubmitBatch {
        query: query.clone(),
        operation_id: operation(),
        expected_revision: 1,
        submission: Submission {
            submitted_by: 7,
            requests: vec![],
            emails,
            personal_links: vec![],
            submitted_at: at,
        },
    };
    let accepted = call(&f, batch_id, "submit", &submit).await;
    let progress = tokio::time::timeout(std::time::Duration::from_secs(180),async {
        loop {
            let p:BatchProgress=serde_json::from_value(call(&f,batch_id,"progress",&query).await).unwrap();
            assert_eq!(p.emails.len(),60);
            let written=p.emails.iter().filter(|e| matches!(&e.observation,Observation::Observed(s) if matches!(s.outcome,EmailCreateOutcome::Created{..}))).count();
            assert!(f.posts()<=50,"real org cap must prevent the 51st POST");
            if written == 50 && p.emails.iter().all(|e| match &e.observation {
                Observation::Observed(s) => matches!(s.outcome, EmailCreateOutcome::Created { .. })
                    || matches!(s.outcome, EmailCreateOutcome::Throttled { until, reason:Some(ghinvite_core::budget::RetryReason::Bucket) } if until > at + chrono::Duration::hours(23)),
                _ => false,
            }) { break p; }
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }
    }).await.expect("real paced batch did not reach its 50-write cap");
    assert!(!progress.handoff_pending);
    assert!(progress.requests.is_empty());
    // When GitHub took each row's invitation (the call log's virtual clock
    // ticks with real time).
    let times: Vec<_> =
        f.h.emulator()
            .calls()
            .into_iter()
            .filter(|call| {
                call.method == "POST" && call.path == "/orgs/acme/invitations" && call.status == 201
            })
            .map(|call| call.at)
            .collect();
    assert_eq!(times.len(), 50);
    assert!(
        times
            .last()
            .unwrap()
            .signed_duration_since(times[0])
            .num_seconds()
            >= 48,
        "normal production write spacing remains active"
    );
    let mut throttled = 0;
    for row in &progress.emails {
        let Observation::Observed(snapshot) = &row.observation else {
            panic!("missing owner receipt")
        };
        assert_eq!(
            snapshot.command,
            submit.submission.emails[usize::from(row.source.row) - 1]
        );
        if let EmailCreateOutcome::Throttled { until, reason } = &snapshot.outcome {
            throttled += 1;
            assert_eq!(*reason, Some(ghinvite_core::budget::RetryReason::Bucket));
            assert!(
                *until >= times[0] + chrono::Duration::hours(24) - chrono::Duration::seconds(5)
            );
            assert!(
                *until <= times[0] + chrono::Duration::hours(24) + chrono::Duration::seconds(5)
            );
        }
    }
    assert_eq!(throttled, 10);
    assert_eq!(call(&f, batch_id, "submit", &submit).await, accepted);
    assert_eq!(call(&f, batch_id, "create", &create).await, created);
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    assert_eq!(f.posts(), 50, "exact batch replay must not write again");
    let budget = f.budget().await;
    assert_eq!(budget["buckets"][0]["used_24h"], 50);
    assert_eq!(budget["buckets"][0]["cap"], 50);
    assert_eq!(budget["content_used_hour"], 50);
    let admin = f.h.restate.admin();
    let link_invocations=admin.sql_or_panic("SELECT id FROM sys_invocation WHERE target_service_name IN ('InvitationLink','InvitationRequest','RepositoryDelivery') LIMIT 1").await;
    assert!(
        link_invocations.is_empty(),
        "email bulk creates no request, link or admission use"
    );
    // Scan every retained journal row, including one-way sends and notifications.
    // Overflow fails closed; no first-N sample is accepted as privacy evidence.
    // Retain complete coverage without materializing tens of thousands of raw
    // hex/JSON rows at once. Stable counts refuse concurrent growth or truncation.
    let journal_rows = tokio::time::timeout(std::time::Duration::from_secs(120), async {
        let count = admin
            .sql_or_panic("SELECT COUNT(*) AS total FROM sys_journal")
            .await;
        let expected = count[0]["total"].as_u64().expect("journal count");
        assert!(
            expected > 0 && expected <= 100000,
            "journal scan incomplete"
        );
        let mut scanned = 0u64;
        let mut cursor: Option<(String, u64)> = None;
        while scanned < expected {
            // OFFSET makes DataFusion retain all preceding wide journal payloads
            // in TopK. Bound each sort to one page using the unique journal key.
            let after = cursor
                .as_ref()
                .map(|(id, index)| {
                    let id = id.replace('\'', "''");
                    format!(" WHERE id > '{id}' OR (id = '{id}' AND index > {index})")
                })
                .unwrap_or_default();
            let rows = admin
                .sql_or_panic(&format!(
                    "SELECT id,index,raw FROM sys_journal{after} ORDER BY id,index LIMIT 1000"
                ))
                .await;
            assert!(
                !rows.is_empty() && rows.len() <= 1000,
                "journal page incomplete"
            );
            scanned += rows.len() as u64;
            assert!(scanned <= expected, "journal grew during scan");
            for row in &rows {
                let next = (
                    row["id"].as_str().expect("journal id").to_owned(),
                    row["index"].as_u64().expect("journal index"),
                );
                assert!(
                    cursor.as_ref().is_none_or(|previous| previous < &next),
                    "journal keys must strictly advance without repeated rows"
                );
                cursor = Some(next);
                let hex = row["raw"]
                    .as_str()
                    .expect("raw journal bytes must be present");
                assert!(hex.len().is_multiple_of(2));
                let bytes: Vec<u8> = hex
                    .as_bytes()
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|pair| {
                        u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16)
                            .expect("valid journal hex")
                    })
                    .collect();
                for marker in ["BULK60_EMAIL_PRIVACY_MARKER", "ghs_"] {
                    assert!(!bytes.windows(marker.len()).any(|s| s == marker.as_bytes()));
                }
            }
        }
        let count = admin
            .sql_or_panic("SELECT COUNT(*) AS total FROM sys_journal")
            .await;
        assert_eq!(
            count[0]["total"].as_u64(),
            Some(expected),
            "journal changed during scan"
        );
        assert_eq!(scanned, expected);
        scanned
    })
    .await
    .expect("complete journal privacy scan timed out");
    fn scan(value: &Value) {
        match value {
            Value::String(s) => {
                for marker in ["BULK60_EMAIL_PRIVACY_MARKER", "ghs_"] {
                    assert!(!s.contains(marker));
                    if !s.is_empty()
                        && s.len().is_multiple_of(2)
                        && s.bytes().all(|b| b.is_ascii_hexdigit())
                    {
                        let decoded: Vec<u8> = s
                            .as_bytes()
                            .as_chunks::<2>()
                            .0
                            .iter()
                            .map(|p| {
                                u8::from_str_radix(std::str::from_utf8(p).unwrap(), 16).unwrap()
                            })
                            .collect();
                        assert!(
                            !decoded
                                .windows(marker.len())
                                .any(|b| b == marker.as_bytes())
                        );
                    }
                }
            }
            Value::Array(values) => values.iter().for_each(scan),
            Value::Object(values) => values.values().for_each(scan),
            _ => {}
        }
    }
    fn check_linkless_reservations(value: &Value, count: &mut usize) {
        match value {
            Value::Object(fields) => {
                if fields
                    .get("request_key")
                    .and_then(Value::as_str)
                    .is_some_and(|s| s.starts_with("email:"))
                {
                    assert!(
                        fields.get("link_id").is_some_and(Value::is_null),
                        "email reservation must have no link attribution"
                    );
                    *count += 1;
                }
                for child in fields.values() {
                    check_linkless_reservations(child, count);
                }
            }
            Value::Array(values) => {
                for child in values {
                    check_linkless_reservations(child, count);
                }
            }
            Value::String(s)
                if !s.is_empty()
                    && s.len().is_multiple_of(2)
                    && s.bytes().all(|b| b.is_ascii_hexdigit()) =>
            {
                let bytes: Vec<u8> = s
                    .as_bytes()
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|p| u8::from_str_radix(std::str::from_utf8(p).unwrap(), 16).unwrap())
                    .collect();
                if let Ok(decoded) = serde_json::from_slice::<Value>(&bytes) {
                    check_linkless_reservations(&decoded, count);
                }
            }
            _ => {}
        }
    }
    let (state_rows, reservations) = tokio::time::timeout(std::time::Duration::from_secs(120), async {
        let count = admin.sql_or_panic("SELECT COUNT(*) AS total FROM state").await;
        let expected = count[0]["total"].as_u64().expect("state count");
        assert!(expected > 0 && expected <= 100000, "state scan incomplete");
        let mut scanned = 0u64;
        let mut reservations = 0;
        while scanned < expected {
            let states = admin.sql_or_panic(&format!(
                "SELECT * FROM state ORDER BY partition_key,service_name,service_key,key LIMIT 1000 OFFSET {scanned}"
            )).await;
            assert!(!states.is_empty() && states.len() <= 1000, "state page incomplete");
            scanned += states.len() as u64;
            assert!(scanned <= expected, "state grew during scan");
            for state in &states {
                scan(state);
                check_linkless_reservations(state, &mut reservations);
            }
        }
        let count = admin.sql_or_panic("SELECT COUNT(*) AS total FROM state").await;
        assert_eq!(count[0]["total"].as_u64(), Some(expected), "state changed during scan");
        assert_eq!(scanned, expected);
        (scanned, reservations)
    }).await.expect("complete state privacy scan timed out");
    assert!(
        reservations >= 60,
        "every batch row must retain a real unlinked budget reservation"
    );
    eprintln!(
        "AC-BULK1: 60 retained email rows, 50 real paced POSTs, 10 bucket throttles; {journal_rows} complete journal rows and {state_rows} state rows scanned"
    );
}

#[tokio::test]
async fn refused_email_budget_reservation_advances_after_real_deadline() {
    let f = Fixture::new(201).await;
    let command = f.command(true).await;
    let until = Utc::now() + chrono::Duration::seconds(3);
    let closure =
        f.h.restate
            .invoke(
                &Call::object("InviteBudget", "org:100", "record_limit"),
                Some(&json!({"bucket":{"kind":"org"},"until":until})),
                None,
            )
            .await;
    assert_eq!(closure.status, 200, "{closure:?}");
    let first = f.create(&command).await;
    assert_eq!(first["outcome"]["kind"], "throttled");
    assert_eq!(f.posts(), 0);
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let r =
                f.h.restate
                    .invoke(
                        &Call::object("EmailInviteDelivery", &command.delivery_key(), "status"),
                        None::<&Value>,
                        None,
                    )
                    .await;
            assert_eq!(r.status, 200);
            if r.body["outcome"]["kind"] == "created" {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("a fresh reservation must replace the expired refusal without external retries");
    assert_eq!(f.posts(), 1);
    assert_eq!(f.budget().await["content_used_hour"], 1);
}
