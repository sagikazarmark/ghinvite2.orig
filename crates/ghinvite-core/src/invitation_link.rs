use crate::ids::InvitationLinkId;
pub use crate::link_secret::{LinkSecretState, SecretHash};
use crate::repository_identity::RepositoryIdentity;
use crate::targets::Targets;
use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AccountAdmin {
    pub account_id: u64,
    pub user_id: u64,
}

impl AccountAdmin {
    /// The authority's admin check: a real user acting for this real account.
    pub fn acts_for(&self, account_id: u64) -> bool {
        self.account_id == account_id && account_id != 0 && self.user_id != 0
    }
}

/// Default GitHub invitation lifetime: seven days.
pub const fn default_invitation_lifetime_seconds() -> u32 {
    604800
}
fn is_default_invitation_lifetime_seconds(value: &u32) -> bool {
    *value == default_invitation_lifetime_seconds()
}

/// Immutable share of each applicable daily invitation bucket.
pub const fn default_budget_share_pct() -> u8 {
    50
}
pub fn is_default_budget_share_pct(value: &u8) -> bool {
    *value == 50
}

/// Sliding-hour admission limit when the admin does not override it.
pub fn default_abuse_threshold(max_uses: Option<u32>) -> u32 {
    max_uses.map_or(20, |uses| uses.div_ceil(10).max(20))
}

/// Allocate the link ID before first submission; reuse it on retry.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateLink {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub batch_source: Option<crate::bulk::BatchRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access_lifetime_seconds: Option<u32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub email_domains: Vec<crate::email_domain::DomainRule>,
    pub link_id: InvitationLinkId,
    pub admin: AccountAdmin,
    pub account_id: u64,
    /// Installation of primary_app(); repository app when repository targets exist.
    pub installation_id: u64,
    /// Separate org-app installation only for mixed repository/organization links.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub organization_installation_id: Option<u64>,
    pub description: String,
    pub internal_note: Option<String>,
    pub expires_at: Option<DateTime<Utc>>,
    pub max_uses: Option<u32>,
    #[serde(
        default = "default_invitation_lifetime_seconds",
        skip_serializing_if = "is_default_invitation_lifetime_seconds"
    )]
    pub invitation_lifetime_seconds: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub abuse_threshold_per_hour: Option<u32>,
    #[serde(
        default = "default_budget_share_pct",
        skip_serializing_if = "is_default_budget_share_pct"
    )]
    pub budget_share_pct: u8,
    pub approval_required: bool,
    pub targets: Targets,
    pub secret_sha256: SecretHash,
}

/// App selection is derived only from immutable targets, never a caller flag.
pub(crate) fn primary_target_app(targets: &Targets) -> crate::account::App {
    if targets.organization.is_some() && targets.repos.is_empty() {
        crate::account::App::Org
    } else {
        crate::account::App::Repo
    }
}
pub(crate) fn target_org_installation_id(
    targets: &Targets,
    primary: u64,
    optional: Option<u64>,
) -> Option<u64> {
    match (targets.organization.is_some(), targets.repos.is_empty()) {
        (false, _) => None,
        (true, true) => Some(primary),
        (true, false) => optional,
    }
}
pub(crate) fn validate_target_installations(
    targets: &Targets,
    account_id: u64,
    primary: u64,
    optional: Option<u64>,
) -> Result<(), InvalidLinkValues> {
    if account_id == 0
        || account_id > crate::storage::integer::MAX_EXACT
        || targets
            .organization
            .as_ref()
            .is_some_and(|org| org.org_id != account_id)
    {
        return Err(InvalidLinkValues);
    }
    validate_installation_ids(targets, primary, optional)
}
fn validate_installation_ids(
    targets: &Targets,
    primary: u64,
    optional: Option<u64>,
) -> Result<(), InvalidLinkValues> {
    let valid = |id| id > 0 && id <= crate::storage::integer::MAX_EXACT;
    if !valid(primary)
        || optional.is_some_and(|id| !valid(id))
        || match (targets.organization.is_some(), targets.repos.is_empty()) {
            (true, false) => optional.is_none(),
            _ => optional.is_some(),
        }
    {
        return Err(InvalidLinkValues);
    }
    Ok(())
}

/// Public creation acknowledgment. The full retained snapshot stays in the owner.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LinkCreated {
    pub link_id: InvitationLinkId,
    pub secret_generation: u32,
}

/// Business refusals are journaled values; malformed commands remain terminal errors.
pub type CreateLinkOutcome = Result<LinkCreated, CreateLinkError>;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum GuardrailField {
    EmailDomains,
    MaxUses,
    ExpiresAt,
    InvitationLifetime,
    AccessLifetime,
    AbuseThreshold,
    BudgetShare,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("invalid invitation link values")]
pub struct InvalidLinkValues;

#[derive(
    Clone, Copy, Debug, PartialEq, Eq, thiserror::Error, Serialize, Deserialize, JsonSchema,
)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CreateLinkError {
    #[error("NoGuardrail")]
    NoGuardrail,
    #[error("InvalidTargets")]
    InvalidTargets { reason: crate::targets::TargetError },
    #[error("OrgAutoApproveNeedsMaxUses")]
    OrgAutoApproveNeedsMaxUses,
    #[error("InvalidGuardrail")]
    InvalidGuardrail { field: GuardrailField },
    #[error("AlreadyExists")]
    AlreadyExists,
}
impl CreateLink {
    pub fn primary_app(&self) -> crate::account::App {
        primary_target_app(&self.targets)
    }
    pub fn org_installation_id(&self) -> Option<u64> {
        target_org_installation_id(
            &self.targets,
            self.installation_id,
            self.organization_installation_id,
        )
    }
    pub fn validate_installations(&self) -> Result<(), InvalidLinkValues> {
        validate_target_installations(
            &self.targets,
            self.account_id,
            self.installation_id,
            self.organization_installation_id,
        )
    }
    pub fn validate_targets(&self) -> Result<(), CreateLinkError> {
        if self
            .targets
            .organization
            .as_ref()
            .is_some_and(|org| org.org_id != self.account_id)
        {
            return Err(CreateLinkError::InvalidTargets {
                reason: crate::targets::TargetError::DifferentAccounts,
            });
        }
        self.targets
            .validate()
            .map_err(|reason| CreateLinkError::InvalidTargets { reason })
    }

    pub fn validate_guardrails(&self) -> Result<(), CreateLinkError> {
        if self.email_domains.len() > crate::email_domain::MAX_DOMAIN_RULES {
            return Err(CreateLinkError::InvalidGuardrail {
                field: GuardrailField::EmailDomains,
            });
        }
        if self.access_lifetime_seconds == Some(0) {
            return Err(CreateLinkError::InvalidGuardrail {
                field: GuardrailField::AccessLifetime,
            });
        }
        if !(3600..=604800).contains(&self.invitation_lifetime_seconds) {
            return Err(CreateLinkError::InvalidGuardrail {
                field: GuardrailField::InvitationLifetime,
            });
        }
        if self.abuse_threshold_per_hour == Some(0) {
            return Err(CreateLinkError::InvalidGuardrail {
                field: GuardrailField::AbuseThreshold,
            });
        }
        if !(1..=100).contains(&self.budget_share_pct) {
            return Err(CreateLinkError::InvalidGuardrail {
                field: GuardrailField::BudgetShare,
            });
        }
        if self.max_uses == Some(0) {
            return Err(CreateLinkError::InvalidGuardrail {
                field: GuardrailField::MaxUses,
            });
        }
        if !self.approval_required && self.max_uses.is_none() && self.targets.organization.is_some()
        {
            return Err(CreateLinkError::OrgAutoApproveNeedsMaxUses);
        }
        if !self.approval_required && self.max_uses.is_none() && self.expires_at.is_none() {
            return Err(CreateLinkError::NoGuardrail);
        }
        Ok(())
    }

    /// Canonicalize syntactically valid input before binding either a success
    /// or a business refusal. This does not establish target or guardrail validity.
    pub fn canonical_input(mut self) -> Result<Self, InvalidLinkValues> {
        // Account/target disagreement remains a typed InvalidTargets refusal.
        validate_installation_ids(
            &self.targets,
            self.installation_id,
            self.organization_installation_id,
        )?;
        let metadata = LinkMetadata::parse(&self.description, self.internal_note.as_deref())?;
        (self.description, self.internal_note) = (metadata.description, metadata.internal_note);
        self.email_domains.sort();
        self.email_domains.dedup();
        self.targets.repos.sort_by_key(|repo| repo.repo_id);
        self.targets.teams.sort_by_key(|team| team.team_id);
        Ok(self)
    }

    /// Bind creation identity to normalized metadata, guardrails and scope.
    /// Admin authority is checked separately.
    pub fn normalized(mut self) -> Result<Self, InvalidLinkValues> {
        if let Some(source) = self.batch_source {
            source.validate().map_err(|_| InvalidLinkValues)?;
            if self.max_uses != Some(1)
                || self.approval_required
                || self.expires_at.is_none()
                || self.targets.organization.is_some()
                || !self.targets.teams.is_empty()
                || !self.email_domains.is_empty()
            {
                return Err(InvalidLinkValues);
            }
        }

        self.email_domains.sort();
        self.email_domains.dedup();
        self.validate_installations()?;
        if self.max_uses == Some(0)
            || self
                .targets
                .organization
                .as_ref()
                .is_some_and(|org| org.org_id != self.account_id)
        {
            return Err(InvalidLinkValues);
        }
        self.validate_guardrails().map_err(|_| InvalidLinkValues)?;
        let metadata = LinkMetadata::parse(&self.description, self.internal_note.as_deref())?;
        (self.description, self.internal_note) = (metadata.description, metadata.internal_note);
        self.targets = self.targets.normalized().map_err(|_| InvalidLinkValues)?;
        Ok(self)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct LinkSnapshot {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pause: Option<crate::link_lifecycle::LinkPause>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<LinkMetadata>,
    pub link_id: InvitationLinkId,
    pub creation: CreateLink,
    pub secret: LinkSecretState,
    pub created_at: DateTime<Utc>,
    pub uses: u64,
    pub revision: u64,
    pub revoked_at: Option<DateTime<Utc>>,
    pub revoked_by: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct LinkMetadata {
    pub description: String,
    pub internal_note: Option<String>,
}

impl LinkMetadata {
    /// Parse mutable metadata; a blank internal note means none.
    pub fn parse(
        description: &str,
        internal_note: Option<&str>,
    ) -> Result<Self, InvalidLinkValues> {
        let description = crate::Description::parse(description).map_err(|_| InvalidLinkValues)?;
        let internal_note = match internal_note {
            Some(note) => crate::InternalNote::parse(note).map_err(|_| InvalidLinkValues)?,
            None => None,
        };
        Ok(Self {
            description: description.into(),
            internal_note: internal_note.map(String::from),
        })
    }
}

/// Authority state is distinct from the derived expiry/exhaustion guardrails.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum LinkState {
    Active,
    Paused,
    Revoked,
}

impl LinkSnapshot {
    pub fn state(&self) -> LinkState {
        if self.revoked_at.is_some() {
            LinkState::Revoked
        } else if self.pause.is_some() {
            LinkState::Paused
        } else {
            LinkState::Active
        }
    }

    pub fn description(&self) -> &str {
        self.metadata
            .as_ref()
            .map(|m| m.description.as_str())
            .unwrap_or(&self.creation.description)
    }
    pub fn internal_note(&self) -> Option<&str> {
        match &self.metadata {
            Some(m) => m.internal_note.as_deref(),
            None => self.creation.internal_note.as_deref(),
        }
    }
    /// Replace the mutable metadata. Saving the current details again changes
    /// nothing; answers whether the link changed.
    pub fn update_metadata(&mut self, metadata: LinkMetadata) -> bool {
        if self.description() == metadata.description
            && self.internal_note() == metadata.internal_note.as_deref()
        {
            return false;
        }
        self.metadata = Some(metadata);
        self.revision += 1;
        true
    }
    /// Move to the next secret generation, which the link knows only by hash.
    pub fn rotate_secret(
        &mut self,
        new_sha256: SecretHash,
    ) -> Result<SecretRotated, RotationRefusal> {
        if self.revoked_at.is_some() {
            return Err(RotationRefusal::Revoked);
        }
        let generation = self.secret.secret_generation.checked_add(1);
        let revision = self.revision.checked_add(1);
        let (Some(generation), Some(revision)) = (generation, revision) else {
            return Err(RotationRefusal::Exhausted);
        };
        self.secret = LinkSecretState {
            secret_generation: generation,
            sha256: new_sha256,
        };
        self.revision = revision;
        Ok(SecretRotated {
            secret_generation: generation,
        })
    }
    pub fn inactive(&self, now: DateTime<Utc>) -> Option<Inactive> {
        Inactive::check(
            self.state() == LinkState::Revoked,
            self.creation.expires_at,
            self.uses,
            self.creation.max_uses,
            now,
        )
        .or_else(|| self.pause.as_ref().map(|_| Inactive::Paused))
    }
    pub fn as_link(&self) -> InvitationLink {
        InvitationLink {
            batch_source: self.creation.batch_source,
            access_lifetime_seconds: self.creation.access_lifetime_seconds,
            email_domains: self.creation.email_domains.clone(),
            id: self.link_id,
            organization_installation_id: self.creation.organization_installation_id,
            installation_id: self.creation.installation_id,
            account_id: self.creation.account_id,
            created_by: self.creation.admin.user_id,
            created_at: self.created_at,
            expires_at: self.creation.expires_at,
            max_uses: self.creation.max_uses,
            invitation_lifetime_seconds: self.creation.invitation_lifetime_seconds,
            abuse_threshold_per_hour: self.creation.abuse_threshold_per_hour,
            budget_share_pct: self.creation.budget_share_pct,
            uses_count: self.uses.try_into().unwrap_or(u32::MAX),
            approval_required: self.creation.approval_required,
            description: self.description().into(),
            internal_note: self.internal_note().map(str::to_owned),
            revoked_at: self.revoked_at,
            revoked_by: self.revoked_by,
            pause: self.pause.clone(),
            targets: self.creation.targets.clone(),
            secret: self.secret.clone(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InvitationLink {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub batch_source: Option<crate::bulk::BatchRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access_lifetime_seconds: Option<u32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub email_domains: Vec<crate::email_domain::DomainRule>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pause: Option<crate::link_lifecycle::LinkPause>,
    pub id: InvitationLinkId,
    /// Installation of primary_app(); repository app when repository targets exist.
    pub installation_id: u64,
    /// Separate org-app installation only for mixed repository/organization links.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub organization_installation_id: Option<u64>,
    pub account_id: u64,
    pub created_by: u64, // user_id
    pub created_at: DateTime<Utc>,
    pub expires_at: Option<DateTime<Utc>>,
    pub max_uses: Option<u32>,
    #[serde(
        default = "default_invitation_lifetime_seconds",
        skip_serializing_if = "is_default_invitation_lifetime_seconds"
    )]
    pub invitation_lifetime_seconds: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub abuse_threshold_per_hour: Option<u32>,
    #[serde(
        default = "default_budget_share_pct",
        skip_serializing_if = "is_default_budget_share_pct"
    )]
    pub budget_share_pct: u8,
    pub uses_count: u32,
    pub approval_required: bool,
    pub description: String,
    pub internal_note: Option<String>,
    pub revoked_at: Option<DateTime<Utc>>,
    pub revoked_by: Option<u64>,
    pub targets: Targets,
    pub secret: LinkSecretState,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct InvitationLinkRepo {
    pub repo_id: u64,
    pub repo_full_name: String,
}

impl InvitationLink {
    pub fn primary_app(&self) -> crate::account::App {
        primary_target_app(&self.targets)
    }
    pub fn org_installation_id(&self) -> Option<u64> {
        target_org_installation_id(
            &self.targets,
            self.installation_id,
            self.organization_installation_id,
        )
    }
    pub fn validate_installations(&self) -> Result<(), InvalidLinkValues> {
        validate_target_installations(
            &self.targets,
            self.account_id,
            self.installation_id,
            self.organization_installation_id,
        )
    }
    pub fn state(&self) -> LinkState {
        if self.revoked_at.is_some() {
            LinkState::Revoked
        } else if self.pause.is_some() {
            LinkState::Paused
        } else {
            LinkState::Active
        }
    }
    /// `is_active` is the per-spec derived state: not revoked AND not expired AND not exhausted.
    pub fn is_active(&self, now: DateTime<Utc>) -> bool {
        Inactive::check(
            self.state() == LinkState::Revoked,
            self.expires_at,
            u64::from(self.uses_count),
            self.max_uses,
            now,
        )
        .is_none()
            && self.pause.is_none()
    }
}

/// Why an invitation link no longer accepts new invitation requests.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Inactive {
    Paused,
    Revoked,
    Expired,
    Exhausted,
}

impl Inactive {
    /// The first of revocation, expiration at `now`, and exhausted max use
    /// that applies; `None` while the invitation link is active.
    pub fn check(
        revoked: bool,
        expires_at: Option<DateTime<Utc>>,
        uses: u64,
        max_uses: Option<u32>,
        now: DateTime<Utc>,
    ) -> Option<Self> {
        if revoked {
            Some(Self::Revoked)
        } else if expires_at.is_some_and(|at| now >= at) {
            Some(Self::Expired)
        } else if max_uses.is_some_and(|max| uses >= u64::from(max)) {
            Some(Self::Exhausted)
        } else {
            None
        }
    }
}

pub const DESCRIPTION_MAX_CHARS: usize = 120;
pub const INTERNAL_NOTE_MAX_BYTES: usize = 16_384;
pub const REPOSITORY_SCOPE_MAX_REPOS: usize = 100;
pub const REPOSITORY_FULL_NAME_MAX_BYTES: usize = 256;

/// An invitation link's description: trimmed, non-empty, single-line text of
/// at most [`DESCRIPTION_MAX_CHARS`] characters.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Description(String);

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum DescriptionError {
    #[error("description is required")]
    Required,
    #[error("description must be a single line")]
    MultiLine,
    #[error("description is too long")]
    TooLong,
}

impl Description {
    /// Parse typed text. A line break anywhere, even a trailing one, is
    /// rejected rather than trimmed away.
    pub fn parse(raw: &str) -> Result<Self, DescriptionError> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            Err(DescriptionError::Required)
        } else if raw.contains(['\r', '\n']) {
            Err(DescriptionError::MultiLine)
        } else if trimmed.chars().count() > DESCRIPTION_MAX_CHARS {
            Err(DescriptionError::TooLong)
        } else {
            Ok(Self(trimmed.to_owned()))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<Description> for String {
    fn from(description: Description) -> Self {
        description.0
    }
}

/// An invitation link's internal note: trimmed, non-empty text of at most
/// [`INTERNAL_NOTE_MAX_BYTES`] UTF-8 bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InternalNote(String);

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("internal note is too long")]
pub struct InternalNoteTooLong;

impl InternalNote {
    /// Parse typed text; blank text means no internal note.
    pub fn parse(raw: &str) -> Result<Option<Self>, InternalNoteTooLong> {
        let trimmed = raw.trim();
        if trimmed.len() > INTERNAL_NOTE_MAX_BYTES {
            Err(InternalNoteTooLong)
        } else {
            Ok((!trimmed.is_empty()).then(|| Self(trimmed.to_owned())))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<InternalNote> for String {
    fn from(note: InternalNote) -> Self {
        note.0
    }
}

/// An invitation link's repository scope: one to
/// [`REPOSITORY_SCOPE_MAX_REPOS`] distinct repositories ordered by repository
/// ID, each named by a valid [`RepositoryIdentity`] full name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RepositoryScope(Vec<InvitationLinkRepo>);

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum RepositoryScopeError {
    #[error("repository scope is empty")]
    Empty,
    #[error("repository scope has too many repositories")]
    TooMany,
    #[error("repository scope lists a repository more than once")]
    Duplicate,
    #[error("repository scope lists an invalid repository")]
    InvalidRepository,
}

impl RepositoryScope {
    pub fn parse(mut repos: Vec<InvitationLinkRepo>) -> Result<Self, RepositoryScopeError> {
        if repos.is_empty() {
            return Err(RepositoryScopeError::Empty);
        }
        if repos.len() > REPOSITORY_SCOPE_MAX_REPOS {
            return Err(RepositoryScopeError::TooMany);
        }
        repos.sort_by_key(|repo| repo.repo_id);
        if repos
            .windows(2)
            .any(|pair| pair[0].repo_id == pair[1].repo_id)
        {
            return Err(RepositoryScopeError::Duplicate);
        }
        if repos.iter().any(|repo| {
            repo.repo_id == 0
                || repo.repo_full_name.len() > REPOSITORY_FULL_NAME_MAX_BYTES
                || RepositoryIdentity::parse(repo.repo_full_name.as_str()).is_err()
        }) {
            return Err(RepositoryScopeError::InvalidRepository);
        }
        Ok(Self(repos))
    }

    pub fn as_slice(&self) -> &[InvitationLinkRepo] {
        &self.0
    }
}

impl From<RepositoryScope> for Vec<InvitationLinkRepo> {
    fn from(scope: RepositoryScope) -> Self {
        scope.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn at(y: i32, mo: u32, d: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, mo, d, 0, 0, 0).unwrap()
    }

    fn base_link() -> InvitationLink {
        InvitationLink {
            batch_source: None,
            access_lifetime_seconds: None,
            email_domains: vec![],
            id: InvitationLinkId::new(),
            organization_installation_id: None,
            installation_id: 1,
            account_id: 2,
            created_by: 3,
            created_at: at(2026, 1, 1),
            expires_at: None,
            max_uses: None,
            invitation_lifetime_seconds: 604800,
            abuse_threshold_per_hour: None,
            budget_share_pct: 50,
            uses_count: 0,

            approval_required: false,
            description: "AI coding workshop".into(),
            internal_note: None,
            revoked_at: None,
            revoked_by: None,
            pause: None,
            targets: crate::targets::Targets {
                repos: vec![],
                ..Default::default()
            },

            secret: crate::LinkSecretState {
                secret_generation: 1,
                sha256: crate::SecretHash::from_bytes([1; 32]),
            },
        }
    }

    #[test]
    fn target_installations_are_canonical_app_scoped_and_portable() {
        let wire = serde_json::json!({"link_id":"01ARZ3NDEKTSV4RRFFQ69G5FAV","admin":{"account_id":2,"user_id":3},"account_id":2,"installation_id":1,"description":"Access","internal_note":null,"expires_at":null,"max_uses":5,"approval_required":false,"targets":{"organization":null,"teams":[],"repos":[{"repo_id":10,"repo_full_name":"acme/repo","grant":"read"}]},"secret_sha256":"01".repeat(32)});
        let repo: CreateLink = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(repo.primary_app(), crate::account::App::Repo);
        assert_eq!(repo.org_installation_id(), None);
        assert_eq!(
            serde_json::to_value(&repo).unwrap(),
            wire,
            "legacy repository canonical content gains no fields"
        );
        assert!(repo.clone().normalized().is_ok());
        let mut restricted = repo.clone();
        restricted.email_domains = vec![
            crate::email_domain::DomainRule::parse("EXAMPLE.COM.")
                .unwrap();
            crate::email_domain::MAX_DOMAIN_RULES + 1
        ];
        assert_eq!(restricted.normalized().unwrap().email_domains.len(), 1);
        let mut too_many = repo.clone();
        too_many.email_domains = (0..=crate::email_domain::MAX_DOMAIN_RULES)
            .map(|n| {
                crate::email_domain::DomainRule::parse(&format!("team{n}.example.com")).unwrap()
            })
            .collect();
        assert_eq!(
            too_many.canonical_input().unwrap().validate_guardrails(),
            Err(CreateLinkError::InvalidGuardrail {
                field: GuardrailField::EmailDomains
            })
        );
        let mut redundant = repo.clone();
        redundant.organization_installation_id = Some(1);
        assert!(redundant.clone().canonical_input().is_err());
        assert!(redundant.normalized().is_err());
        let mut mixed = repo.clone();
        mixed.targets.organization = Some(crate::targets::OrganizationTarget {
            org_id: 2,
            org_login: "acme".into(),
            grant: crate::targets::MemberGrant::Member,
        });
        assert!(mixed.clone().normalized().is_err());
        mixed.organization_installation_id = Some(1); // Same numeric ID in distinct app namespaces is valid.
        assert_eq!(mixed.primary_app(), crate::account::App::Repo);
        assert_eq!(mixed.org_installation_id(), Some(1));
        assert!(mixed.clone().normalized().is_ok());
        for id in [0, crate::storage::integer::MAX_EXACT + 1, u64::MAX] {
            let mut bad = mixed.clone();
            bad.organization_installation_id = Some(id);
            assert!(bad.normalized().is_err());
            let mut bad = repo.clone();
            bad.installation_id = id;
            assert!(bad.normalized().is_err());
        }
        let mut wrong_account = mixed.clone();
        wrong_account.targets.organization.as_mut().unwrap().org_id = 3;
        assert!(
            wrong_account.clone().canonical_input().is_ok(),
            "account mismatch retains typed target refusal"
        );
        assert_eq!(
            wrong_account.validate_targets(),
            Err(CreateLinkError::InvalidTargets {
                reason: crate::targets::TargetError::DifferentAccounts
            })
        );
        assert!(wrong_account.normalized().is_err());
        let mut org = mixed;
        org.targets.repos.clear();
        org.organization_installation_id = None;
        assert_eq!(org.primary_app(), crate::account::App::Org);
        assert_eq!(org.org_installation_id(), Some(1));
        assert!(org.clone().normalized().is_ok());
        org.organization_installation_id = Some(1);
        assert!(
            org.normalized().is_err(),
            "org-only must use its primary field, not two authorities"
        );
    }

    #[test]
    fn creation_requires_explicit_secret_per_target_grants_and_guardrails() {
        let mut input = serde_json::json!({
            "link_id":"01ARZ3NDEKTSV4RRFFQ69G5FAV", "admin":{"account_id":2,"user_id":3},
            "account_id":2,"installation_id":1,"description":"Access","internal_note":null,
            "expires_at":null,"max_uses":null,"approval_required":false,
            "targets":{"organization":null,"teams":[],"repos":[{"repo_id":10,"repo_full_name":"acme/repo","grant":"read"}]},
            "secret_sha256":"01".repeat(32)
        });
        let mut create: CreateLink = serde_json::from_value(input.clone()).unwrap();
        assert_eq!(
            create.validate_guardrails(),
            Err(CreateLinkError::NoGuardrail)
        );
        create.approval_required = true;
        assert_eq!(create.invitation_lifetime_seconds, 604800);
        assert_eq!(create.abuse_threshold_per_hour, None);
        assert_eq!(create.budget_share_pct, 50);
        assert!(
            serde_json::to_value(&create)
                .unwrap()
                .get("budget_share_pct")
                .is_none()
        );
        for pct in [0, 101, u8::MAX] {
            let mut invalid = create.clone();
            invalid.budget_share_pct = pct;
            assert_eq!(
                invalid.validate_guardrails(),
                Err(CreateLinkError::InvalidGuardrail {
                    field: GuardrailField::BudgetShare
                })
            );
        }
        for pct in [1, 50, 100] {
            let mut valid = create.clone();
            valid.budget_share_pct = pct;
            assert!(valid.normalized().is_ok());
        }
        for seconds in [0, 3599, 604801, u32::MAX] {
            let mut invalid = create.clone();
            invalid.invitation_lifetime_seconds = seconds;
            assert_eq!(
                invalid.validate_guardrails(),
                Err(CreateLinkError::InvalidGuardrail {
                    field: GuardrailField::InvitationLifetime
                })
            );
        }
        for seconds in [3600, 604800] {
            let mut valid = create.clone();
            valid.invitation_lifetime_seconds = seconds;
            assert!(valid.normalized().is_ok());
        }
        let mut invalid = create.clone();
        invalid.abuse_threshold_per_hour = Some(0);
        assert_eq!(
            invalid.validate_guardrails(),
            Err(CreateLinkError::InvalidGuardrail {
                field: GuardrailField::AbuseThreshold
            })
        );
        assert_eq!(default_abuse_threshold(None), 20);
        assert_eq!(default_abuse_threshold(Some(200)), 20);
        assert_eq!(default_abuse_threshold(Some(201)), 21);
        assert_eq!(default_abuse_threshold(Some(u32::MAX)), 429496730);
        assert!(create.normalized().is_ok());
        input["max_uses"] = serde_json::json!(1);
        let create: CreateLink = serde_json::from_value(input.clone()).unwrap();
        assert!(create.normalized().is_ok());
        input["targets"]["organization"] =
            serde_json::json!({"org_id":2,"org_login":"acme","grant":"member"});
        input["max_uses"] = serde_json::Value::Null;
        input["expires_at"] = serde_json::json!("2027-01-01T00:00:00Z");
        let create: CreateLink = serde_json::from_value(input.clone()).unwrap();
        assert_eq!(
            create.validate_guardrails(),
            Err(CreateLinkError::OrgAutoApproveNeedsMaxUses)
        );
        input.as_object_mut().unwrap().remove("secret_sha256");
        assert!(serde_json::from_value::<CreateLink>(input).is_err());
    }

    #[test]
    fn fresh_link_is_active() {
        let l = base_link();
        assert!(l.is_active(at(2026, 1, 2)));
    }

    #[test]
    fn revoked_link_is_not_active() {
        let mut l = base_link();
        l.revoked_at = Some(at(2026, 1, 2));
        l.revoked_by = Some(99);
        assert!(!l.is_active(at(2026, 1, 3)));
    }

    #[test]
    fn expired_link_is_not_active() {
        let mut l = base_link();
        l.expires_at = Some(at(2026, 1, 5));
        assert!(l.is_active(at(2026, 1, 4)));
        assert!(!l.is_active(at(2026, 1, 5))); // boundary: at expires_at, no longer active
        assert!(!l.is_active(at(2026, 1, 6)));
    }

    #[test]
    fn exhausted_link_is_not_active() {
        let mut l = base_link();
        l.max_uses = Some(2);
        l.uses_count = 1;
        assert!(l.is_active(at(2026, 2, 1)));
        l.uses_count = 2;
        assert!(!l.is_active(at(2026, 2, 1)));
        l.uses_count = 3;
        assert!(!l.is_active(at(2026, 2, 1)));
    }

    #[test]
    fn unlimited_uses_never_exhaust() {
        let mut l = base_link();
        l.max_uses = None;
        l.uses_count = u32::MAX;
        assert!(l.is_active(at(2026, 2, 1)));
    }

    #[test]
    fn inactivity_checks_revocation_then_expiration_then_max_use() {
        let now = at(2026, 2, 1);
        assert_eq!(
            Inactive::check(true, Some(now), 5, Some(5), now),
            Some(Inactive::Revoked)
        );
        assert_eq!(
            Inactive::check(false, Some(now), 5, Some(5), now),
            Some(Inactive::Expired)
        );
        assert_eq!(
            Inactive::check(false, None, 5, Some(5), now),
            Some(Inactive::Exhausted)
        );
        assert_eq!(Inactive::check(false, None, u64::MAX, None, now), None);
        // Uses beyond u32 still exhaust a u32 max use.
        assert_eq!(
            Inactive::check(false, None, u64::from(u32::MAX) + 1, Some(u32::MAX), now),
            Some(Inactive::Exhausted)
        );
    }

    #[test]
    fn description_is_trimmed_single_line_text_of_at_most_120_chars() {
        assert_eq!(
            Description::parse("  AI coding workshop  ")
                .unwrap()
                .as_str(),
            "AI coding workshop"
        );
        let at_limit = "\u{1f680}".repeat(120);
        assert_eq!(
            Description::parse(&format!("  {at_limit}  "))
                .unwrap()
                .as_str(),
            at_limit
        );
        assert_eq!(
            Description::parse(&"\u{1f680}".repeat(121)),
            Err(DescriptionError::TooLong)
        );
        for blank in ["", "   ", " \t ", "\n"] {
            assert_eq!(Description::parse(blank), Err(DescriptionError::Required));
        }
        for multi_line in ["Workshop\n", "Workshop\r", "Work\nshop", "\r\nWorkshop"] {
            assert_eq!(
                Description::parse(multi_line),
                Err(DescriptionError::MultiLine)
            );
        }
    }

    #[test]
    fn internal_note_is_trimmed_blank_means_none_and_at_most_16384_bytes() {
        assert_eq!(InternalNote::parse(""), Ok(None));
        assert_eq!(InternalNote::parse(" \t\n "), Ok(None));
        assert_eq!(
            InternalNote::parse("  First cohort\n  Keep indentation  \n")
                .unwrap()
                .unwrap()
                .as_str(),
            "First cohort\n  Keep indentation"
        );
        let at_limit = "x".repeat(INTERNAL_NOTE_MAX_BYTES);
        assert_eq!(
            InternalNote::parse(&format!(" {at_limit} "))
                .unwrap()
                .map(String::from),
            Some(at_limit.clone())
        );
        assert_eq!(
            InternalNote::parse(&format!("{at_limit}x")),
            Err(InternalNoteTooLong)
        );
        // Bytes, not characters: 4-byte characters reach the limit sooner.
        assert!(InternalNote::parse(&"\u{1f680}".repeat(4096)).is_ok());
        assert_eq!(
            InternalNote::parse(&"\u{1f680}".repeat(4097)),
            Err(InternalNoteTooLong)
        );
    }

    fn repo(repo_id: u64, full_name: &str) -> InvitationLinkRepo {
        InvitationLinkRepo {
            repo_id,
            repo_full_name: full_name.into(),
        }
    }

    fn repos(count: u64) -> Vec<InvitationLinkRepo> {
        (1..=count)
            .map(|id| repo(id, &format!("acme/repo-{id}")))
            .collect()
    }

    #[test]
    fn repository_scope_holds_one_to_100_repositories_sorted_by_id() {
        assert_eq!(
            RepositoryScope::parse(vec![]),
            Err(RepositoryScopeError::Empty)
        );
        assert_eq!(
            RepositoryScope::parse(repos(1)).unwrap().as_slice().len(),
            1
        );
        assert_eq!(
            RepositoryScope::parse(repos(100)).unwrap().as_slice().len(),
            100
        );
        assert_eq!(
            RepositoryScope::parse(repos(101)),
            Err(RepositoryScopeError::TooMany)
        );
        let scope = RepositoryScope::parse(vec![
            repo(30, "acme/c"),
            repo(10, "acme/a"),
            repo(20, "acme/b"),
        ])
        .unwrap();
        assert_eq!(
            Vec::from(scope),
            vec![repo(10, "acme/a"), repo(20, "acme/b"), repo(30, "acme/c")]
        );
    }

    #[test]
    fn repository_scope_rejects_duplicate_and_invalid_repositories() {
        assert_eq!(
            RepositoryScope::parse(vec![
                repo(10, "acme/a"),
                repo(20, "acme/b"),
                repo(10, "acme/c")
            ]),
            Err(RepositoryScopeError::Duplicate)
        );
        let long_name = format!("acme/{}", "r".repeat(REPOSITORY_FULL_NAME_MAX_BYTES - 5));
        assert!(RepositoryScope::parse(vec![repo(1, &long_name)]).is_ok());
        for invalid in [
            repo(0, "acme/a"),
            repo(1, "acme-a"),
            repo(1, "acme/a/b"),
            repo(1, &format!("{long_name}r")),
        ] {
            assert_eq!(
                RepositoryScope::parse(vec![invalid]),
                Err(RepositoryScopeError::InvalidRepository)
            );
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RotateSecret {
    pub link_id: InvitationLinkId,
    pub operation_id: crate::admission::AdmissionOperationId,
    pub admin: AccountAdmin,
    pub new_sha256: SecretHash,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SecretRotated {
    pub secret_generation: u32,
}
/// The ingress status with which the link authority refuses a command because
/// the link is revoked. The command changed nothing and bound no identity.
///
/// No other link answer uses it, so a caller can tell it from 409, which
/// always means the operation's identity is bound to different input.
pub const LINK_REVOKED_STATUS: u16 = 410;

/// Why the authority refuses to rotate a link's secret.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum RotationRefusal {
    #[error("link revoked")]
    Revoked,
    #[error("secret generation or link revision exhausted")]
    Exhausted,
}

impl RotationRefusal {
    /// The ingress status the link authority answers this refusal with.
    pub const fn status(self) -> u16 {
        match self {
            Self::Revoked => LINK_REVOKED_STATUS,
            Self::Exhausted => 400,
        }
    }
}
