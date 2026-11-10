//! Batch identity is independent of invitation-link admission and use accounting.
pub use crate::ids::BulkBatchId;
use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// Stable source of one reviewed row. Row numbers never change after removal.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BatchRef {
    pub batch_id: BulkBatchId,
    pub row: u16,
}

pub const MAX_ROWS: usize = 200;

/// Administrator-issued requests have a batch source and no admission link.
/// An empty source never becomes an implicit administrator privilege.
pub fn validate_origin(
    link_id: Option<crate::InvitationLinkId>,
    batch: Option<BatchRef>,
) -> Result<(), &'static str> {
    match (link_id, batch) {
        (Some(_), None) => Ok(()),
        (None, Some(batch)) => batch.validate(),
        _ => Err("exactly one request origin is required"),
    }
}

impl BatchRef {
    pub fn validate(self) -> Result<(), &'static str> {
        if self.row == 0 || usize::from(self.row) > MAX_ROWS {
            return Err("invalid batch row");
        }
        Ok(())
    }
}

/// The address itself lives only in the encrypted email vault.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Entry {
    Handle {
        login: String,
    },
    Email {
        email_ref: crate::email_delivery::EmailRef,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub batch_id: BulkBatchId,
    pub admin: crate::invitation_link::AccountAdmin,
    pub installation_id: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub organization_installation_id: Option<u64>,
    pub targets: crate::targets::Targets,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access_lifetime_seconds: Option<u32>,
    pub entries: Vec<Entry>,
}

#[derive(
    Clone, Copy, Debug, PartialEq, Eq, thiserror::Error, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Refusal {
    #[error("a batch must have between one and 200 rows")]
    TooManyRows,
    #[error("invalid batch targets or installation binding")]
    InvalidTargets,
    #[error("invalid batch entry")]
    InvalidEntry,
    #[error("review evidence does not match this batch")]
    InvalidReview,
    #[error("the review changed; review the current rows before submitting")]
    ReviewChanged,
    #[error("the batch has already been submitted")]
    AlreadySubmitted,
    #[error("remove unresolved rows or select a personal link before submitting")]
    UnresolvedRows,
    #[error("submission identities do not match the reviewed rows")]
    InvalidSubmission,
}

fn positive(n: u64) -> bool {
    n > 0 && n <= crate::storage::integer::MAX_EXACT
}
fn valid_login(login: &str) -> bool {
    !login.is_empty()
        && login.len() <= 100
        && !login
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || matches!(c, '/' | '\\' | '@'))
}
impl Manifest {
    pub fn org_installation_id(&self) -> Option<u64> {
        crate::invitation_link::target_org_installation_id(
            &self.targets,
            self.installation_id,
            self.organization_installation_id,
        )
    }
    /// Called before any lookup. A large pasted list never triggers 201 reads.
    pub fn normalize(mut self) -> Result<Self, Refusal> {
        if self.entries.is_empty() || self.entries.len() > MAX_ROWS {
            return Err(Refusal::TooManyRows);
        }
        self.targets = self
            .targets
            .normalized()
            .map_err(|_| Refusal::InvalidTargets)?;
        if !positive(self.admin.account_id)
            || !positive(self.admin.user_id)
            || crate::invitation_link::validate_target_installations(
                &self.targets,
                self.admin.account_id,
                self.installation_id,
                self.organization_installation_id,
            )
            .is_err()
            || self.access_lifetime_seconds == Some(0)
        {
            return Err(Refusal::InvalidTargets);
        }
        for entry in &mut self.entries {
            match entry {
                Entry::Handle { login } => {
                    *login = login
                        .trim()
                        .strip_prefix('@')
                        .unwrap_or(login.trim())
                        .to_owned();
                    if !valid_login(login) {
                        return Err(Refusal::InvalidEntry);
                    }
                }
                Entry::Email { .. } if self.targets.organization.is_none() => {
                    return Err(Refusal::InvalidEntry);
                }
                Entry::Email { .. } => {}
            }
        }
        Ok(self)
    }
}

/// Lookup output is numeric; two aliases resolving to one user are duplicates.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Resolution {
    Found { user_id: u64, login: String },
    NotFound,
    Unavailable,
    Email,
}

/// Advisory read result only. Delivery owners repeat their own role checks.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RowEvidence {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub previews: Vec<crate::bulk_preview::TargetPreview>,
    pub row: u16,
    pub resolution: Resolution,
    pub all_targets_satisfied: bool,
    pub observed_at: DateTime<Utc>,
    pub next_budget_slot: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ReviewStatus {
    Found,
    NotFound,
    Unavailable,
    EmailOrgOnly,
    NeedsPersonalLink,
    AlreadySatisfied,
    OverBudget { until: DateTime<Utc> },
    Duplicate { of_row: u16 },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReviewedRow {
    pub row: u16,
    pub evidence: RowEvidence,
    pub status: ReviewStatus,
}

/// A retained review is required before a submission can produce work.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Review {
    pub manifest: Manifest,
    pub rows: Vec<ReviewedRow>,
    pub reviewed_at: DateTime<Utc>,
    pub revision: u64,
    pub removed: BTreeSet<u16>,
    pub personal_link_rows: BTreeSet<u16>,
    pub submitted: Option<Submission>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RowRequest {
    pub source: BatchRef,
    pub request_id: crate::RequestId,
    pub requester_id: u64,
    pub requester_login: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PersonalLink {
    pub source: BatchRef,
    pub link_id: crate::InvitationLinkId,
    pub secret_sha256: crate::SecretHash,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Submission {
    pub submitted_by: u64,
    pub requests: Vec<RowRequest>,
    pub emails: Vec<crate::email_delivery::EmailCreateCommand>,
    pub personal_links: Vec<PersonalLink>,
    pub submitted_at: DateTime<Utc>,
}
impl Review {
    pub fn new(
        manifest: Manifest,
        evidence: Vec<RowEvidence>,
        now: DateTime<Utc>,
    ) -> Result<Self, Refusal> {
        let manifest = manifest.normalize()?;
        if evidence.len() != manifest.entries.len() {
            return Err(Refusal::InvalidReview);
        }
        let mut users = BTreeMap::new();
        let mut handles = BTreeMap::new();
        let mut emails = BTreeMap::new();
        let mut rows = Vec::with_capacity(evidence.len());
        for (index, (entry, evidence)) in manifest.entries.iter().zip(evidence).enumerate() {
            let row = (index + 1) as u16;
            if evidence.row != row || evidence.observed_at > now {
                return Err(Refusal::InvalidReview);
            }
            let duplicate = match (entry, &evidence.resolution) {
                (Entry::Handle { login: input }, Resolution::Found { user_id, login }) => {
                    if !positive(*user_id) || !valid_login(login) {
                        return Err(Refusal::InvalidReview);
                    }
                    let duplicate = users
                        .get(user_id)
                        .copied()
                        .or_else(|| handles.get(&input.to_ascii_lowercase()).copied());
                    users.entry(*user_id).or_insert(row);
                    handles.entry(input.to_ascii_lowercase()).or_insert(row);
                    duplicate
                }
                (Entry::Handle { login }, Resolution::NotFound | Resolution::Unavailable) => {
                    let old = handles.get(&login.to_ascii_lowercase()).copied();
                    handles.entry(login.to_ascii_lowercase()).or_insert(row);
                    old
                }
                (Entry::Email { email_ref }, Resolution::Email) => {
                    if evidence.all_targets_satisfied {
                        return Err(Refusal::InvalidReview);
                    }
                    let old = emails.get(&email_ref.as_hex()).copied();
                    emails.entry(email_ref.as_hex()).or_insert(row);
                    old
                }
                _ => return Err(Refusal::InvalidReview),
            };
            if !evidence.previews.is_empty() {
                for preview in &evidence.previews {
                    preview.validate(&manifest.targets)?;
                }
                if evidence.all_targets_satisfied
                    != (crate::bulk_preview::TargetPreview::complete(
                        &evidence.previews,
                        &manifest.targets,
                    ) && evidence
                        .previews
                        .iter()
                        .all(|p| p.action == crate::bulk_preview::PreviewAction::Satisfied))
                {
                    return Err(Refusal::InvalidReview);
                }
            }
            let status = if let Some(of_row) = duplicate {
                ReviewStatus::Duplicate { of_row }
            } else {
                match &evidence.resolution {
                    Resolution::NotFound => ReviewStatus::NotFound,
                    Resolution::Unavailable => ReviewStatus::Unavailable,
                    Resolution::Found { .. }
                        if !evidence.previews.is_empty()
                            && !crate::bulk_preview::TargetPreview::complete(
                                &evidence.previews,
                                &manifest.targets,
                            ) =>
                    {
                        ReviewStatus::Unavailable
                    }
                    Resolution::Email if !manifest.targets.repos.is_empty() => {
                        ReviewStatus::NeedsPersonalLink
                    }
                    _ if evidence.all_targets_satisfied => ReviewStatus::AlreadySatisfied,
                    _ if evidence.next_budget_slot.is_some_and(|at| at > now) => {
                        ReviewStatus::OverBudget {
                            until: evidence.next_budget_slot.unwrap(),
                        }
                    }
                    Resolution::Email => ReviewStatus::EmailOrgOnly,
                    Resolution::Found { .. } => ReviewStatus::Found,
                }
            };
            rows.push(ReviewedRow {
                row,
                evidence,
                status,
            });
        }
        Ok(Self {
            manifest,
            rows,
            reviewed_at: now,
            revision: 1,
            removed: BTreeSet::new(),
            personal_link_rows: BTreeSet::new(),
            submitted: None,
        })
    }
    pub fn edit(
        &mut self,
        expected_revision: u64,
        remove: BTreeSet<u16>,
        personal_links: BTreeSet<u16>,
    ) -> Result<(), Refusal> {
        if self.submitted.is_some() {
            return Err(Refusal::AlreadySubmitted);
        }
        if self.revision != expected_revision {
            return Err(Refusal::ReviewChanged);
        }
        if remove
            .iter()
            .any(|row| *row == 0 || usize::from(*row) > self.rows.len())
            || personal_links.iter().any(|row| {
                remove.contains(row)
                    || self.removed.contains(row)
                    || !self
                        .rows
                        .iter()
                        .any(|r| r.row == *row && r.status == ReviewStatus::NeedsPersonalLink)
            })
        {
            return Err(Refusal::InvalidReview);
        }
        let next = self
            .revision
            .checked_add(1)
            .filter(|n| *n <= crate::storage::integer::MAX_EXACT)
            .ok_or(Refusal::InvalidReview)?;
        self.removed.extend(remove);
        self.personal_link_rows = personal_links;
        self.revision = next;
        Ok(())
    }
    /// Validate all allocated work before changing the review. The owner retains
    /// this exact submission before sending any request, email or link command.
    pub fn submit(
        &mut self,
        expected_revision: u64,
        submission: Submission,
    ) -> Result<(), Refusal> {
        if self.submitted.is_some() {
            return Err(Refusal::AlreadySubmitted);
        }
        if self.revision != expected_revision {
            return Err(Refusal::ReviewChanged);
        }
        if !positive(submission.submitted_by) || submission.submitted_at < self.reviewed_at {
            return Err(Refusal::InvalidSubmission);
        }
        let mut expected_requests = BTreeSet::new();
        let mut expected_emails = BTreeSet::new();
        let mut expected_links = BTreeSet::new();
        for row in &self.rows {
            if self.removed.contains(&row.row)
                || matches!(row.status, ReviewStatus::Duplicate { .. })
            {
                continue;
            }
            if row.status == ReviewStatus::Unavailable {
                return Err(Refusal::UnresolvedRows);
            }
            match row.evidence.resolution {
                Resolution::NotFound | Resolution::Unavailable => {
                    return Err(Refusal::UnresolvedRows);
                }
                Resolution::Found { .. } => {
                    expected_requests.insert(row.row);
                }
                Resolution::Email => {
                    expected_emails.insert(row.row);
                    if !self.manifest.targets.repos.is_empty() {
                        if !self.personal_link_rows.contains(&row.row) {
                            return Err(Refusal::UnresolvedRows);
                        }
                        expected_links.insert(row.row);
                    }
                }
            }
        }
        if expected_requests.is_empty() && expected_emails.is_empty() {
            return Err(Refusal::UnresolvedRows);
        }
        let mut requests = BTreeSet::new();
        let mut ids = BTreeSet::new();
        for request in &submission.requests {
            let row = request.source.row;
            if request.source.batch_id != self.manifest.batch_id
                || !requests.insert(row)
                || !ids.insert(request.request_id.to_string())
                || !expected_requests.contains(&row)
                || self.rows[usize::from(row) - 1].evidence.resolution
                    != (Resolution::Found {
                        user_id: request.requester_id,
                        login: request.requester_login.clone(),
                    })
            {
                return Err(Refusal::InvalidSubmission);
            }
        }
        let mut emails = BTreeSet::new();
        for email in &submission.emails {
            let row = email.batch.row;
            if email.batch.batch_id != self.manifest.batch_id
                || !emails.insert(row)
                || !expected_emails.contains(&row)
                || email.validate().is_err()
                || email.account_id != self.manifest.admin.account_id
                || Some(&email.organization) != self.manifest.targets.organization.as_ref()
                || email.teams != self.manifest.targets.teams
                || email.approved_by != submission.submitted_by
                || Some(email.installation_id)
                    != crate::invitation_link::target_org_installation_id(
                        &self.manifest.targets,
                        self.manifest.installation_id,
                        self.manifest.organization_installation_id,
                    )
                || email.approved_at != submission.submitted_at
                || self.manifest.entries[usize::from(row) - 1]
                    != (Entry::Email {
                        email_ref: email.email_ref,
                    })
            {
                return Err(Refusal::InvalidSubmission);
            }
        }
        let mut links = BTreeSet::new();
        let mut link_ids = BTreeSet::new();
        let mut hashes = BTreeSet::new();
        for link in &submission.personal_links {
            if link.source.batch_id != self.manifest.batch_id
                || !links.insert(link.source.row)
                || !expected_links.contains(&link.source.row)
                || !link_ids.insert(link.link_id.to_string())
                || !hashes.insert(link.secret_sha256.as_hex().to_owned())
            {
                return Err(Refusal::InvalidSubmission);
            }
        }
        if requests != expected_requests || emails != expected_emails || links != expected_links {
            return Err(Refusal::InvalidSubmission);
        }
        let revision = self
            .revision
            .checked_add(1)
            .filter(|n| *n <= crate::storage::integer::MAX_EXACT)
            .ok_or(Refusal::InvalidSubmission)?;
        self.submitted = Some(submission);
        self.revision = revision;
        Ok(())
    }

    /// Validate a complete owner snapshot, including deserialized snapshots.
    pub fn validate(&self) -> Result<(), Refusal> {
        let mut expected = Self::new(
            self.manifest.clone(),
            self.rows.iter().map(|r| r.evidence.clone()).collect(),
            self.reviewed_at,
        )?;
        if expected.manifest != self.manifest
            || expected.rows != self.rows
            || self.revision == 0
            || self.revision > crate::storage::integer::MAX_EXACT
        {
            return Err(Refusal::InvalidReview);
        }
        if !self.removed.is_empty() || !self.personal_link_rows.is_empty() {
            expected.edit(1, self.removed.clone(), self.personal_link_rows.clone())?;
        }
        let submitted = u64::from(self.submitted.is_some());
        if self.revision < expected.revision + submitted {
            return Err(Refusal::InvalidReview);
        }
        expected.revision = self.revision - submitted;
        if let Some(submission) = &self.submitted {
            expected.submit(expected.revision, submission.clone())?;
        }
        if expected != *self {
            return Err(Refusal::InvalidReview);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
