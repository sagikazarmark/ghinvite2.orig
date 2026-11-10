//! Immutable repository-only capability for an explicitly selected email row.
use crate::{
    bulk::{Entry, Manifest, PersonalLink, Refusal},
    invitation_link::CreateLink,
    targets::Targets,
};
use chrono::{DateTime, Duration, Utc};

impl PersonalLink {
    /// A reviewed batch may be submitted by another currently authorized admin.
    /// Authorization belongs to the caller; numeric account/actor binding is exact.
    pub fn creation_for_admin(
        &self,
        manifest: &Manifest,
        submitted_at: DateTime<Utc>,
        admin: &crate::invitation_link::AccountAdmin,
    ) -> Result<CreateLink, Refusal> {
        if admin.account_id != manifest.admin.account_id
            || admin.user_id == 0
            || admin.user_id > crate::storage::integer::MAX_EXACT
        {
            return Err(Refusal::InvalidSubmission);
        }
        let mut submitted = manifest.clone();
        submitted.admin = admin.clone();
        self.creation(&submitted, submitted_at)
    }
    /// Derive the command from the accepted manifest and its retained submission
    /// clock. Retrying a handoff never extends the seven-day expiration.
    pub fn creation(
        &self,
        manifest: &Manifest,
        submitted_at: DateTime<Utc>,
    ) -> Result<CreateLink, Refusal> {
        let manifest = manifest.clone().normalize()?;
        self.source
            .validate()
            .map_err(|_| Refusal::InvalidSubmission)?;
        if self.source.batch_id != manifest.batch_id
            || manifest.targets.repos.is_empty()
            || !matches!(
                manifest.entries.get(usize::from(self.source.row) - 1),
                Some(Entry::Email { .. })
            )
        {
            return Err(Refusal::InvalidSubmission);
        }
        let expires_at = submitted_at
            .checked_add_signed(Duration::days(7))
            .ok_or(Refusal::InvalidSubmission)?;
        CreateLink {
            batch_source: Some(self.source),
            access_lifetime_seconds: manifest.access_lifetime_seconds,
            email_domains: Vec::new(),
            link_id: self.link_id,
            admin: manifest.admin.clone(),
            account_id: manifest.admin.account_id,
            installation_id: manifest.installation_id,
            organization_installation_id: None,
            description: format!("Personal invitation · row {}", self.source.row),
            internal_note: None,
            expires_at: Some(expires_at),
            max_uses: Some(1),
            invitation_lifetime_seconds: 604800,
            abuse_threshold_per_hour: None,
            budget_share_pct: 50,
            approval_required: false,
            targets: Targets {
                organization: None,
                teams: Vec::new(),
                repos: manifest.targets.repos,
            },
            secret_sha256: self.secret_sha256,
        }
        .normalized()
        .map_err(|_| Refusal::InvalidSubmission)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        bulk::{BatchRef, BulkBatchId},
        email_delivery::EmailRef,
        invitation_link::AccountAdmin,
        targets::{MemberGrant, OrganizationTarget, RepoRole, RepoTarget},
    };
    fn fixture() -> (Manifest, PersonalLink) {
        let source = BatchRef {
            batch_id: BulkBatchId::new(),
            row: 1,
        };
        (
            Manifest {
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
                        repo_full_name: "acme/repo".into(),
                        grant: RepoRole::Write,
                    }],
                },
                entries: vec![Entry::Email {
                    email_ref: EmailRef::from_bytes([1; 32]),
                }],
            },
            PersonalLink {
                source,
                link_id: crate::InvitationLinkId::new(),
                secret_sha256: crate::SecretHash::from_bytes([2; 32]),
            },
        )
    }
    #[test]
    fn exact_retry_keeps_expiration_and_repository_grants_without_org_or_email() {
        let (manifest, plan) = fixture();
        let now = DateTime::from_timestamp(1800000000, 0).unwrap();
        let command = plan.creation(&manifest, now).unwrap();
        assert_eq!(command, plan.creation(&manifest, now).unwrap());
        assert_eq!(command.expires_at, Some(now + Duration::days(7)));
        assert_eq!(command.targets.repos, manifest.targets.repos);
        assert!(command.targets.organization.is_none());
        assert!(command.targets.teams.is_empty());
        assert_eq!(command.installation_id, 20);
        assert_eq!(command.organization_installation_id, None);
        assert_eq!(command.max_uses, Some(1));
        assert!(!command.approval_required);
        assert_eq!(command.access_lifetime_seconds, Some(3600));
        assert_eq!(command.batch_source, Some(plan.source));
        assert!(
            !serde_json::to_string(&command)
                .unwrap()
                .contains("email_ref")
        );
    }
    #[test]
    fn current_submitting_admin_is_bound_without_changing_targets_or_creator() {
        let (manifest, plan) = fixture();
        let now = DateTime::from_timestamp(1800000000, 0).unwrap();
        let admin = AccountAdmin {
            account_id: 10,
            user_id: 99,
        };
        let command = plan.creation_for_admin(&manifest, now, &admin).unwrap();
        assert_eq!(command.admin, admin);
        assert_eq!(manifest.admin.user_id, 7);
        assert_eq!(command.targets.repos, manifest.targets.repos);
        assert_eq!(command.expires_at, Some(now + Duration::days(7)));
        for bad in [
            AccountAdmin {
                account_id: 11,
                user_id: 99,
            },
            AccountAdmin {
                account_id: 10,
                user_id: 0,
            },
        ] {
            assert!(plan.creation_for_admin(&manifest, now, &bad).is_err());
        }
    }
    #[test]
    fn rejects_different_batch_non_email_rows_and_absent_repository_targets() {
        let (manifest, mut plan) = fixture();
        let now = DateTime::from_timestamp(1800000000, 0).unwrap();
        plan.source.batch_id = BulkBatchId::new();
        assert!(plan.creation(&manifest, now).is_err());
        let (mut manifest, plan) = fixture();
        manifest.entries[0] = Entry::Handle {
            login: "alice".into(),
        };
        assert!(plan.creation(&manifest, now).is_err());
        let (mut manifest, plan) = fixture();
        manifest.targets.repos.clear();
        manifest.installation_id = 30;
        manifest.organization_installation_id = None;
        assert!(plan.creation(&manifest, now).is_err());
    }
}
