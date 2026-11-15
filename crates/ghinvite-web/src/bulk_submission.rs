//! Web-owned, exact submission allocation. Only hashes enter the batch command;
//! personal capabilities remain protected browser-session continuations.
use crate::{
    email_rows::{EmailRowsScope, PreparationError},
    personal_link::PreparedPersonalLink,
};
use chrono::{DateTime, Utc};
use ghinvite_core::{
    bulk::{BatchRef, Resolution, Review, ReviewStatus, RowRequest, Submission},
    bulk_commands::{BatchQuery, SubmitBatch},
    invitation_link::AccountAdmin,
    request_lifecycle::LifecycleOperationId,
};
use serde::{Deserialize, Serialize};

pub struct PreparedSubmission {
    command: SubmitBatch,
    personal: Vec<PreparedPersonalLink>,
}
impl std::fmt::Debug for PreparedSubmission {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreparedSubmission")
            .field("batch_id", &self.command.query.batch_id)
            .field("personal_links", &self.personal.len())
            .finish_non_exhaustive()
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProtectedLink {
    pub plan: ghinvite_core::bulk::PersonalLink,
    pub ciphertext: String,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProtectedSubmission {
    pub command: SubmitBatch,
    pub personal: Vec<ProtectedLink>,
}
impl std::fmt::Debug for ProtectedSubmission {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProtectedSubmission")
            .field("batch_id", &self.command.query.batch_id)
            .field("personal_links", &self.personal.len())
            .finish_non_exhaustive()
    }
}
impl PreparedSubmission {
    /// Call only for a fresh attempt after current full-target authorization.
    /// Retry loads ProtectedSubmission instead of allocating any new identity.
    pub fn new(
        review: &Review,
        admin: AccountAdmin,
        operation_id: LifecycleOperationId,
        at: DateTime<Utc>,
    ) -> Result<Self, ghinvite_core::bulk::Refusal> {
        use ghinvite_core::bulk::Refusal;
        review.validate()?;
        if admin.account_id != review.manifest.admin.account_id {
            return Err(Refusal::InvalidSubmission);
        }
        let mut submission = Submission {
            submitted_by: admin.user_id,
            requests: vec![],
            emails: vec![],
            personal_links: vec![],
            submitted_at: at,
        };
        let mut personal = vec![];
        for row in &review.rows {
            if review.removed.contains(&row.row)
                || matches!(row.status, ReviewStatus::Duplicate { .. })
            {
                continue;
            }
            let source = BatchRef {
                batch_id: review.manifest.batch_id,
                row: row.row,
            };
            match &row.evidence.resolution {
                Resolution::Found { user_id, login } => submission.requests.push(RowRequest {
                    source,
                    request_id: ghinvite_core::RequestId::new(),
                    requester_id: *user_id,
                    requester_login: login.clone(),
                }),
                Resolution::Email => {
                    let organization = review
                        .manifest
                        .targets
                        .organization
                        .clone()
                        .ok_or(Refusal::InvalidSubmission)?;
                    let ghinvite_core::bulk::Entry::Email { email_ref } =
                        review.manifest.entries[usize::from(row.row) - 1]
                    else {
                        return Err(Refusal::InvalidSubmission);
                    };
                    submission
                        .emails
                        .push(ghinvite_core::email_delivery::EmailCreateCommand {
                            batch: source,
                            account_id: admin.account_id,
                            installation_id: review
                                .manifest
                                .org_installation_id()
                                .ok_or(Refusal::InvalidSubmission)?,
                            organization,
                            teams: review.manifest.targets.teams.clone(),
                            email_ref,
                            approved_by: admin.user_id,
                            approved_at: at,
                        });
                    if review.personal_link_rows.contains(&row.row) {
                        let link = PreparedPersonalLink::generate(source)
                            .map_err(|_| Refusal::InvalidSubmission)?;
                        submission.personal_links.push(link.plan().clone());
                        personal.push(link);
                    }
                }
                Resolution::NotFound | Resolution::Unavailable => {
                    return Err(Refusal::UnresolvedRows);
                }
            }
        }
        // This is the same complete-set validator the authoritative owner runs.
        // Nothing has been staged, sent or made visible by allocation.
        let mut expected = review.clone();
        expected.submit(review.revision, submission.clone())?;
        Ok(Self {
            command: SubmitBatch {
                query: BatchQuery {
                    batch_id: review.manifest.batch_id,
                    admin,
                },
                operation_id,
                expected_revision: review.revision,
                submission,
            },
            personal,
        })
    }
    pub fn command(&self) -> &SubmitBatch {
        &self.command
    }
    /// Persist the result before the first ingress call. Nested personal-link
    /// envelopes retain exact original capabilities for an uncertain response.
    pub fn protect(
        self,
        keyring: &kekring::Keyring,
        scope: &EmailRowsScope,
        now: i64,
    ) -> Result<ProtectedSubmission, PreparationError> {
        if self.command.query.batch_id != scope.batch_id
            || self.command.query.admin.account_id != scope.account_id
            || self
                .command
                .submission
                .emails
                .iter()
                .any(|email| email.installation_id != scope.installation_id)
        {
            return Err(PreparationError::Unavailable);
        }
        let personal = self
            .personal
            .into_iter()
            .map(|link| {
                let plan = link.plan().clone();
                let ciphertext = link.seal(keyring, scope, now)?;
                Ok(ProtectedLink { plan, ciphertext })
            })
            .collect::<Result<Vec<_>, PreparationError>>()?;
        Ok(ProtectedSubmission {
            command: self.command,
            personal,
        })
    }
}
impl ProtectedSubmission {
    /// Call after recovering this exact submission and acknowledging every
    /// personal-link creation, and after a durable one-time display claim.
    pub fn restore_personal_links(
        &self,
        keyring: &kekring::Keyring,
        scope: &EmailRowsScope,
        now: i64,
    ) -> Result<Vec<PreparedPersonalLink>, PreparationError> {
        if self.command.query.batch_id != scope.batch_id
            || self.command.query.admin.account_id != scope.account_id
            || self
                .command
                .submission
                .emails
                .iter()
                .any(|email| email.installation_id != scope.installation_id)
            || self.personal.len() != self.command.submission.personal_links.len()
            || self
                .personal
                .iter()
                .zip(&self.command.submission.personal_links)
                .any(|(a, b)| a.plan != *b)
        {
            return Err(PreparationError::Unavailable);
        }
        self.personal
            .iter()
            .map(|link| {
                PreparedPersonalLink::restore(&link.ciphertext, keyring, scope, &link.plan, now)
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ghinvite_core::{
        bulk::{BulkBatchId, Entry, Manifest, RowEvidence},
        email_delivery::EmailRef,
        targets::{MemberGrant, OrganizationTarget, RepoRole, RepoTarget, Targets},
    };
    fn fixture() -> Review {
        let at = DateTime::from_timestamp(1800000000, 0).unwrap();
        let email = Entry::Email {
            email_ref: EmailRef::from_bytes([2; 32]),
        };
        Review::new(
            Manifest {
                batch_id: BulkBatchId::new(),
                admin: AccountAdmin {
                    account_id: 100,
                    user_id: 7,
                },
                installation_id: 1,
                organization_installation_id: Some(2),
                access_lifetime_seconds: Some(3600),
                targets: Targets {
                    organization: Some(OrganizationTarget {
                        org_id: 100,
                        org_login: "acme".into(),
                        grant: MemberGrant::Member,
                    }),
                    teams: vec![],
                    repos: vec![RepoTarget {
                        repo_id: 10,
                        repo_full_name: "acme/api".into(),
                        grant: RepoRole::Write,
                    }],
                },
                entries: vec![
                    Entry::Handle {
                        login: "alice".into(),
                    },
                    email.clone(),
                    email,
                    Entry::Handle {
                        login: "missing".into(),
                    },
                ],
            },
            vec![
                Resolution::Found {
                    user_id: 8,
                    login: "alice".into(),
                },
                Resolution::Email,
                Resolution::Email,
                Resolution::NotFound,
            ]
            .into_iter()
            .enumerate()
            .map(|(i, resolution)| RowEvidence {
                row: i as u16 + 1,
                resolution,
                all_targets_satisfied: false,
                previews: vec![],
                observed_at: at,
                next_budget_slot: None,
            })
            .collect(),
            at,
        )
        .unwrap()
    }
    fn operation() -> LifecycleOperationId {
        ghinvite_core::RequestId::new()
            .to_string()
            .try_into()
            .unwrap()
    }
    #[test]
    fn unresolved_review_allocates_no_submittable_command() {
        let review = fixture();
        assert!(matches!(
            PreparedSubmission::new(
                &review,
                AccountAdmin {
                    account_id: 100,
                    user_id: 9
                },
                operation(),
                review.reviewed_at
            ),
            Err(ghinvite_core::bulk::Refusal::UnresolvedRows)
        ));
    }
    #[test]
    fn protected_attempt_preserves_exact_work_and_original_personal_capability() {
        let mut review = fixture();
        review
            .edit(
                1,
                std::collections::BTreeSet::from([4]),
                std::collections::BTreeSet::from([2]),
            )
            .unwrap();
        let at = review.reviewed_at;
        let prepared = PreparedSubmission::new(
            &review,
            AccountAdmin {
                account_id: 100,
                user_id: 9,
            },
            operation(),
            at,
        )
        .unwrap();
        let command = prepared.command().clone();
        assert_eq!(command.submission.requests.len(), 1);
        assert_eq!(command.submission.emails.len(), 1);
        assert_eq!(command.submission.personal_links.len(), 1);
        assert_eq!(command.submission.submitted_by, 9);
        assert_eq!(command.submission.emails[0].approved_by, 9);
        assert_eq!(review.manifest.admin.user_id, 7);
        assert_eq!(command.submission.personal_links[0].source.row, 2);
        let ring = kekring::Keyring::builder("p1")
            .generation(1, [4; 32])
            .current(1)
            .build()
            .unwrap();
        let scope = EmailRowsScope {
            session: "session-one".into(),
            batch_id: review.manifest.batch_id,
            account_id: 100,
            installation_id: 2,
            session_expires: at.timestamp() + 3600,
        };
        let protected = prepared.protect(&ring, &scope, at.timestamp()).unwrap();
        let encoded = serde_json::to_string(&protected).unwrap();
        assert_eq!(protected.command, command);
        let first = protected
            .restore_personal_links(&ring, &scope, at.timestamp())
            .unwrap()
            .pop()
            .unwrap()
            .show_once();
        let recovered: ProtectedSubmission = serde_json::from_str(&encoded).unwrap();
        let second = recovered
            .restore_personal_links(&ring, &scope, at.timestamp() + 1)
            .unwrap()
            .pop()
            .unwrap()
            .show_once();
        assert_eq!(first, second);
        assert!(!encoded.contains(first.strip_prefix("/i#").unwrap()));
        let mut wrong = scope;
        wrong.installation_id = 3;
        assert!(
            recovered
                .restore_personal_links(&ring, &wrong, at.timestamp())
                .is_err()
        );
    }
}
