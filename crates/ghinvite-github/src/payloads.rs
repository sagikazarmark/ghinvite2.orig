//! On-the-wire response shapes from the GitHub REST API. Field set is
//! deliberately minimal: anything ghinvite doesn't read is omitted (serde will
//! ignore extra keys by default).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// `GET /user` response.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct GhUser {
    pub id: u64,
    pub login: String,
    #[serde(default)]
    pub avatar_url: Option<String>,
    /// Some response shapes carry `type: "User"` / `"Organization"`. Optional
    /// because `/user` always returns "User"; included for shape-reuse where
    /// installation owners come back with the same field.
    #[serde(rename = "type", default)]
    pub account_type: Option<String>,
}

/// `GET /user/memberships/orgs/{login}` response. We only care about the
/// `role` and `state` fields for admin re-check.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct GhMembership {
    pub role: String,  // "admin" | "member"
    pub state: String, // "active" | "pending"
}

/// `GET /repos/{owner}/{repo}` (a small slice; we use it for display + access
/// confirmation).
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct GhRepo {
    pub id: u64,
    pub full_name: String,
    pub private: bool,
}

/// `POST /app/installations/{id}/access_tokens` response.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct GhInstallationToken {
    pub token: String,
    /// ISO-8601 timestamp ~1 hour out. Used by [`crate::token_cache::TokenCache`].
    pub expires_at: DateTime<Utc>,
}

/// `PUT /repos/{owner}/{repo}/collaborators/{username}` response shape on 201.
/// The `id` field is GitHub's `invitation_id` — what we store in our
/// `github_invitations.github_invitation_id` column.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct GhCollaboratorInvite {
    pub id: u64,
}

/// `GET /repos/{owner}/{repo}/invitations` (list, pagination ignored — v1 link
/// repo sets stay tiny).
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct GhInvitationListItem {
    pub id: u64,
    pub invitee: GhUser,
    pub permissions: String, // "read"|"write"|"admin"|"triage"|"maintain"
    pub created_at: DateTime<Utc>,
}

/// `GET /installation/repositories` response envelope.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct GhInstallationRepos {
    pub total_count: u64,
    pub repositories: Vec<GhRepo>,
}

/// `GET /user/installations` response envelope.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct GhUserInstallationList {
    pub total_count: u64,
    pub installations: Vec<GhUserInstallation>,
}

/// One GitHub App installation visible to a user access token.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct GhUserInstallation {
    pub id: u64,
    pub account: GhUser,
    pub repository_selection: String,
    pub target_type: String,
    pub target_id: u64,
}

/// OAuth code-exchange success payload. GitHub returns `application/json`
/// when `Accept: application/json` is set.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct GhTokenResponse {
    pub access_token: String,
    pub token_type: String, // "bearer"
    pub scope: String,      // space-separated, e.g. "read:user"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_decodes_minimum_payload() {
        let raw = br#"{"id":42,"login":"octocat","avatar_url":"https://avatars.example/u/42","extra":"ignored"}"#;
        let u: GhUser = serde_json::from_slice(raw).unwrap();
        assert_eq!(u.id, 42);
        assert_eq!(u.login, "octocat");
        assert!(u.account_type.is_none());
    }

    #[test]
    fn membership_decodes() {
        let raw = br#"{"role":"admin","state":"active"}"#;
        let m: GhMembership = serde_json::from_slice(raw).unwrap();
        assert_eq!(m.role, "admin");
    }

    #[test]
    fn installation_token_decodes_expires_at() {
        let raw = br#"{"token":"ghs_xxx","expires_at":"2026-05-04T13:00:00Z"}"#;
        let t: GhInstallationToken = serde_json::from_slice(raw).unwrap();
        assert_eq!(t.token, "ghs_xxx");
        assert_eq!(t.expires_at.to_rfc3339(), "2026-05-04T13:00:00+00:00");
    }

    #[test]
    fn collaborator_invite_decodes_id_only() {
        let raw =
            br#"{"id":98765,"invitee":{"id":42,"login":"x"},"repository":{"full_name":"a/b"}}"#;
        let inv: GhCollaboratorInvite = serde_json::from_slice(raw).unwrap();
        assert_eq!(inv.id, 98765);
    }

    #[test]
    fn installation_repos_decodes() {
        let raw = br#"{"total_count":2,"repositories":[
            {"id":1,"full_name":"a/b","private":true},
            {"id":2,"full_name":"a/c","private":false}
        ]}"#;
        let r: GhInstallationRepos = serde_json::from_slice(raw).unwrap();
        assert_eq!(r.total_count, 2);
        assert_eq!(r.repositories.len(), 2);
        assert_eq!(r.repositories[0].full_name, "a/b");
    }

    #[test]
    fn user_installation_list_decodes_org_installation() {
        let raw = br#"{
            "total_count": 1,
            "installations": [{
                "id": 77,
                "account": {
                    "id": 9001,
                    "login": "acme",
                    "avatar_url": "https://avatars.example/u/9001",
                    "type": "Organization"
                },
                "repository_selection": "selected",
                "target_type": "Organization",
                "target_id": 9001
            }]
        }"#;
        let list: GhUserInstallationList = serde_json::from_slice(raw).unwrap();
        assert_eq!(list.total_count, 1);
        assert_eq!(list.installations[0].id, 77);
        assert_eq!(list.installations[0].account.login, "acme");
        assert_eq!(
            list.installations[0].account.account_type.as_deref(),
            Some("Organization")
        );
        assert_eq!(list.installations[0].repository_selection, "selected");
    }

    #[test]
    fn user_installation_list_decodes_user_installation() {
        let raw = br#"{
            "total_count": 1,
            "installations": [{
                "id": 88,
                "account": {"id": 42, "login": "octocat", "type": "User"},
                "repository_selection": "all",
                "target_type": "User",
                "target_id": 42
            }]
        }"#;
        let list: GhUserInstallationList = serde_json::from_slice(raw).unwrap();
        assert_eq!(list.installations[0].id, 88);
        assert_eq!(list.installations[0].account.login, "octocat");
        assert_eq!(list.installations[0].repository_selection, "all");
        assert_eq!(list.installations[0].target_type, "User");
    }

    #[test]
    fn token_response_decodes() {
        let raw = br#"{"access_token":"u_xxx","token_type":"bearer","scope":"read:user"}"#;
        let t: GhTokenResponse = serde_json::from_slice(raw).unwrap();
        assert_eq!(t.access_token, "u_xxx");
    }
}
