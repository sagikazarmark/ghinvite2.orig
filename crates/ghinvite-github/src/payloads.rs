//! On-the-wire response shapes from the GitHub REST API. Field set is
//! deliberately minimal: anything ghinvite doesn't read is omitted (serde will
//! ignore extra keys by default).
//!
//! ghinvite persists `GhUser` and `GhAppInstallation` (authority cache,
//! Restate journal): their serde attributes are gh-toolkit's, unchanged.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// `GET /user` response.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct GhUser {
    /// Stable numeric GitHub account identifier; use this rather than login for binding identity.
    pub id: u64,
    /// Current provider login, which may change after a rename.
    pub login: String,
    #[serde(default)]
    /// Optional provider avatar locator; use the bounded avatar API rather than forwarding arbitrary response URLs.
    pub avatar_url: Option<String>,
    #[serde(
        default,
        deserialize_with = "advisory_time",
        skip_serializing_if = "Option::is_none"
    )]
    /// Account creation time when supplied by the endpoint; absence remains unknown.
    pub created_at: Option<DateTime<Utc>>,
    /// Some response shapes carry `type: "User"` / `"Organization"`. Optional
    /// because `/user` always returns "User"; included for shape-reuse where
    /// installation owners come back with the same field.
    #[serde(rename = "type", default)]
    pub account_type: Option<String>,
}

/// `GET /repos/{owner}/{repo}` (a small slice; we use it for display + access
/// confirmation).
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct GhRepo {
    /// Stable numeric GitHub repository identifier.
    pub id: u64,
    /// Current owner/name path; it is not a durable repository identity.
    pub full_name: String,
    /// Whether GitHub currently marks this repository private.
    pub private: bool,
}

/// `PUT /repos/{owner}/{repo}/collaborators/{username}` response shape on 201.
/// The `id` field is GitHub's `invitation_id` — what we store in our
/// `github_invitations.github_invitation_id` column.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct GhCollaboratorInvite {
    /// Numeric upstream invitation identifier.
    pub id: u64,
    #[serde(default, deserialize_with = "advisory_time")]
    /// Provider creation time when present; absence must not be replaced with the local request time.
    pub created_at: Option<DateTime<Utc>>,
}

// Invalid optional display metadata cannot erase a successful invitation receipt.
pub(crate) fn advisory_time<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<Option<DateTime<Utc>>, D::Error> {
    let value = serde_json::Value::deserialize(d)?;
    Ok(serde_json::from_value(value).ok())
}

/// `GET /installation/repositories` response envelope.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct GhInstallationRepos {
    /// Provider-reported total, not proof that this page contains every repository.
    pub total_count: u64,
    /// Repositories on this response page.
    pub repositories: Vec<GhRepo>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
/// Current app installation facts for exact app, account and suspension validation.
pub struct GhAppInstallation {
    /// Numeric installation identifier.
    pub id: u64,
    /// Numeric app identifier; validate this against the configured app slot.
    pub app_id: u64,
    #[serde(deserialize_with = "installation_timestamp")]
    /// Provider installation creation time.
    pub created_at: DateTime<Utc>,
    #[serde(deserialize_with = "installation_timestamp")]
    /// Provider installation last-update time.
    pub updated_at: DateTime<Utc>,
    /// Installed account, including stable numeric identity.
    pub account: GhUser,
    /// Whether installation access covers all repositories or an explicit selection.
    pub repository_selection: GhRepositorySelection,
    /// Granted permission names and access levels reported by GitHub; requested permissions are not equivalent to grants.
    pub permissions: std::collections::BTreeMap<String, GhInstallationPermission>,
    /// Positive suspension time; when present the installation cannot authorize normal app actions.
    pub suspended_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
/// Repository-selection scope reported for an installation.
pub enum GhRepositorySelection {
    /// The installation is configured for all repositories in its account.
    All,
    /// The installation is configured for an explicit repository selection.
    Selected,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
/// Granted access level for a named installation permission.
pub enum GhInstallationPermission {
    /// The named permission allows reads.
    Read,
    /// The named permission allows writes as well as its corresponding reads.
    Write,
}

fn installation_timestamp<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<DateTime<Utc>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Timestamp {
        Text(DateTime<Utc>),
        Epoch(i64),
    }
    match Timestamp::deserialize(deserializer)? {
        Timestamp::Text(value) => Ok(value),
        Timestamp::Epoch(value) => DateTime::from_timestamp(value, 0)
            .ok_or_else(|| serde::de::Error::custom("invalid installation timestamp")),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn verified_installation_requires_scope_and_application_identity() {
        let complete = serde_json::json!({
            "id":9,"app_id":1,"created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-01T00:00:00Z","account":{"id":100,"login":"acme"},
            "repository_selection":"selected", "permissions":{"metadata":"read"},
            "suspended_at":null
        });
        let parsed: super::GhAppInstallation = serde_json::from_value(complete.clone()).unwrap();
        assert_eq!(parsed.app_id, 1);
        assert_eq!(
            parsed.repository_selection,
            super::GhRepositorySelection::Selected
        );
        for field in [
            "app_id",
            "created_at",
            "updated_at",
            "repository_selection",
            "permissions",
        ] {
            let mut missing = complete.clone();
            missing.as_object_mut().unwrap().remove(field);
            assert!(serde_json::from_value::<super::GhAppInstallation>(missing).is_err());
        }
        let mut numeric = complete.clone();
        numeric["created_at"] = serde_json::json!(1767225600);
        numeric["updated_at"] = serde_json::json!(1767225600);
        let numeric: super::GhAppInstallation = serde_json::from_value(numeric).unwrap();
        assert_eq!(numeric.created_at, parsed.created_at);
        assert_eq!(numeric.updated_at, parsed.updated_at);
        let mut unknown = complete;
        unknown["permissions"]["metadata"] = serde_json::json!("admin");
        assert!(serde_json::from_value::<super::GhAppInstallation>(unknown).is_err());
    }
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
}

#[cfg(test)]
mod invitation_clock_tests {
    use super::*;
    #[test]
    fn optional_invitation_clock_cannot_erase_positive_write_receipt() {
        for raw in [
            r#"{"id":99}"#,
            r#"{"id":99,"created_at":"invalid"}"#,
            r#"{"id":99,"created_at":42}"#,
        ] {
            let value: GhCollaboratorInvite = serde_json::from_str(raw).unwrap();
            assert_eq!(value.id, 99);
            assert!(value.created_at.is_none());
        }
        let value: GhCollaboratorInvite =
            serde_json::from_str(r#"{"id":99,"created_at":"2026-10-17T12:00:00Z"}"#).unwrap();
        assert!(value.created_at.is_some());
    }
}
