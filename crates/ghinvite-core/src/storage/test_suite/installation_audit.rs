use super::super::AuditPosition;
use super::*;
use crate::{
    account::{AccountInstallationRef, AccountSettings, App, Installation},
    installation_audit::*,
};

/// Full installation projection and immutable audit append are one transaction.
pub async fn scenario_installation_transition_audit<S: Storage>(s: S) {
    let mut installation = Installation::repo(sample_account(701, 9027, "audit-installation"));
    installation.installed_by_user_id = Some(701);
    installation.installed_by_login = Some("installer".into());
    let owner = AccountInstallationRef {
        app: App::Repo,
        account_id: 9027,
    };
    let at = dt("2026-05-05T12:00:00Z");
    let event = AuditEvent {
        id: AuditEventId::new(),
        account_id: 9027,
        occurred_at: at,
        event_type: EventType::InstallationCreated,
        actor_kind: ActorKind::Github,
        actor_id: None,
        target_kind: TargetKind::Installation,
        target_id: "repo:701".into(),
        metadata: serde_json::json!({"app":"repo"}),
        request_id: Some("legacy-invocation".into()),
    };
    // A projection retained before this writer existed must replay its exact event.
    s.put_app_installation(&installation).await.unwrap();
    s.audit(&event).await.unwrap();
    let original = InstallationChange {
        installation: Some(installation.clone()),
        settings: None,
        event: Some(event.clone()),
        audit_facts: vec![],
    };
    let old_wire = serde_json::to_value(&original).unwrap();
    assert!(old_wire.get("audit_facts").is_none());
    assert_eq!(
        serde_json::from_value::<InstallationChange>(old_wire).unwrap(),
        original
    );
    s.project_installation_change(owner, &original)
        .await
        .unwrap();
    installation.revision = 2;
    installation.suspended_at = Some(at);
    let suspended = InstallationChange {
        installation: Some(installation.clone()),
        settings: None,
        event: None,
        audit_facts: vec![InstallationAuditFact::V1 {
            revision: 2,
            fact: InstallationAuditKind::Suspended { suspended_at: at },
        }],
    };
    let suspended_event = suspended.events(owner).unwrap().remove(0);
    s.project_installation_change(owner, &suspended)
        .await
        .unwrap();
    s.project_installation_change(owner, &suspended)
        .await
        .unwrap();
    installation.revision = 4;
    installation.suspended_at = None;
    let accepted = InstallationChange {
        installation: Some(installation.clone()),
        settings: None,
        event: None,
        audit_facts: vec![InstallationAuditFact::V1 {
            revision: 4,
            fact: InstallationAuditKind::PermissionsAccepted {
                source_updated_at: at + chrono::Duration::minutes(2),
                payload_sha256: "a".repeat(64),
                sender_id: 701,
            },
        }],
    };
    s.project_installation_change(owner, &accepted)
        .await
        .unwrap();
    // A late historical fact is appended even though its snapshot is stale.
    let mut late = suspended.clone();
    late.installation.as_mut().unwrap().revision = 3;
    late.audit_facts[0] = InstallationAuditFact::V1 {
        revision: 3,
        fact: InstallationAuditKind::Suspended { suspended_at: at },
    };
    s.project_installation_change(owner, &late).await.unwrap();
    s.project_installation_change(owner, &original)
        .await
        .unwrap();
    assert_eq!(
        s.app_installation(App::Repo, 701).await.unwrap(),
        Some(installation.clone())
    );
    let events = s
        .list_audit_events(9027, None, AuditPosition::Latest)
        .await
        .unwrap()
        .events;
    assert_eq!(events.len(), 4);
    assert!(events.contains(&event));
    assert!(events.contains(&suspended_event));
    assert!(
        events
            .iter()
            .filter(|v| v.id != event.id)
            .all(|v| v.request_id.is_none())
    );
    // Changed fact content at the same semantic ID cannot rewrite history.
    let mut conflict = accepted.clone();
    if let InstallationAuditFact::V1 {
        fact: InstallationAuditKind::PermissionsAccepted { payload_sha256, .. },
        ..
    } = &mut conflict.audit_facts[0]
    {
        *payload_sha256 = "b".repeat(64);
    }
    assert!(
        s.project_installation_change(owner, &conflict)
            .await
            .is_err()
    );
    // A conflicting legacy event rolls back both installation and settings.
    let mut next = original.clone();
    installation.revision = 5;
    installation.account.account_login = "renamed".into();
    next.installation = Some(installation);
    next.settings = Some(AccountSettings {
        account_id: 9027,
        revision: 1,
        repo_admin_delegation: true,
        changed_by: 701,
        changed_at: at,
    });
    next.event.as_mut().unwrap().metadata = serde_json::json!({"changed":true});
    assert!(s.project_installation_change(owner, &next).await.is_err());
    assert_eq!(
        s.app_installation(App::Repo, 701).await.unwrap(),
        accepted.installation
    );
    assert!(s.account_settings(9027).await.unwrap().is_none());
    let mut org = accepted.clone();
    org.installation.as_mut().unwrap().app = App::Org;
    let org_owner = AccountInstallationRef {
        app: App::Org,
        ..owner
    };
    s.project_installation_change(org_owner, &org)
        .await
        .unwrap();
    assert_ne!(
        accepted.events(owner).unwrap()[0].id,
        org.events(org_owner).unwrap()[0].id
    );
    assert!(s.project_installation_change(owner, &org).await.is_err());
    let mut malformed = suspended;
    malformed.installation.as_mut().unwrap().suspended_at = None;
    assert!(
        s.project_installation_change(owner, &malformed)
            .await
            .is_err()
    );
    assert_eq!(
        s.list_audit_events(9027, None, AuditPosition::Latest)
            .await
            .unwrap()
            .events
            .len(),
        5
    );
}
