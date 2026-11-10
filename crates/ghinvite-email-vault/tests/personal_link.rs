use chrono::{DateTime, Duration};
use ghinvite_core::{
    Account, AccountType, InvitationLinkId, SecretHash, SelectedRepos, User,
    audit::EventType,
    bulk::{BatchRef, BulkBatchId, Entry, Manifest, PersonalLink},
    email_delivery::EmailRef,
    invitation_link::{AccountAdmin, LinkSecretState, LinkSnapshot},
    storage::{
        AppInstallationStorage, RecordStorage,
        projection::{AuditIntent, ProjectionEnvelope, ProjectionStorage},
    },
    targets::{MemberGrant, OrganizationTarget, RepoRole, RepoTarget, Targets},
};
#[tokio::test]
async fn personal_link_projection_preserves_exact_source_guardrails_and_one_safe_audit() {
    let s = ghinvite_storage_sqlx::SqlxStorage::in_memory()
        .await
        .unwrap();
    let at = DateTime::from_timestamp(1800000000, 0).unwrap();
    s.put_app_installation(&ghinvite_core::account::Installation::repo(
        (Account {
            installation_id: 20,
            account_id: 10,
            account_login: "acme".into(),
            account_type: AccountType::Organization,
            installed_at: at,
            uninstalled_at: None,
            selected_repos: SelectedRepos::All,
        })
        .clone(),
    ))
    .await
    .unwrap();
    s.upsert_user(&User {
        user_id: 99,
        login: "submitter".into(),
        avatar_url: None,
        github_created_at: None,
        last_seen_at: at,
    })
    .await
    .unwrap();
    let source = BatchRef {
        batch_id: BulkBatchId::new(),
        row: 1,
    };
    let manifest = Manifest {
        batch_id: source.batch_id,
        admin: AccountAdmin {
            account_id: 10,
            user_id: 7,
        },
        installation_id: 20,
        organization_installation_id: Some(30),
        access_lifetime_seconds: Some(3600),
        targets: Targets {
            organization: Some(OrganizationTarget {
                org_id: 10,
                org_login: "acme".into(),
                grant: MemberGrant::Member,
            }),
            teams: vec![],
            repos: vec![RepoTarget {
                repo_id: 40,
                repo_full_name: "acme/api".into(),
                grant: RepoRole::Write,
            }],
        },
        entries: vec![Entry::Email {
            email_ref: EmailRef::from_bytes([7; 32]),
        }],
    };
    let plan = PersonalLink {
        source,
        link_id: InvitationLinkId::new(),
        secret_sha256: SecretHash::from_bytes([8; 32]),
    };
    let command = plan
        .creation_for_admin(
            &manifest,
            at,
            &AccountAdmin {
                account_id: 10,
                user_id: 99,
            },
        )
        .unwrap();
    let envelope = ProjectionEnvelope {
        transition_id: format!("link/{}/created", plan.link_id),
        link: LinkSnapshot {
            pause: None,
            metadata: None,
            link_id: plan.link_id,
            secret: LinkSecretState {
                secret_generation: 1,
                sha256: command.secret_sha256,
            },
            creation: command,
            created_at: at,
            uses: 0,
            revision: 1,
            revoked_at: None,
            revoked_by: None,
        },
        events: vec![AuditIntent {
            event_id: format!("link/{}/created", plan.link_id),
            kind: EventType::InvitationLinkCreated,
            actor_id: Some(99),
            target_id: plan.link_id.to_string(),
            effective_at: at,
            evaluated_at: at,
        }],
    };
    s.debug_set_audit_failure(true).await.unwrap();
    assert!(s.apply_transition(&envelope).await.is_err());
    assert!(
        s.get_invitation_link_by_id(plan.link_id)
            .await
            .unwrap()
            .is_none()
    );
    s.debug_set_audit_failure(false).await.unwrap();
    for _ in 0..3 {
        s.apply_transition(&envelope).await.unwrap();
    }
    let read = s
        .get_invitation_link_by_id(plan.link_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(read.batch_source, Some(source));
    assert_eq!(read.created_by, 99);
    assert_eq!(read.max_uses, Some(1));
    assert!(!read.approval_required);
    assert_eq!(read.expires_at, Some(at + Duration::days(7)));
    assert_eq!(read.access_lifetime_seconds, Some(3600));
    assert_eq!(read.targets.repos, manifest.targets.repos);
    assert!(read.targets.organization.is_none());
    assert!(read.targets.teams.is_empty());
    let audit = s.debug_list_audit(10).await.unwrap();
    assert_eq!(audit.len(), 1);
    assert_eq!(audit[0].event_type, EventType::InvitationLinkCreated);
    assert_eq!(
        audit[0].metadata,
        serde_json::json!({"source":{"kind":"batch","batch_id":source.batch_id,"row":1}})
    );
    let encoded = serde_json::to_string(&audit).unwrap();
    assert!(!encoded.contains(&plan.secret_sha256.as_hex()));
    assert!(!encoded.contains("email_ref"));
    let mut conflict = envelope.clone();
    conflict.link.revision = 2;
    conflict.link.creation.batch_source.as_mut().unwrap().row = 2;
    assert!(s.apply_transition(&conflict).await.is_err());
    assert_eq!(
        s.get_invitation_link_by_id(plan.link_id).await.unwrap(),
        Some(read)
    );
}
