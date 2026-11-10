use super::*;
use crate::{
    bulk::{
        BatchRef, BulkBatchId, Entry, Manifest, Resolution, Review, RowEvidence, RowRequest,
        Submission,
    },
    storage::bulk::BulkStorage,
    targets::{RepoRole, RepoTarget, Targets},
};
pub async fn scenario_bulk_review<
    S: Storage
        + ProjectionStorage
        + BulkStorage
        + crate::storage::blocked_digest::DailyDigestStorage
        + crate::storage::email_delivery::EmailDeliveryStorage,
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
    s.upsert_user(&sample_user(7, "owner")).await.unwrap();
    s.upsert_user(&sample_user(8, "submitting-admin"))
        .await
        .unwrap();
    s.upsert_user(&sample_user(91, "alice")).await.unwrap();
    let manifest = Manifest {
        batch_id: BulkBatchId::new(),
        admin: crate::invitation_link::AccountAdmin {
            account_id: 100,
            user_id: 7,
        },
        installation_id: 1,
        organization_installation_id: Some(2),
        targets: Targets {
            organization: Some(crate::targets::OrganizationTarget {
                org_id: 100,
                org_login: "acme".into(),
                grant: crate::targets::MemberGrant::Member,
            }),
            teams: vec![crate::targets::TeamTarget {
                team_id: 21,
                team_slug: "developers".into(),
                grant: crate::targets::MemberGrant::Member,
            }],
            repos: vec![RepoTarget {
                repo_id: 10,
                repo_full_name: "acme/api".into(),
                grant: RepoRole::Write,
            }],
        },
        access_lifetime_seconds: None,
        entries: vec![
            Entry::Handle {
                login: "alice".into(),
            },
            Entry::Handle {
                login: "alias".into(),
            },
            Entry::Handle {
                login: "bob".into(),
            },
        ],
    };
    let evidence = |row, id, login: &str| RowEvidence {
        previews: Vec::new(),
        row,
        resolution: Resolution::Found {
            user_id: id,
            login: login.into(),
        },
        all_targets_satisfied: false,
        observed_at: at,
        next_budget_slot: None,
    };
    let mut review = Review::new(
        manifest,
        vec![
            evidence(1, 91, "alice"),
            evidence(2, 91, "alice"),
            evidence(3, 92, "bob"),
        ],
        at,
    )
    .unwrap();
    let original = review.clone();
    let mut missing_parent = original.clone();
    missing_parent.manifest.admin.user_id = 99;
    assert!(matches!(
        s.project_bulk_review(&missing_parent).await,
        Err(super::super::Error::ProjectionDependency)
    ));
    assert!(
        s.bulk_review(100, review.manifest.batch_id)
            .await
            .unwrap()
            .is_none()
    );
    s.project_bulk_review(&review).await.unwrap();
    s.project_bulk_review(&review).await.unwrap();
    let created = crate::storage::bulk::Projection {
        review: review.clone(),
        events: vec![crate::storage::bulk::BulkAudit {
            operation_id: RequestId::new().to_string().try_into().unwrap(),
            actor: review.manifest.admin.clone(),
            at,
            revision: 1,
            kind: crate::audit::EventType::BulkBatchCreated,
            removed_rows: Default::default(),
        }],
    };
    s.project_bulk_projection(&created).await.unwrap();
    s.project_bulk_projection(&created).await.unwrap();
    let audits = s
        .list_audit_events(
            100,
            Some(crate::audit::EventType::BulkBatchCreated),
            Default::default(),
        )
        .await
        .unwrap();
    assert_eq!(audits.events.len(), 1);
    assert_eq!(
        audits.events[0].target_kind,
        crate::audit::TargetKind::BulkBatch
    );
    assert_eq!(audits.events[0].actor_id, Some(7));
    assert_eq!(
        audits.events[0].metadata,
        serde_json::json!({"batch_id":review.manifest.batch_id,"revision":1,"rows":3})
    );
    assert_eq!(
        s.bulk_review(101, review.manifest.batch_id).await.unwrap(),
        None
    );
    review
        .edit(1, std::collections::BTreeSet::from([3]), Default::default())
        .unwrap();
    let mut bad_audit = created.clone();
    bad_audit.review = review.clone();
    bad_audit.events[0].at += chrono::Duration::seconds(1);
    assert!(
        matches!(
            s.project_bulk_projection(&bad_audit).await,
            Err(super::super::Error::ProjectionInvariant(_))
        ),
        "changed audit payload rolls back the batch head atomically"
    );
    assert_eq!(
        s.bulk_review(100, review.manifest.batch_id).await.unwrap(),
        Some(original.clone())
    );
    s.project_bulk_review(&review).await.unwrap();
    s.project_bulk_projection(&created).await.unwrap();
    assert_eq!(
        s.list_audit_events(
            100,
            Some(crate::audit::EventType::BulkBatchCreated),
            Default::default()
        )
        .await
        .unwrap(),
        audits,
        "older audit replay is retained without regressing the review"
    );
    s.project_bulk_review(&original).await.unwrap();
    assert_eq!(
        s.bulk_review(100, review.manifest.batch_id).await.unwrap(),
        Some(review.clone())
    );
    let mut conflict = review.clone();
    conflict.removed.insert(1);
    assert!(matches!(
        s.project_bulk_review(&conflict).await,
        Err(super::super::Error::ProjectionInvariant(_))
    ));
    let request_id = RequestId::new();
    let source = BatchRef {
        batch_id: review.manifest.batch_id,
        row: 1,
    };
    review
        .submit(
            2,
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
    assert_eq!(
        review.revision, 3,
        "submission gets a distinct projection revision"
    );
    let snapshot = crate::request_lifecycle::RequestSnapshot {
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
    let envelope = crate::storage::projection::RequestProjectionEnvelope {
        request: snapshot.clone(),
        events: vec![],
        approval: None,
    };
    assert!(
        matches!(
            s.apply_request(&envelope).await,
            Err(super::super::Error::ProjectionDependency)
        ),
        "unsubmitted review cannot create a request"
    );
    s.project_bulk_review(&review).await.unwrap();
    s.project_bulk_review(&review).await.unwrap();
    s.apply_request(&envelope).await.unwrap();
    s.apply_request(&envelope).await.unwrap();
    delivery_bindings(&s, &review, &snapshot).await;
    assert_eq!(
        s.get_invitation_request(request_id).await.unwrap(),
        Some(snapshot.as_request())
    );
    assert!(
        s.list_invitation_links_for_account(100)
            .await
            .unwrap()
            .is_empty(),
        "batch request has no synthetic link or link use"
    );
    let mut wrong = envelope.clone();
    wrong.request.batch.as_mut().unwrap().row = 2;
    assert!(
        matches!(
            s.apply_request(&wrong).await,
            Err(super::super::Error::ProjectionInvariant(_))
        ),
        "duplicate/unissued row cannot adopt another row's request"
    );
    let mut wrong = envelope.clone();
    wrong.request.account_id = 101;
    assert!(matches!(
        s.apply_request(&wrong).await,
        Err(super::super::Error::ProjectionInvariant(_))
    ));
    let mut wrong = envelope.clone();
    wrong.request.decision.as_mut().unwrap().decided_by = Some(7);
    assert!(
        matches!(
            s.apply_request(&wrong).await,
            Err(super::super::Error::ProjectionInvariant(_))
        ),
        "creator is not substituted for the retained submitting administrator"
    );
    assert_eq!(
        s.get_invitation_request(request_id).await.unwrap(),
        Some(snapshot.as_request())
    );
    s.project_bulk_review(&original).await.unwrap();
    assert_eq!(
        s.bulk_review(100, review.manifest.batch_id).await.unwrap(),
        Some(review.clone())
    );
    let mut changed = review.clone();
    changed.revision += 1;
    changed.submitted.as_mut().unwrap().requests[0].request_id = RequestId::new();
    assert!(matches!(
        s.project_bulk_review(&changed).await,
        Err(super::super::Error::ProjectionInvariant(_))
    ));
    // A request collision must roll back both the new batch and its row set.
    let mut collision = review.clone();
    collision.manifest.batch_id = BulkBatchId::new();
    collision.submitted.as_mut().unwrap().requests[0]
        .source
        .batch_id = collision.manifest.batch_id;
    assert!(s.project_bulk_review(&collision).await.is_err());
    assert_eq!(
        s.bulk_review(100, collision.manifest.batch_id)
            .await
            .unwrap(),
        None
    );
    assert_eq!(
        s.bulk_review(100, review.manifest.batch_id).await.unwrap(),
        Some(review)
    );
}

async fn delivery_bindings<
    S: Storage
        + BulkStorage
        + ProjectionStorage
        + crate::storage::blocked_digest::DailyDigestStorage
        + crate::storage::email_delivery::EmailDeliveryStorage,
>(
    s: &S,
    review: &Review,
    request: &crate::request_lifecycle::RequestSnapshot,
) {
    use crate::delivery::{CreateCommand, CreateOutcome, CreateReceipt, DeliverySnapshot};
    use crate::org_delivery::{OrganizationCreateCommand, OrganizationDeliverySnapshot};
    use crate::storage::Error;
    let manifest = &review.manifest;
    let repo = CreateCommand {
        budget_attribution: Default::default(),
        access_lifetime_seconds: manifest.access_lifetime_seconds,
        budget_share_pct: crate::invitation_link::default_budget_share_pct(),
        invitation_id: Some(GithubInvitationId::new()),
        delivery_generation: 1,
        link_id: None,
        batch: request.batch,
        request_id: request.request_id,
        approval_id: request.decision.as_ref().unwrap().decision_id.clone(),
        account_id: request.account_id,
        installation_id: manifest.installation_id,
        requester_id: request.requester_id,
        repo_id: 10,
        repo_full_name: "acme/api".into(),
        permission: Permission::Push,
        approved_at: request.admitted_at,
    };
    let planned = |command| {
        DeliverySnapshot::from(CreateReceipt {
            audit_version: crate::delivery_audit::DeliveryAuditVersion::CanonicalV1,
            command,
            outcome: CreateOutcome::Planned,
            revision: 1,
            confirmed_at: None,
        })
    };
    let mutations: &[fn(&mut CreateCommand)] = &[
        |c| c.batch.as_mut().unwrap().row = 2,
        |c| c.account_id = 101,
        |c| c.requester_id = 92,
        |c| c.installation_id = 2,
        |c| c.repo_id = 11,
        |c| c.repo_full_name = "acme/other".into(),
        |c| c.permission = Permission::Admin,
        |c| c.access_lifetime_seconds = Some(3600),
        |c| c.approval_id = "other-approval".into(),
        |c| c.approved_at += chrono::Duration::seconds(1),
        |c| c.link_id = Some(InvitationLinkId::new()),
    ];
    for mutate in mutations {
        let mut wrong = repo.clone();
        mutate(&mut wrong);
        assert!(
            matches!(
                s.project_delivery(&planned(wrong)).await,
                Err(Error::ProjectionInvariant(_))
            ),
            "a batch row cannot authorize a different repository command"
        );
        assert!(
            s.list_delivery_for_request(request.request_id)
                .await
                .unwrap()
                .is_empty(),
            "failed delivery binding rolls back all rows"
        );
    }
    let expected = planned(repo.clone());
    s.project_delivery(&expected).await.unwrap();
    s.project_delivery(&expected).await.unwrap();
    assert_eq!(
        s.list_delivery_for_request(request.request_id)
            .await
            .unwrap(),
        vec![expected]
    );
    // Reissue changes the delivery generation, preserving its original batch source.
    let mut later = repo;
    later.invitation_id = Some(GithubInvitationId::new());
    later.delivery_generation = 2;
    later.budget_attribution = crate::reissue::BudgetAttribution::AdministratorReissue;
    s.project_delivery(&planned(later)).await.unwrap();

    let org = OrganizationCreateCommand {
        budget_attribution: Default::default(),
        access_lifetime_seconds: manifest.access_lifetime_seconds,
        budget_share_pct: crate::invitation_link::default_budget_share_pct(),
        invitation_id: Some(GithubInvitationId::new()),
        delivery_generation: 1,
        link_id: None,
        batch: request.batch,
        request_id: request.request_id,
        approval_id: request.decision.as_ref().unwrap().decision_id.clone(),
        account_id: request.account_id,
        installation_id: manifest.organization_installation_id.unwrap(),
        requester_id: request.requester_id,
        organization: manifest.targets.organization.clone().unwrap(),
        teams: manifest.targets.teams.clone(),
        approved_at: request.admitted_at,
    };
    let mutations: &[fn(&mut OrganizationCreateCommand)] = &[
        |c| c.batch.as_mut().unwrap().row = 2,
        |c| c.requester_id = 92,
        |c| c.installation_id = 1,
        |c| c.organization.org_login = "other".into(),
        |c| c.teams.clear(),
        |c| c.teams[0].team_id = 22,
        |c| c.teams[0].team_slug = "other".into(),
        |c| c.access_lifetime_seconds = Some(3600),
        |c| c.approval_id = "other-approval".into(),
        |c| c.approved_at += chrono::Duration::seconds(1),
    ];
    for mutate in mutations {
        let mut wrong = org.clone();
        mutate(&mut wrong);
        let wrong = OrganizationDeliverySnapshot::planned(wrong).unwrap();
        assert!(
            matches!(
                s.project_organization_delivery(&wrong).await,
                Err(Error::ProjectionInvariant(_))
            ),
            "the complete organization/team command must match the retained batch"
        );
        assert!(
            s.get_organization_delivery(&org.storage_id())
                .await
                .unwrap()
                .is_none()
        );
    }
    let expected = OrganizationDeliverySnapshot::planned(org.clone()).unwrap();
    s.project_organization_delivery(&expected).await.unwrap();
    s.project_organization_delivery(&expected).await.unwrap();
    assert_eq!(
        s.get_organization_delivery(&org.storage_id())
            .await
            .unwrap(),
        Some(expected)
    );
    operational_bindings(s, review, request).await;
}

/// Linkless owners must contribute independently to monitoring and immutable
/// daily snapshots; neither a missing link nor an email creates a fake parent.
async fn operational_bindings<
    S: Storage
        + BulkStorage
        + ProjectionStorage
        + crate::storage::blocked_digest::DailyDigestStorage
        + crate::storage::email_delivery::EmailDeliveryStorage,
>(
    s: &S,
    review: &Review,
    request: &crate::request_lifecycle::RequestSnapshot,
) {
    use crate::{
        delivery::{BlockedReason, CreateOutcome},
        email_delivery::{EmailBlockedReason, EmailCreateOutcome, EmailDeliverySnapshot},
        org_delivery::{OrganizationBlockedReason, OrganizationCreateOutcome, TeamCreateOutcome},
        storage::{
            console_scope::ConsoleScope,
            operational::{RepositoryTelemetry, SignalKind},
            organization_telemetry::{OrganizationStepTelemetry, OrganizationTelemetryStep},
        },
    };
    let at = request.admitted_at;
    let now = at + chrono::Duration::hours(2);
    let mut repo = s
        .list_delivery_for_request(request.request_id)
        .await
        .unwrap()
        .into_iter()
        .find(|r| r.create.command.delivery_generation == 1)
        .unwrap();
    repo.create.outcome = CreateOutcome::Blocked {
        reason: BlockedReason::InstallationUnavailable,
    };
    repo.create.revision += 1;
    repo.revision += 1;
    s.project_delivery(&repo).await.unwrap();
    let telemetry = RepositoryTelemetry {
        request_id: request.request_id,
        target_id: 10,
        delivery_generation: 1,
        account_id: 100,
        unknown_since: None,
        last_observed_at: Some(at),
    };
    s.project_repository_telemetry(&telemetry).await.unwrap();
    s.project_repository_telemetry(&telemetry).await.unwrap();
    assert!(
        s.project_repository_telemetry(&RepositoryTelemetry {
            account_id: 101,
            ..telemetry
        })
        .await
        .is_err()
    );
    assert_eq!(
        s.list_delivery_for_request(request.request_id)
            .await
            .unwrap()
            .into_iter()
            .find(|r| r.create.command.delivery_generation == 1),
        Some(repo)
    );

    let org_key = format!(
        "org:{}",
        crate::delivery::delivery_key(request.request_id, 100, 1)
    );
    let mut org = s
        .get_organization_delivery(&org_key)
        .await
        .unwrap()
        .unwrap();
    org.create.outcome = OrganizationCreateOutcome::OutcomeUnknown;
    org.create.revision += 1;
    org.revision += 1;
    let team = org.teams.get_mut(&21).unwrap();
    team.create.outcome = TeamCreateOutcome::Blocked {
        reason: OrganizationBlockedReason::IdpSyncedTeam,
    };
    team.create.revision += 1;
    team.revision += 1;
    s.project_organization_delivery(&org).await.unwrap();
    for step in [
        OrganizationTelemetryStep::Organization,
        OrganizationTelemetryStep::Team { team_id: 21 },
    ] {
        let telemetry = OrganizationStepTelemetry {
            request_id: request.request_id,
            org_id: 100,
            delivery_generation: 1,
            account_id: 100,
            step,
            unknown_since: Some(at),
            last_observed_at: Some(at),
        };
        s.project_organization_telemetry(&telemetry).await.unwrap();
        assert!(
            s.project_organization_telemetry(&OrganizationStepTelemetry {
                account_id: 101,
                org_id: 101,
                ..telemetry
            })
            .await
            .is_err()
        );
    }
    assert_eq!(
        s.get_organization_delivery(&org_key).await.unwrap(),
        Some(org)
    );

    // An email receipt has no Request, Link, or fabricated requester. Its raw
    // mailbox and recovery reference are never part of the operational view.
    let email: EmailDeliverySnapshot = serde_json::from_value(serde_json::json!({
        "command": {
            "batch":{"batch_id":BulkBatchId::new(),"row":1},
            "account_id":100,"installation_id":2,
            "organization":review.manifest.targets.organization,
            "teams":[],"email_ref":"ed".repeat(32),"approved_by":7,"approved_at":at
        },
        "revision":1,"outcome":{"kind":"blocked","reason":"email_recovery_unavailable"},
        "confirmed_at":null,"settlement":null,"settled_at":null
    }))
    .unwrap();
    s.project_email_delivery(&email).await.unwrap();
    let report = s.delivery_signals(now).await.unwrap();
    assert_eq!(report.counts.repository_deliveries, 2);
    assert_eq!(report.counts.organization_deliveries, 1);
    assert_eq!(report.counts.team_steps, 1);
    assert_eq!(report.counts.email_deliveries, 1);
    assert_eq!(report.counts.total_steps(), 5);
    assert_eq!(report.counts.unknown.total, 1);
    assert_eq!(report.counts.unknown.over_threshold, 1);
    assert!(!report.incomplete());
    assert_eq!(report.by_account.len(), 1);
    assert_eq!(report.by_account[0].account_id, 100);
    for step in &report.steps {
        assert_eq!(step.link_id, None);
        assert!(step.batch_id.is_some());
        assert!(step.batch_row.is_some());
        assert_eq!(step.request_id.is_none(), step.kind == SignalKind::Email);
    }
    let serialized = serde_json::to_string(&report).unwrap();
    assert!(!serialized.contains(&"ed".repeat(32)));
    assert!(!serialized.contains("email_ref"));

    s.record_blocked_digest(now).await.unwrap();
    let account = ConsoleScope::account(100).unwrap();
    let digest = s
        .scoped_blocked_digest(&account, now.date_naive())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(digest.counts.iter().map(|c| c.count).sum::<u64>(), 3);
    assert!(
        digest
            .counts
            .iter()
            .any(|c| c.kind == SignalKind::Email && c.count == 1)
    );
    assert!(
        s.scoped_blocked_digest(&ConsoleScope::account(101).unwrap(), now.date_naive())
            .await
            .unwrap()
            .unwrap()
            .counts
            .is_empty()
    );
    // A repo delegate cannot see any slice of an organization-bearing batch,
    // nor any email delivery, even when they administer its repository.
    assert!(
        s.scoped_blocked_digest(
            &ConsoleScope::repo_admin(100, [10]).unwrap(),
            now.date_naive()
        )
        .await
        .unwrap()
        .unwrap()
        .counts
        .is_empty()
    );
    let mut changed = email.clone();
    changed.revision += 1;
    changed.outcome = EmailCreateOutcome::Blocked {
        reason: EmailBlockedReason::NoSeat,
    };
    s.project_email_delivery(&changed).await.unwrap();
    s.record_blocked_digest(now + chrono::Duration::minutes(1))
        .await
        .unwrap();
    assert_eq!(
        s.scoped_blocked_digest(&account, now.date_naive())
            .await
            .unwrap(),
        Some(digest)
    );

    // Delegation applies to the complete immutable batch target set, before
    // grouping. A projected single step cannot reveal a partially owned batch.
    let mut manifest = review.manifest.clone();
    manifest.batch_id = BulkBatchId::new();
    manifest.organization_installation_id = None;
    manifest.targets.organization = None;
    manifest.targets.teams.clear();
    manifest.targets.repos.push(RepoTarget {
        repo_id: 11,
        repo_full_name: "acme/web".into(),
        grant: RepoRole::Read,
    });
    manifest.entries = vec![Entry::Handle {
        login: "alice".into(),
    }];
    let mut independent = Review::new(
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
    .unwrap();
    let source = BatchRef {
        batch_id: independent.manifest.batch_id,
        row: 1,
    };
    let independent_request_id = RequestId::new();
    independent
        .submit(
            1,
            Submission {
                submitted_by: 8,
                requests: vec![RowRequest {
                    source,
                    request_id: independent_request_id,
                    requester_id: 91,
                    requester_login: "alice".into(),
                }],
                emails: vec![],
                personal_links: vec![],
                submitted_at: at,
            },
        )
        .unwrap();
    s.project_bulk_review(&independent).await.unwrap();
    let mut independent_request = request.clone();
    independent_request.request_id = independent_request_id;
    independent_request.batch = Some(source);
    independent_request.decision.as_mut().unwrap().decision_id =
        format!("request.approved/{independent_request_id}");
    s.apply_request(&crate::storage::projection::RequestProjectionEnvelope {
        request: independent_request.clone(),
        events: vec![],
        approval: None,
    })
    .await
    .unwrap();
    let mut independent_delivery = s
        .list_delivery_for_request(request.request_id)
        .await
        .unwrap()
        .into_iter()
        .find(|r| r.create.command.delivery_generation == 1)
        .unwrap();
    independent_delivery.create.command.batch = Some(source);
    independent_delivery.create.command.request_id = independent_request_id;
    independent_delivery.create.command.invitation_id = Some(GithubInvitationId::new());
    independent_delivery.create.command.approval_id =
        independent_request.decision.unwrap().decision_id;
    s.project_delivery(&independent_delivery).await.unwrap();
    let tomorrow = now + chrono::Duration::days(1);
    s.record_blocked_digest(tomorrow).await.unwrap();
    let full = s
        .scoped_blocked_digest(
            &ConsoleScope::repo_admin(100, [10, 11]).unwrap(),
            tomorrow.date_naive(),
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(full.counts.len(), 1);
    assert_eq!(full.counts[0].kind, SignalKind::Repo);
    assert_eq!(full.counts[0].count, 1);
    assert!(
        s.scoped_blocked_digest(
            &ConsoleScope::repo_admin(100, [10]).unwrap(),
            tomorrow.date_naive()
        )
        .await
        .unwrap()
        .unwrap()
        .counts
        .is_empty()
    );

    changed.revision += 1;
    changed.outcome = EmailCreateOutcome::OutcomeUnknown;
    changed.unknown_since = Some(at);
    s.project_email_delivery(&changed).await.unwrap();
    let report = s.delivery_signals(now).await.unwrap();
    assert_eq!(report.counts.unknown.total, 2);
    assert_eq!(report.counts.unknown.over_threshold, 2);
    assert_eq!(report.counts.unknown.missing_timestamp, 0);
    assert!(!report.incomplete());
    let mut corrupt = report.steps.clone();
    let email_step = corrupt
        .iter_mut()
        .find(|r| r.kind == SignalKind::Email)
        .unwrap();
    email_step.request_id = Some(request.request_id);
    assert!(crate::storage::operational::report(corrupt, now).is_err());
    let mut corrupt = report.steps;
    let email_step = corrupt
        .iter_mut()
        .find(|r| r.kind == SignalKind::Email)
        .unwrap();
    email_step.batch_row = Some(2);
    assert!(crate::storage::operational::report(corrupt, now).is_err());
}
