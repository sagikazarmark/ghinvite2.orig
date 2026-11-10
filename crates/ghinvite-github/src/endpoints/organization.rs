//! Organization/team wire contracts. These functions perform one HTTP attempt.
//! Callers own numeric identity binding, role precedence, budgets and write fences.
//! Failed-invitation visibility/expiry meanings still require the live SP-08/SP-18 probes.

use super::{Endpoint, call};
use crate::{
    error::ApiError,
    payloads::GhUser,
    rest::{ApiResponse, Rest},
    roles::{MembershipState, OrgRole, TeamRole},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
/// Organization membership facts with explicit role, state and numeric identities.
pub struct OrganizationMembership {
    /// Provider organization role; unknown variants must not grant authority.
    pub role: OrgRole,
    /// Active or pending membership state; pending is not active membership.
    pub state: MembershipState,
    /// User whose numeric identity must match the requested actor.
    pub user: GhUser,
    /// Organization whose numeric identity must match the requested account.
    pub organization: GhUser,
}
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
/// Current membership facts for one team and user.
pub struct TeamMembership {
    /// Provider team role; a stronger role must not be inferred from membership alone.
    pub role: TeamRole,
    /// Active or pending team membership state.
    pub state: MembershipState,
}
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
/// Team identity returned by the organization endpoints.
pub struct Team {
    /// Positive LDAP mapping only; absent is not proof that Cloud team sync is off.
    #[serde(default, rename = "ldap_dn", deserialize_with = "ldap_mapped")]
    pub externally_managed: bool,
    /// Stable numeric team identifier, independent of its slug.
    pub id: u64,
    /// Current team path component, which can change after a rename.
    pub slug: String,
    /// Current human-readable team name.
    pub name: String,
}

/// Missing billing facts remain unknown; callers must not infer paid seats or
/// established status from the absence of fields available only to some tokens.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct Organization {
    /// Stable numeric organization identifier.
    pub id: u64,
    /// Current organization login, used for endpoint routing after numeric binding.
    pub login: String,
    /// Provider organization creation time.
    pub created_at: DateTime<Utc>,
    /// Plan details when visible to the current credential; absence is not evidence of free capacity.
    pub plan: Option<OrganizationPlan>,
    /// Whether GitHub reports an organization-wide two-factor requirement; absent data remains unknown.
    pub two_factor_requirement_enabled: Option<bool>,
    /// Base repository permission when visible; it is not proof that a particular requester is already a member.
    pub default_repository_permission: Option<BasePermission>,
}
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
/// Organization-wide default repository access reported by GitHub.
pub enum BasePermission {
    /// Members receive no repository permission solely from the organization default.
    None,
    /// The organization default grants repository read access to existing members.
    Read,
    /// The organization default grants repository write access to existing members.
    Write,
    /// The organization default grants repository administration to existing members.
    Admin,
    #[serde(other)]
    /// An unsupported provider value; callers must not promote it into an authorization claim.
    Unknown,
}
fn ldap_mapped<'de, D: serde::Deserializer<'de>>(d: D) -> Result<bool, D::Error> {
    Ok(Option::<String>::deserialize(d)?.is_some_and(|dn| !dn.trim().is_empty()))
}
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
/// Optional organization billing facts; these may be hidden by the credential's permissions.
pub struct OrganizationPlan {
    /// Provider plan name, used for display rather than authorization.
    pub name: String,
    /// Seat allowance when disclosed; absence must not be treated as unlimited capacity.
    pub seats: Option<u64>,
    /// Occupied seats when disclosed; absence must not be treated as zero usage.
    pub filled_seats: Option<u64>,
}
impl OrganizationPlan {
    /// Positive paid capacity only. Missing/private billing fields and free
    /// plans must not be interpreted as exhausted seats.
    pub fn paid_seats(&self) -> Option<(u64, u64)> {
        if self.name.is_empty() || self.name.eq_ignore_ascii_case("free") {
            return None;
        }
        self.seats
            .zip(self.filled_seats)
            .filter(|(seats, _)| *seats > 0)
    }
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
/// Role recorded on an existing organization invitation.
pub enum InvitationRole {
    /// An invitation to become an ordinary organization member.
    DirectMember,
    /// An invitation to become an organization owner.
    Admin,
    /// An invitation for the billing-manager role.
    BillingManager,
    /// A provider-reported hiring-manager invitation.
    HiringManager,
}
/// Create roles differ from observed historical roles. P1 chooses DirectMember;
/// reinstatement and privilege elevation are never defaults.
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum CreateInvitationRole {
    /// Request ordinary organization membership.
    DirectMember,
    /// Request organization-owner membership; applications must authorize this privilege separately.
    Admin,
    /// Request the billing-manager role.
    BillingManager,
    /// Request restoration of a former member's prior organization access.
    Reinstate,
}

/// An invitation addresses exactly one identity; it cannot contain both fields.
#[derive(Clone, Serialize)]
#[serde(untagged)]
pub enum Invitee {
    /// Address the invitation to a known numeric GitHub user.
    User {
        /// Stable numeric identity of the intended invitee.
        invitee_id: u64,
    },
    /// Address the invitation to an email mailbox; prefer the zeroizing email API for sensitive delivery paths.
    Email {
        /// Mailbox used by this serializable request DTO; callers are responsible for avoiding unsafe persistence.
        email: String,
    },
}
#[derive(Clone, Debug, Serialize)]
/// Explicit recipient, role and teams for a new organization invitation.
pub struct OrganizationInvitationInput {
    #[serde(flatten)]
    /// Recipient identity chosen by the caller.
    pub invitee: Invitee,
    /// Requested role; the toolkit does not decide whether the actor may grant it.
    pub role: CreateInvitationRole,
    /// Numeric team identifiers to attach to the invitation; membership is not confirmed by submission alone.
    pub team_ids: Vec<u64>,
}
/// GitHub invitation IDs are not invitee user IDs. A login/email alone must not
/// be used as numeric identity evidence when resolving an uncertain write.
#[derive(Clone, Deserialize, PartialEq, Eq)]
pub struct OrganizationInvitation {
    /// Numeric upstream organization invitation identifier.
    pub id: u64,
    /// Provider-resolved login when available; email invitations may not expose one.
    pub login: Option<String>,
    /// Provider-returned mailbox when available; treat this as personal data.
    pub email: Option<String>,
    /// Role recorded by the provider for this invitation.
    pub role: InvitationRole,
    /// Provider invitation creation time, used as timing evidence.
    pub created_at: DateTime<Utc>,
    /// Account that created the invitation.
    pub inviter: GhUser,
    /// Provider-reported number of attached teams; enumerate teams when exact membership evidence is required.
    pub team_count: u64,
}
/// Failed-list entries are retained separately from pending entries. Raw reason
/// prose is deliberately not exposed as an outcome; interpreting expiry versus
/// cancellation remains an app policy gated on SP-08/SP-18 evidence.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct FailedOrganizationInvitation {
    #[serde(flatten)]
    /// Invitation facts associated with the failed delivery.
    pub invitation: OrganizationInvitation,
    #[serde(default, deserialize_with = "optional_failed_at")]
    /// Provider failure time when supplied; absence remains unknown.
    pub failed_at: Option<DateTime<Utc>>,
}

// GitHub's documented failed-list example contains an empty failed_at. Missing,
// null and blank therefore mean unknown, never an inferred expiration time.
fn optional_failed_at<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<DateTime<Utc>>, D::Error> {
    let raw = Option::<String>::deserialize(deserializer)?;
    raw.filter(|s| !s.is_empty())
        .map(|s| s.parse().map_err(serde::de::Error::custom))
        .transpose()
}

fn status<T>(response: ApiResponse<T>, expected: u16) -> Result<T, ApiError> {
    if response.status == expected {
        Ok(response.body)
    } else {
        Err(ApiError::Decode {
            what: "organization_status",
        })
    }
}
/// Read organization facts without inferring hidden billing or security fields.
pub async fn get_organization(rest: &Rest<'_>, org: &str) -> Result<Organization, ApiError> {
    status(
        call::<(), _>(rest, Endpoint::GetOrg, &[org], None).await?,
        200,
    )
}
/// Read one user's organization membership, preserving pending versus active state.
pub async fn organization_membership(
    rest: &Rest<'_>,
    org: &str,
    login: &str,
) -> Result<OrganizationMembership, ApiError> {
    status(
        call::<(), _>(rest, Endpoint::GetOrgMembership, &[org, login], None).await?,
        200,
    )
}
/// Exhaustively enumerate pending organization invitations; an incomplete read is an error.
pub async fn list_invitations(
    rest: &Rest<'_>,
    org: &str,
) -> Result<Vec<OrganizationInvitation>, ApiError> {
    let e = Endpoint::ListOrgInvitations;
    rest.list_all(e, e.meta().path.seg(org), &[]).await
}
/// Exhaustively enumerate failed organization invitations; this does not establish acceptance of absent invitations.
pub async fn list_failed_invitations(
    rest: &Rest<'_>,
    org: &str,
) -> Result<Vec<FailedOrganizationInvitation>, ApiError> {
    let e = Endpoint::ListFailedOrgInvitations;
    rest.list_all(e, e.meta().path.seg(org), &[]).await
}
/// Create an organization invitation using explicit recipient, role and team identifiers.
pub async fn create_invitation(
    rest: &Rest<'_>,
    org: &str,
    input: &OrganizationInvitationInput,
) -> Result<OrganizationInvitation, ApiError> {
    status(
        call(rest, Endpoint::CreateOrgInvitation, &[org], Some(input)).await?,
        201,
    )
}
/// Cancel a specific upstream organization invitation; ambiguous transport outcomes require reconciliation.
pub async fn cancel_invitation(rest: &Rest<'_>, org: &str, id: u64) -> Result<(), ApiError> {
    status(
        call::<(), _>(
            rest,
            Endpoint::CancelOrgInvitation,
            &[org, &id.to_string()],
            None,
        )
        .await?,
        204,
    )
}
/// Remove a specific user's organization membership; the toolkit does not decide ownership or removal policy.
pub async fn remove_organization_member(
    rest: &Rest<'_>,
    org: &str,
    login: &str,
) -> Result<(), ApiError> {
    status(
        call::<(), _>(rest, Endpoint::RemoveOrgMember, &[org, login], None).await?,
        204,
    )
}
/// Exhaustively enumerate teams for the organization.
pub async fn list_teams(rest: &Rest<'_>, org: &str) -> Result<Vec<Team>, ApiError> {
    let e = Endpoint::ListOrgTeams;
    rest.list_all(e, e.meta().path.seg(org), &[]).await
}
/// Read a team by its current slug; bind the returned numeric identity before using it as authority.
pub async fn get_team(rest: &Rest<'_>, org: &str, slug: &str) -> Result<Team, ApiError> {
    status(
        call::<(), _>(rest, Endpoint::GetTeam, &[org, slug], None).await?,
        200,
    )
}
/// Read a user's membership in a specific team, preserving its role and state.
pub async fn team_membership(
    rest: &Rest<'_>,
    org: &str,
    slug: &str,
    login: &str,
) -> Result<TeamMembership, ApiError> {
    status(
        call::<(), _>(rest, Endpoint::GetTeamMembership, &[org, slug, login], None).await?,
        200,
    )
}
/// No role default: the caller must observe and preserve existing maintainers.
pub async fn put_team_membership(
    rest: &Rest<'_>,
    org: &str,
    slug: &str,
    login: &str,
    role: TeamRole,
) -> Result<TeamMembership, ApiError> {
    #[derive(Serialize)]
    struct Body {
        role: TeamRole,
    }
    status(
        call(
            rest,
            Endpoint::PutTeamMembership,
            &[org, slug, login],
            Some(&Body { role }),
        )
        .await?,
        200,
    )
}
/// Remove a user's team membership without inferring whether the application owns that access.
pub async fn remove_team_membership(
    rest: &Rest<'_>,
    org: &str,
    slug: &str,
    login: &str,
) -> Result<(), ApiError> {
    status(
        call::<(), _>(
            rest,
            Endpoint::RemoveTeamMembership,
            &[org, slug, login],
            None,
        )
        .await?,
        204,
    )
}

/// Complete invitation-team evidence; callers bind numeric IDs to immutable targets.
pub async fn list_invitation_teams(
    rest: &Rest<'_>,
    org: &str,
    invitation_id: u64,
) -> Result<Vec<Team>, ApiError> {
    let e = Endpoint::ListOrgInvitationTeams;
    rest.list_all(
        e,
        e.meta().path.seg(org).seg(&invitation_id.to_string()),
        &[],
    )
    .await
}

impl std::fmt::Debug for Invitee {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::User { invitee_id } => f
                .debug_struct("User")
                .field("invitee_id", invitee_id)
                .finish(),
            Self::Email { .. } => f.write_str("Email([REDACTED])"),
        }
    }
}
impl std::fmt::Debug for OrganizationInvitation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OrganizationInvitation")
            .field("id", &self.id)
            .field("role", &self.role)
            .field("created_at", &self.created_at)
            .field("team_count", &self.team_count)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod eligibility_tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn optional_facts_never_infer_capacity_or_unknown_base_permissions() {
        let bare = json!({"id":1,"login":"org","created_at":"2020-01-01T00:00:00Z"});
        let org: Organization = serde_json::from_value(bare.clone()).unwrap();
        assert!(org.plan.is_none());
        assert!(org.two_factor_requirement_enabled.is_none());
        assert!(org.default_repository_permission.is_none());
        let mut future = bare;
        future["default_repository_permission"] = json!("future_role");
        assert_eq!(
            serde_json::from_value::<Organization>(future)
                .unwrap()
                .default_repository_permission,
            Some(BasePermission::Unknown)
        );
        for (name, seats, filled, expected) in [
            ("free", Some(0), Some(0), None),
            ("FREE", Some(10), Some(10), None),
            ("enterprise", None, Some(10), None),
            ("enterprise", Some(10), None, None),
            ("enterprise", Some(0), Some(0), None),
            ("enterprise", Some(10), Some(11), Some((10, 11))),
        ] {
            assert_eq!(
                OrganizationPlan {
                    name: name.into(),
                    seats,
                    filled_seats: filled
                }
                .paid_seats(),
                expected
            );
        }
    }
    #[test]
    fn ldap_mapping_is_positive_only_and_diagnostics_do_not_keep_raw_dn() {
        for (dn, expected) in [
            (serde_json::Value::Null, false),
            (json!(""), false),
            (json!("  "), false),
            (json!("LDAP_PRIVATE_MARKER"), true),
        ] {
            let team: Team =
                serde_json::from_value(json!({"id":5,"slug":"team","name":"Team","ldap_dn":dn}))
                    .unwrap();
            assert_eq!(team.externally_managed, expected);
            assert!(!format!("{team:?}").contains("LDAP_PRIVATE_MARKER"));
        }
        let team: Team =
            serde_json::from_value(json!({"id":5,"slug":"team","name":"Team"})).unwrap();
        assert!(!team.externally_managed);
    }
}
