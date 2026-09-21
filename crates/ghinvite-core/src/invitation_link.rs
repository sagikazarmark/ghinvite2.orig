use crate::ids::InvitationLinkId;
use crate::permission::Permission;
use crate::repository_identity::RepositoryIdentity;
use crate::slug::Slug;
use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InvitationLink {
    pub id: InvitationLinkId,
    pub slug: Slug,
    pub installation_id: u64,
    pub account_id: u64,
    pub created_by: u64, // user_id
    pub created_at: DateTime<Utc>,
    pub expires_at: Option<DateTime<Utc>>,
    pub max_uses: Option<u32>,
    pub uses_count: u32,
    pub permission: Permission,
    pub approval_required: bool,
    pub description: String,
    pub internal_note: Option<String>,
    pub revoked_at: Option<DateTime<Utc>>,
    pub revoked_by: Option<u64>,
    pub repos: Vec<InvitationLinkRepo>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct InvitationLinkRepo {
    pub repo_id: u64,
    pub repo_full_name: String,
}

impl InvitationLink {
    /// `is_active` is the per-spec derived state: not revoked AND not expired AND not exhausted.
    pub fn is_active(&self, now: DateTime<Utc>) -> bool {
        Inactive::check(
            self.revoked_at.is_some(),
            self.expires_at,
            u64::from(self.uses_count),
            self.max_uses,
            now,
        )
        .is_none()
    }
}

/// Why an invitation link no longer accepts new invitation requests.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Inactive {
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
    use rand::SeedableRng;
    use rand_chacha::ChaCha8Rng;

    fn at(y: i32, mo: u32, d: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, mo, d, 0, 0, 0).unwrap()
    }

    fn base_link() -> InvitationLink {
        let mut rng = ChaCha8Rng::seed_from_u64(20_260_504);
        InvitationLink {
            id: InvitationLinkId::new(),
            slug: Slug::generate(&mut rng),
            installation_id: 1,
            account_id: 2,
            created_by: 3,
            created_at: at(2026, 1, 1),
            expires_at: None,
            max_uses: None,
            uses_count: 0,
            permission: Permission::Pull,
            approval_required: false,
            description: "AI coding workshop".into(),
            internal_note: None,
            revoked_at: None,
            revoked_by: None,
            repos: vec![],
        }
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
