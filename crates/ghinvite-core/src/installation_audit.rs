//! Versioned verified installation facts and compatible retained projections.
use crate::{
    account::{AccountInstallationRef, AccountSettings, Installation},
    audit::{ActorKind, AuditEvent, EventType, TargetKind},
};
use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum InstallationAuditKind {
    Suspended {
        suspended_at: DateTime<Utc>,
    },
    PermissionsAccepted {
        source_updated_at: DateTime<Utc>,
        payload_sha256: String,
        sender_id: u64,
    },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "version", rename_all = "snake_case", deny_unknown_fields)]
pub enum InstallationAuditFact {
    V1 {
        revision: u64,
        fact: InstallationAuditKind,
    },
}
impl InstallationAuditFact {
    pub fn event(&self, installation: &Installation) -> Result<AuditEvent, &'static str> {
        crate::storage::integer::validate(installation)
            .map_err(|_| "invalid installation audit integers")?;
        let Self::V1 { revision, fact } = self;
        if *revision == 0 || *revision != installation.revision {
            return Err("installation audit revision mismatch");
        }
        let (kind, phase, at, actor, actor_id, details) = match fact {
            InstallationAuditKind::Suspended { suspended_at } => {
                if installation.suspended_at != Some(*suspended_at) {
                    return Err("installation suspension evidence mismatch");
                }
                (
                    EventType::InstallationSuspended,
                    "suspended",
                    *suspended_at,
                    ActorKind::Github,
                    None,
                    serde_json::json!({"suspended_at":suspended_at}),
                )
            }
            InstallationAuditKind::PermissionsAccepted {
                source_updated_at,
                payload_sha256,
                sender_id,
            } => {
                if *sender_id == 0
                    || *sender_id > crate::storage::integer::MAX_EXACT
                    || installation.installed_by_user_id != Some(*sender_id)
                    || payload_sha256.len() != 64
                    || !payload_sha256
                        .bytes()
                        .all(|v| v.is_ascii_digit() || (b'a'..=b'f').contains(&v))
                {
                    return Err("installation permission evidence mismatch");
                }
                (
                    EventType::InstallationPermissionsAccepted,
                    "permissions_accepted",
                    *source_updated_at,
                    ActorKind::User,
                    Some(*sender_id),
                    serde_json::json!({"source_updated_at":source_updated_at,"payload_sha256":payload_sha256}),
                )
            }
        };
        if at < installation.account.installed_at {
            return Err("installation audit time precedes installation");
        }
        let target = format!(
            "{}:{}",
            installation.app.as_str(),
            installation.account.installation_id
        );
        let digest = Sha256::digest(format!(
            "ghinvite/installation/audit/v1/{target}/{revision}/{phase}"
        ));
        Ok(AuditEvent {
            id: crate::AuditEventId::from_ulid(ulid::Ulid::from_bytes(
                digest[..16].try_into().expect("digest length"),
            )),
            account_id: installation.account.account_id,
            occurred_at: at,
            event_type: kind,
            actor_kind: actor,
            actor_id,
            target_kind: TargetKind::Installation,
            target_id: target,
            metadata: serde_json::json!({"app":installation.app,"revision":revision,"source":details}),
            request_id: None,
        })
    }
}

/// Existing event-only JSON is preserved; new facts need no synthetic legacy event.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct InstallationChange {
    pub installation: Option<Installation>,
    pub settings: Option<AccountSettings>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<serde_json::Value>")]
    pub event: Option<AuditEvent>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub audit_facts: Vec<InstallationAuditFact>,
}
impl InstallationChange {
    pub fn events(&self, owner: AccountInstallationRef) -> Result<Vec<AuditEvent>, &'static str> {
        owner.key()?;
        if self
            .installation
            .as_ref()
            .is_some_and(|v| v.app != owner.app || v.account.account_id != owner.account_id)
            || self.settings.as_ref().is_some_and(|v| {
                v.account_id != owner.account_id || owner.app != crate::account::App::Repo
            })
            || self.audit_facts.len() > 8
        {
            return Err("installation projection identity mismatch");
        }
        let mut events = vec![];
        if let Some(event) = &self.event {
            if event.account_id != owner.account_id
                || event.target_kind != TargetKind::Installation
                || !matches!(
                    event.event_type,
                    EventType::InstallationCreated
                        | EventType::InstallationReposChanged
                        | EventType::InstallationUninstalled
                        | EventType::InstallationOwnerRecorded
                        | EventType::RepositoryAdminDelegationChanged
                        | EventType::OrganizationInviteCapChanged
                )
            {
                return Err("invalid legacy installation audit");
            }
            events.push(event.clone());
        }
        for fact in &self.audit_facts {
            events.push(
                fact.event(
                    self.installation
                        .as_ref()
                        .ok_or("installation audit snapshot missing")?,
                )?,
            );
        }
        if events.is_empty() {
            return Err("installation projection has no retained events");
        }
        Ok(events)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Account, AccountType, SelectedRepos, account::App};
    fn installation() -> Installation {
        Installation {
            app: App::Repo,
            revision: 2,
            account: Account {
                installation_id: 5,
                account_id: 10,
                account_login: "acme".into(),
                account_type: AccountType::Organization,
                installed_at: DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
                    .unwrap()
                    .with_timezone(&Utc),
                uninstalled_at: None,
                selected_repos: SelectedRepos::All,
            },
            suspended_at: None,
            permissions: Default::default(),
            installed_by_user_id: Some(7),
            installed_by_login: Some("installer".into()),
        }
    }
    fn permissions(installation: &Installation) -> InstallationAuditFact {
        InstallationAuditFact::V1 {
            revision: installation.revision,
            fact: InstallationAuditKind::PermissionsAccepted {
                source_updated_at: installation.account.installed_at + chrono::Duration::hours(1),
                payload_sha256: "a".repeat(64),
                sender_id: 7,
            },
        }
    }
    #[test]
    fn canonical_facts_bind_app_revision_source_and_actor_without_invocation_identity() {
        let installation = installation();
        let fact = permissions(&installation);
        let event = fact.event(&installation).unwrap();
        assert_eq!(event.event_type, EventType::InstallationPermissionsAccepted);
        assert_eq!(event.actor_id, Some(7));
        assert!(event.request_id.is_none());
        assert_eq!(event, fact.event(&installation).unwrap());
        let mut other = installation.clone();
        other.app = App::Org;
        assert_ne!(event.id, fact.event(&other).unwrap().id);
        other = installation.clone();
        other.revision += 1;
        assert!(fact.event(&other).is_err());
        assert_ne!(event.id, permissions(&other).event(&other).unwrap().id);
        other = installation.clone();
        other.installed_by_user_id = Some(8);
        assert!(fact.event(&other).is_err());
        other = installation;
        other.account.account_id = crate::storage::integer::MAX_EXACT + 1;
        assert!(fact.event(&other).is_err());
    }
    #[test]
    fn malformed_or_unbound_sources_are_refused() {
        let mut installation = installation();
        for hash in [
            "a".repeat(63),
            "A".repeat(64),
            "private upstream text".into(),
        ] {
            let mut fact = permissions(&installation);
            let InstallationAuditFact::V1 {
                fact: InstallationAuditKind::PermissionsAccepted { payload_sha256, .. },
                ..
            } = &mut fact
            else {
                unreachable!()
            };
            *payload_sha256 = hash;
            assert!(fact.event(&installation).is_err());
        }
        let at = installation.account.installed_at - chrono::Duration::seconds(1);
        installation.suspended_at = Some(at);
        assert!(
            InstallationAuditFact::V1 {
                revision: 2,
                fact: InstallationAuditKind::Suspended { suspended_at: at }
            }
            .event(&installation)
            .is_err()
        );
        installation.suspended_at = None;
        assert!(
            InstallationAuditFact::V1 {
                revision: 2,
                fact: InstallationAuditKind::Suspended {
                    suspended_at: installation.account.installed_at
                }
            }
            .event(&installation)
            .is_err()
        );
        assert!(serde_json::from_value::<InstallationAuditFact>(serde_json::json!({"version":"v2","revision":2,"fact":{"kind":"suspended","suspended_at":at}})).is_err());
    }
    #[test]
    fn facts_require_matching_owner_and_cannot_enter_through_legacy_event_slot() {
        let installation = installation();
        let fact = permissions(&installation);
        let event = fact.event(&installation).unwrap();
        let owner = AccountInstallationRef {
            app: App::Repo,
            account_id: 10,
        };
        let mut change = InstallationChange {
            installation: Some(installation),
            settings: None,
            event: Some(event),
            audit_facts: vec![],
        };
        assert!(change.events(owner).is_err());
        change.event = None;
        change.audit_facts.push(fact);
        assert!(change.events(owner).is_ok());
        assert!(
            change
                .events(AccountInstallationRef {
                    app: App::Org,
                    ..owner
                })
                .is_err()
        );
        change.audit_facts = vec![change.audit_facts[0].clone(); 9];
        assert!(change.events(owner).is_err());
    }
}
