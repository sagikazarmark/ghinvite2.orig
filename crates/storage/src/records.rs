//! Row-shaped intermediate types for the sqlx impl. Each has a 1:1 column
//! mapping; conversion to domain types happens in named `try_into_domain`
//! methods (so the conversion is explicit, not magic).

use crate::Error;
use audit::{ActorKind, AuditEvent, EventType, TargetKind};
use chrono::{DateTime, Utc};
use domain::{
    Account, AccountType, AuditEventId, GithubInvitation, GithubInvitationId, InvitationLink,
    InvitationLinkId, InvitationLinkRepo, InvitationRequest, InvitationState, RequestId,
    RequestState, SelectedRepos, Slug, User,
};
use sqlx::FromRow;
use std::str::FromStr;
use ulid::Ulid;

/// Centralized `u64 → i64` cast for SQLite bindings. SQLite has no unsigned
/// integers, so every `u64` we store rides as `i64`. The `debug_assert!` is the
/// safety net for development; production never sees `u64` values above
/// `i64::MAX` from GitHub's id space.
#[inline]
pub(crate) fn u64_to_i64(v: u64) -> i64 {
    debug_assert!(v <= i64::MAX as u64, "u64 value {v} exceeds i64::MAX");
    v as i64
}

#[derive(FromRow, Debug)]
pub struct InstallationRow {
    pub installation_id: i64,
    pub account_id: i64,
    pub account_login: String,
    pub account_type: String,
    pub installed_at: DateTime<Utc>,
    pub uninstalled_at: Option<DateTime<Utc>>,
    pub selected_repos: String,
}

impl InstallationRow {
    pub fn try_into_domain(self) -> Result<Account, Error> {
        Ok(Account {
            installation_id: self.installation_id as u64,
            account_id: self.account_id as u64,
            account_login: self.account_login,
            account_type: AccountType::from_str(&self.account_type)
                .map_err(|e| Error::Corrupt(e.to_string()))?,
            installed_at: self.installed_at,
            uninstalled_at: self.uninstalled_at,
            selected_repos: parse_selected_repos(&self.selected_repos)?,
        })
    }
}

pub fn parse_selected_repos(raw: &str) -> Result<SelectedRepos, Error> {
    if raw == "all" {
        return Ok(SelectedRepos::All);
    }
    let v: Vec<u64> = serde_json::from_str(raw)
        .map_err(|e| Error::Corrupt(format!("selected_repos JSON: {e}")))?;
    Ok(SelectedRepos::Subset(v))
}

pub fn encode_selected_repos(s: &SelectedRepos) -> String {
    match s {
        SelectedRepos::All => "all".to_string(),
        SelectedRepos::Subset(v) => serde_json::to_string(v).expect("vec<u64> serializes"),
    }
}

#[derive(FromRow, Debug)]
pub struct UserRow {
    pub user_id: i64,
    pub login: String,
    pub avatar_url: Option<String>,
    pub last_seen_at: DateTime<Utc>,
}

impl UserRow {
    pub fn into_domain(self) -> User {
        User {
            user_id: self.user_id as u64,
            login: self.login,
            avatar_url: self.avatar_url,
            last_seen_at: self.last_seen_at,
        }
    }
}

#[derive(FromRow, Debug)]
pub struct InvitationLinkRow {
    pub id: String,
    pub slug: String,
    pub installation_id: i64,
    pub account_id: i64,
    pub created_by: i64,
    pub created_at: DateTime<Utc>,
    pub expires_at: Option<DateTime<Utc>>,
    pub max_uses: Option<i64>,
    pub uses_count: i64,
    pub permission: String,
    pub approval_required: i64,
    pub description: String,
    pub internal_note: Option<String>,
    pub revoked_at: Option<DateTime<Utc>>,
    pub revoked_by: Option<i64>,
}

impl InvitationLinkRow {
    pub fn try_into_domain(self, repos: Vec<InvitationLinkRepo>) -> Result<InvitationLink, Error> {
        Ok(InvitationLink {
            id: InvitationLinkId::from_ulid(
                Ulid::from_str(&self.id).map_err(|e| Error::Corrupt(format!("link id: {e}")))?,
            ),
            slug: Slug::from_string(self.slug).map_err(|e| Error::Corrupt(format!("slug: {e}")))?,
            installation_id: self.installation_id as u64,
            account_id: self.account_id as u64,
            created_by: self.created_by as u64,
            created_at: self.created_at,
            expires_at: self.expires_at,
            max_uses: self.max_uses.map(|m| m as u32),
            uses_count: self.uses_count as u32,
            permission: self.permission.parse().map_err(
                |e: domain::permission::UnknownPermission| Error::Corrupt(e.to_string()),
            )?,
            approval_required: self.approval_required != 0,
            description: self.description,
            internal_note: self.internal_note,
            revoked_at: self.revoked_at,
            revoked_by: self.revoked_by.map(|r| r as u64),
            repos,
        })
    }
}

/// Flat row shape for `invitation_links LEFT JOIN invitation_link_repos` queries.
/// Splits into a `InvitationLinkRow` plus optional repo fields via [`Self::split`].
#[derive(FromRow, Debug)]
pub struct InvitationLinkJoinRow {
    pub id: String,
    pub slug: String,
    pub installation_id: i64,
    pub account_id: i64,
    pub created_by: i64,
    pub created_at: DateTime<Utc>,
    pub expires_at: Option<DateTime<Utc>>,
    pub max_uses: Option<i64>,
    pub uses_count: i64,
    pub permission: String,
    pub approval_required: i64,
    pub description: String,
    pub internal_note: Option<String>,
    pub revoked_at: Option<DateTime<Utc>>,
    pub revoked_by: Option<i64>,
    pub repo_id: Option<i64>,
    pub repo_full_name: Option<String>,
}

impl InvitationLinkJoinRow {
    pub fn split(self) -> (InvitationLinkRow, Option<i64>, Option<String>) {
        (
            InvitationLinkRow {
                id: self.id,
                slug: self.slug,
                installation_id: self.installation_id,
                account_id: self.account_id,
                created_by: self.created_by,
                created_at: self.created_at,
                expires_at: self.expires_at,
                max_uses: self.max_uses,
                uses_count: self.uses_count,
                permission: self.permission,
                approval_required: self.approval_required,
                description: self.description,
                internal_note: self.internal_note,
                revoked_at: self.revoked_at,
                revoked_by: self.revoked_by,
            },
            self.repo_id,
            self.repo_full_name,
        )
    }
}

#[derive(FromRow, Debug)]
pub struct InvitationLinkRepoRow {
    pub invitation_link_id: String,
    pub repo_id: i64,
    pub repo_full_name: String,
}

impl InvitationLinkRepoRow {
    pub fn into_domain(self) -> InvitationLinkRepo {
        InvitationLinkRepo {
            repo_id: self.repo_id as u64,
            repo_full_name: self.repo_full_name,
        }
    }
}

#[derive(FromRow, Debug)]
pub struct InvitationRequestRow {
    pub id: String,
    pub invitation_link_id: String,
    pub requester_id: i64,
    pub justification: Option<String>,
    pub state: String,
    pub decided_by: Option<i64>,
    pub decided_at: Option<DateTime<Utc>>,
    pub decline_reason: Option<String>,
    pub created_at: DateTime<Utc>,
}

impl InvitationRequestRow {
    pub fn try_into_domain(self) -> Result<InvitationRequest, Error> {
        Ok(InvitationRequest {
            id: RequestId::from_ulid(
                Ulid::from_str(&self.id).map_err(|e| Error::Corrupt(format!("req id: {e}")))?,
            ),
            invitation_link_id: InvitationLinkId::from_ulid(
                Ulid::from_str(&self.invitation_link_id)
                    .map_err(|e| Error::Corrupt(format!("link id: {e}")))?,
            ),
            requester_id: self.requester_id as u64,
            justification: self.justification,
            state: RequestState::from_str(&self.state)
                .map_err(|e| Error::Corrupt(e.to_string()))?,
            decided_by: self.decided_by.map(|d| d as u64),
            decided_at: self.decided_at,
            decline_reason: self.decline_reason,
            created_at: self.created_at,
        })
    }
}

#[derive(FromRow, Debug)]
pub struct GithubInvitationRow {
    pub id: String,
    pub invitation_request_id: String,
    pub repo_id: i64,
    pub github_invitation_id: Option<i64>,
    pub state: String,
    pub error_message: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl GithubInvitationRow {
    pub fn try_into_domain(self) -> Result<GithubInvitation, Error> {
        Ok(GithubInvitation {
            id: GithubInvitationId::from_ulid(
                Ulid::from_str(&self.id).map_err(|e| Error::Corrupt(format!("ginv id: {e}")))?,
            ),
            invitation_request_id: RequestId::from_ulid(
                Ulid::from_str(&self.invitation_request_id)
                    .map_err(|e| Error::Corrupt(format!("req id: {e}")))?,
            ),
            repo_id: self.repo_id as u64,
            github_invitation_id: self.github_invitation_id.map(|g| g as u64),
            state: InvitationState::from_str(&self.state)
                .map_err(|e| Error::Corrupt(e.to_string()))?,
            error_message: self.error_message,
            created_at: self.created_at,
            updated_at: self.updated_at,
        })
    }
}

#[derive(FromRow, Debug)]
pub struct AuditEventRow {
    pub id: String,
    pub account_id: i64,
    pub occurred_at: DateTime<Utc>,
    pub event_type: String,
    pub actor_kind: String,
    pub actor_id: Option<i64>,
    pub target_kind: String,
    pub target_id: String,
    pub metadata: Option<String>,
    pub request_id: Option<String>,
}

impl AuditEventRow {
    pub fn try_into_domain(self) -> Result<AuditEvent, Error> {
        Ok(AuditEvent {
            id: AuditEventId::from_ulid(
                Ulid::from_str(&self.id).map_err(|e| Error::Corrupt(format!("audit id: {e}")))?,
            ),
            account_id: self.account_id as u64,
            occurred_at: self.occurred_at,
            event_type: EventType::from_str(&self.event_type)
                .map_err(|e| Error::Corrupt(e.to_string()))?,
            actor_kind: ActorKind::from_str(&self.actor_kind)
                .map_err(|e| Error::Corrupt(e.to_string()))?,
            actor_id: self.actor_id.map(|a| a as u64),
            target_kind: TargetKind::from_str(&self.target_kind)
                .map_err(|e| Error::Corrupt(e.to_string()))?,
            target_id: self.target_id,
            metadata: match self.metadata {
                None => serde_json::Value::Null,
                Some(s) => serde_json::from_str(&s)
                    .map_err(|e| Error::Corrupt(format!("audit metadata: {e}")))?,
            },
            request_id: self.request_id,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_selected_repos_all() {
        match parse_selected_repos("all").unwrap() {
            SelectedRepos::All => (),
            _ => panic!("expected All"),
        }
    }

    #[test]
    fn parse_selected_repos_subset() {
        match parse_selected_repos("[1,2,3]").unwrap() {
            SelectedRepos::Subset(v) => assert_eq!(v, vec![1, 2, 3]),
            _ => panic!("expected Subset"),
        }
    }

    #[test]
    fn parse_selected_repos_rejects_garbage() {
        let err = parse_selected_repos("not json").unwrap_err();
        match err {
            Error::Corrupt(_) => (),
            other => panic!("expected Corrupt, got {other:?}"),
        }
    }

    #[test]
    fn encode_selected_repos_round_trip() {
        for s in [
            SelectedRepos::All,
            SelectedRepos::Subset(vec![]),
            SelectedRepos::Subset(vec![10, 20, 30]),
        ] {
            let encoded = encode_selected_repos(&s);
            let decoded = parse_selected_repos(&encoded).unwrap();
            assert_eq!(decoded, s);
        }
    }

    #[test]
    fn u64_to_i64_round_trips_normal_values() {
        assert_eq!(u64_to_i64(0), 0);
        assert_eq!(u64_to_i64(42), 42);
        assert_eq!(u64_to_i64(i64::MAX as u64), i64::MAX);
    }
}
