//! Audit visibility uses the complete retained source scope, before paging.
use super::*;
use crate::{
    access_grant::{AccessGrantSnapshot, RemovalOutcome, RemovalReason, accepted_repository},
    bulk::{
        BatchRef, BulkBatchId, Entry, Manifest, Resolution, Review, RowEvidence, RowRequest,
        Submission,
    },
    delivery::{CreateCommand, CreateOutcome, CreateReceipt, DeliverySnapshot, Settlement},
    storage::{
        access_grants::AccessGrantStorage,
        bulk::{BulkAudit, BulkStorage, Projection},
        console_scope::ConsoleScope,
    },
    targets::{RepoRole, RepoTarget, Targets},
};

pub async fn scenario_bulk_delegated_audit<
    S: Storage + ProjectionStorage + BulkStorage + AccessGrantStorage,
>(
    s: S,
) {
    let at = dt("2026-10-17T10:00:00Z");
    s.put_app_installation(&crate::account::Installation::repo(
        (sample_account(1, 100, "acme")).clone(),
    ))
    .await
    .unwrap();
    s.put_app_installation(&sample_org_installation(2, 100, "acme"))
        .await
        .unwrap();
    for (id, login) in [(7, "creator"), (8, "submitter"), (91, "alice")] {
        s.upsert_user(&sample_user(id, login)).await.unwrap();
    }
    let manifest = Manifest {
        batch_id: BulkBatchId::new(),
        admin: crate::invitation_link::AccountAdmin {
            account_id: 100,
            user_id: 7,
        },
        installation_id: 1,
        organization_installation_id: None,
        access_lifetime_seconds: Some(3600),
        targets: Targets {
            organization: None,
            teams: vec![],
            repos: vec![
                RepoTarget {
                    repo_id: 10,
                    repo_full_name: "acme/api".into(),
                    grant: RepoRole::Write,
                },
                RepoTarget {
                    repo_id: 11,
                    repo_full_name: "acme/web".into(),
                    grant: RepoRole::Read,
                },
            ],
        },
        entries: vec![Entry::Handle {
            login: "alice".into(),
        }],
    };
    let reviewed = |manifest, at| {
        Review::new(
            manifest,
            vec![RowEvidence {
                row: 1,
                resolution: Resolution::Found {
                    user_id: 91,
                    login: "alice".into(),
                },
                previews: vec![],
                all_targets_satisfied: false,
                observed_at: at,
                next_budget_slot: None,
            }],
            at,
        )
        .unwrap()
    };
    let created = |review: Review| Projection {
        events: vec![BulkAudit {
            operation_id: RequestId::new().to_string().try_into().unwrap(),
            actor: review.manifest.admin.clone(),
            at: review.reviewed_at,
            revision: 1,
            kind: EventType::BulkBatchCreated,
            removed_rows: Default::default(),
        }],
        review,
    };
    let mut review = reviewed(manifest.clone(), at);
    s.project_bulk_projection(&created(review.clone()))
        .await
        .unwrap();
    let request_id = RequestId::new();
    let source = BatchRef {
        batch_id: manifest.batch_id,
        row: 1,
    };
    review
        .submit(
            1,
            Submission {
                submitted_by: 8,
                requests: vec![RowRequest {
                    source,
                    request_id,
                    requester_id: 91,
                    requester_login: "alice".into(),
                }],
                emails: vec![],
                personal_links: vec![],
                submitted_at: at,
            },
        )
        .unwrap();
    s.project_bulk_review(&review).await.unwrap();
    let request = crate::request_lifecycle::RequestSnapshot {
        link_id: None,
        batch: Some(source),
        domain_evidence: None,
        request_id,
        account_id: 100,
        requester_id: 91,
        justification: None,
        state: RequestState::Approved,
        admitted_at: at,
        decision_deadline: None,
        revision: 1,
        decision: Some(crate::request_lifecycle::TerminalDecision {
            decision_id: format!("request.approved/{request_id}"),
            decided_by: Some(8),
            effective_at: at,
            evaluated_at: at,
            decline_reason: None,
            requester_message: None,
            cancellation_reason: None,
        }),
    };
    s.apply_request(&crate::storage::projection::RequestProjectionEnvelope {
        request: request.clone(),
        approval: None,
        events: vec![],
    })
    .await
    .unwrap();
    s.audit(&AuditEvent {
        id: AuditEventId::new(),
        account_id: 100,
        occurred_at: at,
        event_type: EventType::RequestApproved,
        actor_kind: ActorKind::User,
        actor_id: Some(8),
        target_kind: TargetKind::InvitationRequest,
        target_id: request_id.to_string(),
        metadata: serde_json::json!({"source":"batch"}),
        request_id: Some(request_id.to_string()),
    })
    .await
    .unwrap();
    let command = CreateCommand {
        budget_attribution: Default::default(),
        access_lifetime_seconds: Some(3600),
        budget_share_pct: 50,
        invitation_id: Some(GithubInvitationId::new()),
        delivery_generation: 1,
        link_id: None,
        batch: Some(source),
        request_id,
        approval_id: request.decision.as_ref().unwrap().decision_id.clone(),
        account_id: 100,
        installation_id: 1,
        requester_id: 91,
        repo_id: 10,
        repo_full_name: "acme/api".into(),
        permission: Permission::Push,
        approved_at: at,
    };
    let mut delivery = DeliverySnapshot::from(CreateReceipt {
        audit_version: crate::delivery_audit::DeliveryAuditVersion::CanonicalV1,
        command,
        outcome: CreateOutcome::Created { upstream_id: 900 },
        revision: 2,
        confirmed_at: Some(at),
    });
    delivery.revision = 3;
    delivery.settlement = Some(Settlement {
        state: InvitationState::Accepted,
        event: AuditEvent {
            id: AuditEventId::new(),
            account_id: 100,
            occurred_at: at,
            event_type: EventType::InvitationAccepted,
            actor_kind: ActorKind::Github,
            actor_id: None,
            target_kind: TargetKind::GithubInvitation,
            target_id: delivery
                .create
                .command
                .invitation_id
                .expect("legacy fixture identity")
                .to_string(),
            metadata: serde_json::Value::Null,
            request_id: Some(request_id.to_string()),
        },
    });
    s.project_delivery(&delivery).await.unwrap();
    // Legacy ULID events must match their explicitly retained identity. An
    // unrelated target cannot borrow request_id or metadata to obtain scope,
    // even when it falls beside valid history. Historical physical-key events
    // remain separately bound to the exact stored delivery ID.
    for seconds in [-1, 1] {
        s.audit(&AuditEvent {
            id: AuditEventId::new(),
            account_id: 100,
            occurred_at: at + chrono::Duration::seconds(seconds),
            event_type: EventType::InvitationAccepted,
            actor_kind: ActorKind::Github,
            actor_id: None,
            target_kind: TargetKind::GithubInvitation,
            target_id: format!("unknown:{}", delivery.create.command.delivery_key()),
            metadata: serde_json::Value::Null,
            request_id: Some(request_id.to_string()),
        })
        .await
        .unwrap();
    }
    let grant = accepted_repository(&delivery).unwrap().unwrap();
    let removed = AccessGrantSnapshot {
        command: grant.clone(),
        revision: 1,
        outcome: RemovalOutcome::Removed {
            reason: RemovalReason::AccessExpiry,
            at: at + chrono::Duration::hours(1),
        },
    };
    s.project_access_grant(&removed).await.unwrap();

    // Real immutable creation records provide more than one page. Mixed target
    // batches on both sides of the visible time range must not affect probes.
    let mut visible_created = vec![manifest.batch_id];
    for index in 0..34 {
        let mut next = manifest.clone();
        next.batch_id = BulkBatchId::new();
        if index == 0 || index == 33 {
            next.organization_installation_id = Some(2);
            next.targets.organization = Some(crate::targets::OrganizationTarget {
                org_id: 100,
                org_login: "acme".into(),
                grant: crate::targets::MemberGrant::Member,
            });
        } else {
            visible_created.push(next.batch_id);
        }
        let clock = at + chrono::Duration::seconds(i64::from(index) - 1);
        s.project_bulk_projection(&created(reviewed(next, clock)))
            .await
            .unwrap();
    }
    assert!(
        s.list_invitation_links_for_account(100)
            .await
            .unwrap()
            .is_empty()
    );
    let first = s.distinct_console_repo_targets(100, None, 1).await.unwrap();
    assert_eq!(
        first.rows.iter().map(|r| r.id).collect::<Vec<_>>(),
        vec![10]
    );
    let second = s
        .distinct_console_repo_targets(100, first.next, 1)
        .await
        .unwrap();
    assert_eq!(
        second.rows.iter().map(|r| r.id).collect::<Vec<_>>(),
        vec![11]
    );
    assert_eq!(second.next, None);
    assert!(
        s.distinct_console_repo_targets(101, None, 501)
            .await
            .unwrap()
            .rows
            .is_empty()
    );

    let full = ConsoleScope::repo_admin(100, [10, 11]).unwrap();
    let first = s
        .scoped_list_audit_events(&full, Some(EventType::BulkBatchCreated), Default::default())
        .await
        .unwrap();
    assert_eq!(first.events.len(), 25);
    assert!(first.has_older);
    assert!(!first.has_newer);
    let oldest = *first.events.last().map(|e| e.id).as_ref().unwrap();
    let boundary = crate::storage::AuditBoundary::from(first.events.last().unwrap());
    let second = s
        .scoped_list_audit_events(
            &full,
            Some(EventType::BulkBatchCreated),
            crate::storage::AuditPosition::Before(boundary),
        )
        .await
        .unwrap();
    assert_eq!(second.events.len(), 8);
    assert!(!second.has_older);
    assert!(second.has_newer);
    let mut actual = first
        .events
        .iter()
        .chain(second.events.iter())
        .map(|e| e.target_id.clone())
        .collect::<Vec<_>>();
    actual.sort();
    let mut expected = visible_created
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    expected.sort();
    assert_eq!(actual, expected);
    let forward = s
        .scoped_list_audit_events(
            &full,
            Some(EventType::BulkBatchCreated),
            crate::storage::AuditPosition::After(boundary),
        )
        .await
        .unwrap();
    assert!(forward.events.iter().all(|e| e.id != oldest));
    assert!(!forward.has_newer);
    for kind in [
        EventType::RequestApproved,
        EventType::InvitationAccepted,
        EventType::AccessRemoved,
    ] {
        let page = s
            .scoped_list_audit_events(&full, Some(kind), Default::default())
            .await
            .unwrap();
        assert_eq!(page.events.len(), 1, "missing batch-scoped {kind:?}");
        assert_eq!(page.events[0].request_id, Some(request_id.to_string()));
        assert!(!page.has_older);
        assert!(!page.has_newer);
    }
    for denied in [
        ConsoleScope::repo_admin(100, [10]).unwrap(),
        ConsoleScope::repo_admin(100, []).unwrap(),
        ConsoleScope::repo_admin(101, [10, 11]).unwrap(),
    ] {
        let page = s
            .scoped_list_audit_events(&denied, None, Default::default())
            .await
            .unwrap();
        assert!(page.events.is_empty());
        assert!(!page.has_older);
        assert!(!page.has_newer);
        let page = s
            .scoped_list_audit_events(
                &denied,
                Some(EventType::BulkBatchCreated),
                crate::storage::AuditPosition::After(boundary),
            )
            .await
            .unwrap();
        assert!(page.events.is_empty());
        assert!(!page.has_older);
        assert!(!page.has_newer);
    }
    assert_eq!(
        s.get_access_grant(&grant.storage_id()).await.unwrap(),
        Some(removed)
    );
    assert_eq!(
        s.list_delivery_for_request(request_id).await.unwrap(),
        vec![delivery]
    );
}
