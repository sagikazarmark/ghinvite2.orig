//! GitHub repository, organization, installation and user endpoint contracts.
//!
//! Each function takes a credential-scoped [`Rest`] and performs one HTTP
//! attempt per page; complete listings follow bounded pagination. Write
//! fencing, retries and role precedence are the caller's policy.
pub mod authority;
pub mod authority_teams;
mod invitation_alias;
pub mod organization;

use crate::{
    error::{ApiError, ForbiddenReason},
    payloads::{GhAppInstallation, GhRepo, GhUser},
    rest::{
        ApiResponse, CredentialKind, Listing, PageCompletion, PartialInfo, PartialRest,
        PathTemplate, Rest,
    },
    roles::RepoRole,
    transport::{Method, Response},
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

/// An installation permission an endpoint requires.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
#[non_exhaustive]
pub enum Permission {
    /// Repository administration (collaborators and invitations).
    Administration,
    /// Organization members, invitations and teams.
    Members,
    /// Repository metadata.
    Metadata,
}

impl Permission {
    /// GitHub's JSON key.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Administration => "administration",
            Self::Members => "members",
            Self::Metadata => "metadata",
        }
    }
}

impl From<Permission> for octoauth::Permission {
    fn from(permission: Permission) -> Self {
        match permission {
            Permission::Administration => octoauth::RepositoryPermission::Administration.into(),
            Permission::Members => octoauth::OrganizationPermission::Members.into(),
            Permission::Metadata => octoauth::RepositoryPermission::Metadata.into(),
        }
    }
}

/// Level of access to a permission. `Write` includes `Read`.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum Access {
    /// Read access.
    Read,
    /// Write access.
    Write,
    /// Administrative access.
    Admin,
}

impl Access {
    /// GitHub's JSON value.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Write => "write",
            Self::Admin => "admin",
        }
    }
}

impl From<Access> for octoauth::Access {
    fn from(access: Access) -> Self {
        match access {
            Access::Read => octoauth::Access::Read,
            Access::Write => octoauth::Access::Write,
            Access::Admin => octoauth::Access::Admin,
        }
    }
}

macro_rules! endpoint_set {
    ($( $variant:ident => $description:literal),+ $(,)?) => {
        /// The endpoints this crate calls.
        #[derive(Clone, Copy, Debug, Eq, PartialEq)]
        #[non_exhaustive]
        pub enum Endpoint { $( #[doc = $description] $variant),+ }
        impl Endpoint {
            /// Every endpoint, from the same declaration as the enum.
            pub const fn all() -> &'static [Self] { &[$( Self::$variant),+] }
        }
    };
}
endpoint_set! {
    GetOrgMembership => "Read one user's organization role and active or pending membership state.",
    GetOrg => "Read organization identity and optional plan/security facts visible to the credential.",
    ListOrgInvitations => "List pending organization invitations without inferring acceptance from absence.",
    ListOrgInvitationTeams => "List the exact teams attached to one organization invitation.",
    ListFailedOrgInvitations => "List organization invitations that the provider reports failed.",
    CreateOrgInvitation => "Create an organization invitation for an explicit recipient, role and team set.",
    CancelOrgInvitation => "Cancel one organization invitation by its numeric upstream identity.",
    RemoveOrgMember => "Remove a user's organization membership; application policy authorizes this destructive effect.",
    ListOrgTeams => "List organization teams with stable numeric identities.",
    GetTeam => "Read a team using its current organization and slug.",
    GetTeamMembership => "Read the role and state of one user's team membership.",
    PutTeamMembership => "Set an explicit team role for one user without implicit role-precedence policy.",
    RemoveTeamMembership => "Remove one user's team membership.",
    GetRepository => "Read repository metadata and numeric owner identity.",
    CollaboratorPermission => "Read a user's effective repository role.",
    ListCollaborators => "List repository collaborators with their effective roles.",
    PutCollaborator => "Set an explicit collaborator role, distinguishing a new invitation from immediate no-content success.",
    ListRepoInvitations => "List pending repository invitations and their provider timing evidence.",
    UpdateRepoInvitation => "Change the role offered by one existing repository invitation.",
    DeleteRepoInvitation => "Delete one pending repository invitation.",
    RemoveCollaborator => "Remove one collaborator's repository access.",
    GetUserByLogin => "Resolve a mutable login to current numeric user identity.",
    GetUserById => "Read a user by stable numeric identity rather than a mutable login.",
    GetInstallation => "Read installation facts under app-JWT authority.",
    ListInstallationRepos => "List repositories reachable by the presented installation token.",
    ListInstallationRequests => "List pending app installation requests using app-JWT authority.",
}

/// Security and dispatch properties of one endpoint.
#[derive(Clone, Debug)]
pub struct EndpointMeta {
    /// Exact HTTP method; never derive retry safety from it.
    pub method: Method,
    /// Route template; segments are encoded separately.
    pub path: PathTemplate,
    /// Credential classes accepted; a mismatch fails before any request.
    pub credential: &'static [CredentialKind],
    /// Required installation permission, or none for an explicit exemption.
    pub permission: Option<(Permission, Access)>,
    /// Whether the call can spend GitHub's content-creation budget.
    pub content_creating: bool,
    /// Whether the response supports a complete page walk.
    pub paginated: bool,
    /// Whether repeating the operation is safe after an ambiguous failure.
    pub retry_safe: bool,
    /// Endpoint-specific API version; none means the environment's default.
    pub api_version: Option<&'static str>,
}

impl Endpoint {
    /// Alias of [`Endpoint::GetOrgMembership`].
    #[allow(non_upper_case_globals)]
    pub const GetOrganizationMembership: Self = Self::GetOrgMembership;

    /// The endpoint's metadata, used by typed dispatch.
    pub const fn meta(self) -> EndpointMeta {
        use Access::{Read, Write};
        use CredentialKind::{AppJwt, Installation};
        use Method::{Delete, Get, Patch, Post, Put};
        use Permission::{Administration, Members, Metadata};
        let (method, path, credential, permission, paginated): (_, _, &'static [_], _, _) =
            match self {
                Self::GetOrgMembership => (
                    Get,
                    "/orgs/{}/memberships/{}",
                    &[Installation],
                    Some((Members, Read)),
                    false,
                ),
                Self::GetOrg => (
                    Get,
                    "/orgs/{}",
                    &[Installation],
                    Some((Metadata, Read)),
                    false,
                ),
                Self::ListOrgInvitations => (
                    Get,
                    "/orgs/{}/invitations",
                    &[Installation],
                    Some((Members, Read)),
                    true,
                ),
                Self::ListOrgInvitationTeams => (
                    Get,
                    "/orgs/{}/invitations/{}/teams",
                    &[Installation],
                    Some((Members, Read)),
                    true,
                ),
                Self::ListFailedOrgInvitations => (
                    Get,
                    "/orgs/{}/failed_invitations",
                    &[Installation],
                    Some((Members, Read)),
                    true,
                ),
                Self::CreateOrgInvitation => (
                    Post,
                    "/orgs/{}/invitations",
                    &[Installation],
                    Some((Members, Write)),
                    false,
                ),
                Self::CancelOrgInvitation => (
                    Delete,
                    "/orgs/{}/invitations/{}",
                    &[Installation],
                    Some((Members, Write)),
                    false,
                ),
                Self::RemoveOrgMember => (
                    Delete,
                    "/orgs/{}/memberships/{}",
                    &[Installation],
                    Some((Members, Write)),
                    false,
                ),
                // UNVERIFIED: live org-app permission/list visibility (TR-13, SP-01).
                Self::ListOrgTeams => (
                    Get,
                    "/orgs/{}/teams",
                    &[Installation],
                    Some((Members, Read)),
                    true,
                ),
                Self::GetTeam => (
                    Get,
                    "/orgs/{}/teams/{}",
                    &[Installation],
                    Some((Members, Read)),
                    false,
                ),
                Self::GetTeamMembership => (
                    Get,
                    "/orgs/{}/teams/{}/memberships/{}",
                    &[Installation],
                    Some((Members, Read)),
                    false,
                ),
                Self::PutTeamMembership => (
                    Put,
                    "/orgs/{}/teams/{}/memberships/{}",
                    &[Installation],
                    Some((Members, Write)),
                    false,
                ),
                Self::RemoveTeamMembership => (
                    Delete,
                    "/orgs/{}/teams/{}/memberships/{}",
                    &[Installation],
                    Some((Members, Write)),
                    false,
                ),
                Self::GetRepository => (
                    Get,
                    "/repos/{}/{}",
                    &[Installation],
                    Some((Metadata, Read)),
                    false,
                ),
                Self::CollaboratorPermission => (
                    Get,
                    "/repos/{}/{}/collaborators/{}/permission",
                    &[Installation],
                    Some((Metadata, Read)),
                    false,
                ),
                // UNVERIFIED: direct affiliation with the repo app token (SP-07).
                Self::ListCollaborators => (
                    Get,
                    "/repos/{}/{}/collaborators",
                    &[Installation],
                    Some((Metadata, Read)),
                    true,
                ),
                Self::PutCollaborator => (
                    Put,
                    "/repos/{}/{}/collaborators/{}",
                    &[Installation],
                    Some((Administration, Write)),
                    false,
                ),
                Self::ListRepoInvitations => (
                    Get,
                    "/repos/{}/{}/invitations",
                    &[Installation],
                    Some((Administration, Read)),
                    true,
                ),
                Self::UpdateRepoInvitation => (
                    Patch,
                    "/repos/{}/{}/invitations/{}",
                    &[Installation],
                    Some((Administration, Write)),
                    false,
                ),
                Self::DeleteRepoInvitation => (
                    Delete,
                    "/repos/{}/{}/invitations/{}",
                    &[Installation],
                    Some((Administration, Write)),
                    false,
                ),
                Self::RemoveCollaborator => (
                    Delete,
                    "/repos/{}/{}/collaborators/{}",
                    &[Installation],
                    Some((Administration, Write)),
                    false,
                ),
                Self::GetUserByLogin => (Get, "/users/{}", &[Installation], None, false),
                // UNVERIFIED numeric-user lookup spelling in P1 §7.2; retained PoC path.
                Self::GetUserById => (Get, "/user/{}", &[Installation], None, false),
                Self::GetInstallation => (Get, "/app/installations/{}", &[AppJwt], None, false),
                Self::ListInstallationRepos => (
                    Get,
                    "/installation/repositories",
                    &[Installation],
                    Some((Metadata, Read)),
                    true,
                ),
                Self::ListInstallationRequests => {
                    (Get, "/app/installation-requests", &[AppJwt], None, true)
                }
            };
        EndpointMeta {
            method,
            path: PathTemplate::new(path),
            credential,
            permission,
            paginated,
            content_creating: matches!(
                self,
                Self::PutCollaborator
                    | Self::UpdateRepoInvitation
                    | Self::DeleteRepoInvitation
                    | Self::RemoveCollaborator
                    | Self::CreateOrgInvitation
                    | Self::CancelOrgInvitation
                    | Self::RemoveOrgMember
                    | Self::PutTeamMembership
                    | Self::RemoveTeamMembership
            ),
            retry_safe: !matches!(self, Self::UpdateRepoInvitation | Self::CreateOrgInvitation),
            api_version: None,
        }
    }

    /// Why an endpoint needs no installation permission beyond its credential.
    pub const fn permission_exemption(self) -> Option<&'static str> {
        match self {
            Self::GetUserByLogin | Self::GetUserById => {
                Some("public user identity; authenticated using any installation")
            }
            Self::GetInstallation | Self::ListInstallationRequests => {
                Some("app-JWT authority, not installation permissions")
            }
            _ => None,
        }
    }

    /// The basis for the endpoint's retry-safety classification.
    pub const fn retry_reason(self) -> &'static str {
        match self {
            Self::UpdateRepoInvitation => "PATCH is not assumed duplicate-effect safe",
            Self::CreateOrgInvitation => {
                "invitation POST must not repeat after an uncertain response"
            }
            Self::PutTeamMembership => {
                "explicit-role membership PUT repeats the same effect; role precedence remains caller policy"
            }
            Self::PutCollaborator => "repeated collaborator PUT does not create a second grant",
            Self::DeleteRepoInvitation
            | Self::RemoveCollaborator
            | Self::CancelOrgInvitation
            | Self::RemoveOrgMember
            | Self::RemoveTeamMembership => "delete a specific resource; repetition is absent/404",
            Self::GetOrgMembership
            | Self::GetRepository
            | Self::CollaboratorPermission
            | Self::ListCollaborators
            | Self::ListRepoInvitations
            | Self::GetUserByLogin
            | Self::GetUserById
            | Self::GetInstallation
            | Self::ListInstallationRepos
            | Self::ListInstallationRequests
            | Self::GetOrg
            | Self::ListOrgInvitations
            | Self::ListOrgInvitationTeams
            | Self::ListFailedOrgInvitations
            | Self::ListOrgTeams
            | Self::GetTeam
            | Self::GetTeamMembership => "GET observes without creating an effect",
        }
    }

    pub(crate) fn authorize(self, rest: &Rest<'_>) -> Result<(), ApiError> {
        authorize_meta(&self.meta(), rest)
    }
}

fn authorize_meta(meta: &EndpointMeta, rest: &Rest<'_>) -> Result<(), ApiError> {
    if meta.credential.contains(&rest.credential_kind()) {
        Ok(())
    } else {
        Err(ApiError::Forbidden {
            reason: ForbiddenReason::WrongCredential,
        })
    }
}

impl ApiError {
    /// Whether this failure may be retried given the endpoint's retry safety;
    /// an ambiguous unsafe write must not be replayed blindly.
    pub fn is_transient_for(&self, meta: &EndpointMeta) -> bool {
        self.is_transient_for_retry_safe(meta.retry_safe)
    }
}

async fn call<Req: Serialize + Sync, Resp: DeserializeOwned>(
    rest: &Rest<'_>,
    endpoint: Endpoint,
    segments: &[&str],
    body: Option<&Req>,
) -> Result<ApiResponse<Resp>, ApiError> {
    call_meta(rest, endpoint.meta(), segments, body).await
}

async fn call_meta<Req: Serialize + Sync, Resp: DeserializeOwned>(
    rest: &Rest<'_>,
    meta: EndpointMeta,
    segments: &[&str],
    body: Option<&Req>,
) -> Result<ApiResponse<Resp>, ApiError> {
    authorize_meta(&meta, rest)?;
    let path = segments
        .iter()
        .fold(meta.path, |path, segment| path.seg(segment));
    rest.call_with_version(meta.method, path, &[], body, meta.api_version)
        .await
}

/// Name-addressed repository; `PathTemplate` encodes the segments.
#[derive(Clone, Copy, Debug)]
pub struct Repository<'a> {
    /// Current owner; numeric identity belongs in the caller's binding.
    pub owner: &'a str,
    /// Current repository name.
    pub name: &'a str,
}

impl Repository<'_> {
    fn path(&self, endpoint: Endpoint) -> PathTemplate {
        endpoint.meta().path.seg(self.owner).seg(self.name)
    }
}

/// Read the repository's numeric identity and canonical name. The caller binds
/// `id` to its retained target before accepting an availability observation.
pub async fn get_repository(rest: &Rest<'_>, repo: Repository<'_>) -> Result<GhRepo, ApiError> {
    Ok(call::<(), _>(
        rest,
        Endpoint::GetRepository,
        &[repo.owner, repo.name],
        None,
    )
    .await?
    .body)
}

/// A user's current repository permission, decoded into the role model.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CollaboratorPermission {
    /// Account whose numeric identity must match the requested collaborator.
    pub user: GhUser,
    /// Effective role, including custom roles without privilege inference.
    pub role: RepoRole,
}

#[derive(Deserialize)]
struct PermissionWire {
    user: GhUser,
    role_name: String,
}

/// Read `role_name` exclusively; missing modern role data fails closed.
pub async fn collaborator_permission(
    rest: &Rest<'_>,
    repo: Repository<'_>,
    login: &str,
) -> Result<CollaboratorPermission, ApiError> {
    let response: ApiResponse<PermissionWire> = call::<(), _>(
        rest,
        Endpoint::CollaboratorPermission,
        &[repo.owner, repo.name, login],
        None,
    )
    .await?;
    if response.body.role_name.is_empty() {
        return Err(ApiError::Decode {
            what: "collaborator_role",
        });
    }
    Ok(CollaboratorPermission {
        user: response.body.user,
        role: RepoRole::parse(&response.body.role_name),
    })
}

/// One collaborator and its effective role.
#[derive(Clone, Debug, Deserialize)]
pub struct Collaborator {
    /// Collaborator account.
    #[serde(flatten)]
    pub user: GhUser,
    /// Effective role; callers decide role precedence.
    #[serde(deserialize_with = "deserialize_role")]
    pub role_name: RepoRole,
}

fn deserialize_role<'de, D: serde::Deserializer<'de>>(d: D) -> Result<RepoRole, D::Error> {
    String::deserialize(d).map(|s| RepoRole::parse(&s))
}

/// A pending repository invitation as reported by GitHub.
#[derive(Clone, Debug, Deserialize)]
pub struct RepoInvitation {
    /// Numeric upstream invitation identifier.
    pub id: u64,
    /// Recipient account when disclosed.
    pub invitee: Option<GhUser>,
    /// Role offered by this invitation.
    #[serde(deserialize_with = "deserialize_role")]
    pub permissions: RepoRole,
    /// Provider invitation creation timestamp.
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// Whether GitHub reports the invitation expired.
    pub expired: bool,
}

/// The successful outcomes of a collaborator PUT.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum CollaboratorWrite {
    /// 201: GitHub created an invitation rather than granting access.
    Created {
        /// The new invitation's ID.
        invitation_id: u64,
        /// GitHub's creation time when present; never a local substitute.
        created_at: Option<chrono::DateTime<chrono::Utc>>,
    },
    /// 204: no new invitation; no identifier is inferred.
    NoContent,
}

fn grant(role: &RepoRole, patch: bool) -> Result<&'static str, ApiError> {
    Ok(match role {
        RepoRole::Read => {
            if patch {
                "read"
            } else {
                "pull"
            }
        }
        RepoRole::Write => {
            if patch {
                "write"
            } else {
                "push"
            }
        }
        RepoRole::Triage => "triage",
        RepoRole::Maintain => "maintain",
        RepoRole::Admin => "admin",
        RepoRole::None | RepoRole::Custom(_) => {
            return Err(ApiError::Decode {
                what: "built_in_role",
            });
        }
    })
}

/// Set a collaborator's built-in role, keeping invitation-created (201)
/// distinct from no-content (204).
pub async fn put_collaborator(
    rest: &Rest<'_>,
    repo: Repository<'_>,
    login: &str,
    role: &RepoRole,
) -> Result<CollaboratorWrite, ApiError> {
    Endpoint::PutCollaborator.authorize(rest)?;
    let response: ApiResponse<Option<crate::payloads::GhCollaboratorInvite>> = call(
        rest,
        Endpoint::PutCollaborator,
        &[repo.owner, repo.name, login],
        Some(&serde_json::json!({"permission":grant(role,false)?})),
    )
    .await?;
    match (response.status, response.body) {
        (201, Some(invitation)) => Ok(CollaboratorWrite::Created {
            invitation_id: invitation.id,
            created_at: invitation.created_at,
        }),
        (204, None) => Ok(CollaboratorWrite::NoContent),
        _ => Err(ApiError::Decode {
            what: "collaborator_status",
        }),
    }
}

/// PATCH a pending invitation with a built-in role. The caller must compare
/// the prior role and ensure this raises access; this never chooses policy.
pub async fn update_repo_invitation(
    rest: &Rest<'_>,
    repo: Repository<'_>,
    id: u64,
    role: &RepoRole,
) -> Result<RepoInvitation, ApiError> {
    Endpoint::UpdateRepoInvitation.authorize(rest)?;
    let response = call(
        rest,
        Endpoint::UpdateRepoInvitation,
        &[repo.owner, repo.name, &id.to_string()],
        Some(&serde_json::json!({"permissions":grant(role,true)?})),
    )
    .await?;
    if response.status != 200 {
        return Err(ApiError::Decode {
            what: "invitation_status",
        });
    }
    Ok(response.body)
}

/// Delete one repository invitation; anything but 204 is not a deletion.
pub async fn delete_repo_invitation(
    rest: &Rest<'_>,
    repo: Repository<'_>,
    id: u64,
) -> Result<(), ApiError> {
    let response: ApiResponse<()> = call::<(), _>(
        rest,
        Endpoint::DeleteRepoInvitation,
        &[repo.owner, repo.name, &id.to_string()],
        None,
    )
    .await?;
    if response.status != 204 {
        return Err(ApiError::Decode {
            what: "delete_status",
        });
    }
    Ok(())
}

/// Remove one collaborator's access; anything but 204 is not a removal.
pub async fn remove_collaborator(
    rest: &Rest<'_>,
    repo: Repository<'_>,
    login: &str,
) -> Result<(), ApiError> {
    let response: ApiResponse<()> = call::<(), _>(
        rest,
        Endpoint::RemoveCollaborator,
        &[repo.owner, repo.name, login],
        None,
    )
    .await?;
    if response.status != 204 {
        return Err(ApiError::Decode {
            what: "delete_status",
        });
    }
    Ok(())
}

/// Resolve a current login to numeric user facts; a login is not identity.
pub async fn get_user_by_login(rest: &Rest<'_>, login: &str) -> Result<GhUser, ApiError> {
    Ok(
        call::<(), _>(rest, Endpoint::GetUserByLogin, &[login], None)
            .await?
            .body,
    )
}

/// UNVERIFIED numeric-user lookup path in the P1 endpoint contract.
pub async fn get_user_by_id(rest: &Rest<'_>, id: u64) -> Result<GhUser, ApiError> {
    Ok(
        call::<(), _>(rest, Endpoint::GetUserById, &[&id.to_string()], None)
            .await?
            .body,
    )
}

/// Read installation facts under an [`AppCredential`](crate::AppCredential)
/// for exact app/account/suspension validation.
pub async fn get_installation(rest: &Rest<'_>, id: u64) -> Result<GhAppInstallation, ApiError> {
    Ok(
        call::<(), _>(rest, Endpoint::GetInstallation, &[&id.to_string()], None)
            .await?
            .body,
    )
}

/// A pending request to install an app.
#[derive(Clone, Debug, Deserialize)]
pub struct InstallationRequest {
    /// Numeric installation-request identifier.
    pub id: u64,
    /// Account on which installation was requested.
    pub account: GhUser,
    /// Numeric requester identity only.
    pub requester: InstallationRequester,
    /// GitHub's creation time.
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// The requester of an installation request; GitHub sends no login.
#[derive(Clone, Debug, Deserialize)]
pub struct InstallationRequester {
    /// Numeric user identifier of the requester.
    pub id: u64,
}

impl Rest<'_> {
    /// Collect every page or fail. No prefix escapes when a later page fails,
    /// is malformed, cycles, or reports SSO partial results.
    pub async fn list_all<T: DeserializeOwned>(
        &self,
        endpoint: Endpoint,
        path: PathTemplate,
        query: &[(&str, &str)],
    ) -> Result<Vec<T>, ApiError> {
        let mut all = Vec::new();
        self.for_each_page(endpoint, path, query, |page| {
            all.extend(page);
            Ok(())
        })
        .await?;
        Ok(all)
    }

    /// Stream pages. Earlier callbacks can run before a later failure, so
    /// only a final `Ok(())` makes the observed items a complete listing.
    pub async fn for_each_page<T: DeserializeOwned>(
        &self,
        endpoint: Endpoint,
        path: PathTemplate,
        query: &[(&str, &str)],
        mut absorb: impl FnMut(Vec<T>) -> Result<(), ApiError>,
    ) -> Result<(), ApiError> {
        self.walk_pages(endpoint, path, query, |response| {
            let page = serde_json::from_slice(&response.body)
                .map_err(|_| ApiError::Decode { what: "shape" })?;
            absorb(page)
        })
        .await
    }

    async fn walk_pages(
        &self,
        endpoint: Endpoint,
        path: PathTemplate,
        query: &[(&str, &str)],
        absorb: impl FnMut(Response) -> Result<(), ApiError>,
    ) -> Result<(), ApiError> {
        self.walk_pages_policy(
            endpoint.meta(),
            path,
            query,
            false,
            endpoint == Endpoint::ListRepoInvitations,
            absorb,
        )
        .await
        .map(|_| ())
    }

    #[cfg(test)]
    async fn walk_pages_meta(
        &self,
        meta: EndpointMeta,
        path: PathTemplate,
        query: &[(&str, &str)],
        absorb: impl FnMut(Response) -> Result<(), ApiError>,
    ) -> Result<(), ApiError> {
        self.walk_pages_policy(meta, path, query, false, false, absorb)
            .await
            .map(|_| ())
    }

    async fn walk_pages_policy(
        &self,
        meta: EndpointMeta,
        path: PathTemplate,
        query: &[(&str, &str)],
        allow_partial: bool,
        invitation_alias: bool,
        mut absorb: impl FnMut(Response) -> Result<(), ApiError>,
    ) -> Result<PageCompletion, ApiError> {
        authorize_meta(&meta, self)?;
        if !meta.paginated || meta.method != Method::Get {
            return Err(ApiError::Decode {
                what: "pagination_endpoint",
            });
        }
        let built = path.build()?;
        let mut first = self.env.api_base.clone();
        let base = self.env.api_base.as_str().trim_end_matches('/');
        first.set_path(&format!(
            "{}{}",
            self.env.api_base.path().trim_end_matches('/'),
            built
        ));
        first.query_pairs_mut().extend_pairs(query.iter().copied());
        if !query.iter().any(|(key, _)| *key == "per_page") {
            first.query_pairs_mut().append_pair("per_page", "100");
        }
        let first_path = first
            .as_str()
            .strip_prefix(base)
            .ok_or(ApiError::Decode { what: "pagination" })?
            .to_owned();
        let expected_path = first.path().to_owned();
        let alias =
            invitation_alias::Identity::new(invitation_alias, &built, self.env.api_base.path());
        let mut partial: Option<PartialInfo> = None;
        crate::pagination::walk_link_pages_with(
            first_path,
            base,
            crate::pagination::MAX_PAGES,
            |path| {
                let expected_path = &expected_path;
                let alias = &alias;
                async move {
                    // Never send a Link-selected credential to another endpoint,
                    // origin, URL userinfo or fragment, even on a trusted API base.
                    let url = url::Url::parse(&format!("{base}{path}"))
                        .map_err(|_| ApiError::Decode { what: "pagination" })?;
                    if url.origin() != self.env.api_base.origin()
                        || (url.path() != expected_path && !alias.matches(url.path()))
                        || !url.username().is_empty()
                        || url.password().is_some()
                        || url.fragment().is_some()
                    {
                        return Err(ApiError::Decode { what: "pagination" });
                    }
                    let response = self
                        .env
                        .send(self.request(Method::Get, &path, meta.api_version))
                        .await?;
                    if response.status != 200 {
                        return Err(ApiError::from_response(&response));
                    }
                    alias.observe_first_page(&response);
                    if !allow_partial && let Some(info) = PartialInfo::from_response(&response) {
                        return Err(ApiError::Partial {
                            reason: info.reason,
                            org_ids: info.org_ids,
                        });
                    }
                    Ok(response)
                }
            },
            |response| {
                if let Some(info) = PartialInfo::from_response(&response) {
                    match &mut partial {
                        Some(existing) => existing.merge(info),
                        None => partial = Some(info),
                    }
                }
                absorb(response)
            },
            || ApiError::Decode { what: "pagination" },
        )
        .await?;
        Ok(PageCompletion { partial })
    }
}

impl PartialRest<'_, '_> {
    /// Collect all available pages with explicit SSO omission metadata. Later
    /// transport, decode or pagination failures are still errors.
    pub async fn list_all<T: DeserializeOwned>(
        &self,
        endpoint: Endpoint,
        path: PathTemplate,
        query: &[(&str, &str)],
    ) -> Result<Listing<T>, ApiError> {
        let mut items = Vec::new();
        let completion = self
            .for_each_page(endpoint, path, query, |page| {
                items.extend(page);
                Ok(())
            })
            .await?;
        Ok(Listing {
            items,
            partial: completion.partial,
        })
    }

    /// Stream available pages and return final completeness metadata.
    pub async fn for_each_page<T: DeserializeOwned>(
        &self,
        endpoint: Endpoint,
        path: PathTemplate,
        query: &[(&str, &str)],
        mut absorb: impl FnMut(Vec<T>) -> Result<(), ApiError>,
    ) -> Result<PageCompletion, ApiError> {
        self.client
            .walk_pages_policy(
                endpoint.meta(),
                path,
                query,
                true,
                endpoint == Endpoint::ListRepoInvitations,
                |response| {
                    let page = serde_json::from_slice(&response.body)
                        .map_err(|_| ApiError::Decode { what: "shape" })?;
                    absorb(page)
                },
            )
            .await
    }
}

/// Every pending repository invitation; partial SSO visibility or incomplete
/// pagination is an error.
pub async fn list_repo_invitations(
    rest: &Rest<'_>,
    repo: Repository<'_>,
) -> Result<Vec<RepoInvitation>, ApiError> {
    rest.list_all(
        Endpoint::ListRepoInvitations,
        repo.path(Endpoint::ListRepoInvitations),
        &[],
    )
    .await
}

/// UNVERIFIED direct-affiliation support with the repo installation permission.
pub async fn list_collaborators(
    rest: &Rest<'_>,
    repo: Repository<'_>,
) -> Result<Vec<Collaborator>, ApiError> {
    rest.list_all(
        Endpoint::ListCollaborators,
        repo.path(Endpoint::ListCollaborators),
        &[("affiliation", "direct")],
    )
    .await
}

/// Every pending app installation request, under an
/// [`AppCredential`](crate::AppCredential).
pub async fn list_installation_requests(
    rest: &Rest<'_>,
) -> Result<Vec<InstallationRequest>, ApiError> {
    rest.list_all(
        Endpoint::ListInstallationRequests,
        Endpoint::ListInstallationRequests.meta().path,
        &[],
    )
    .await
}

/// Every repository the presented installation can access. The reported
/// `total_count` must match and repository IDs must be unique.
pub async fn list_installation_repos(rest: &Rest<'_>) -> Result<Vec<GhRepo>, ApiError> {
    let mut all = Vec::new();
    let mut total = None;
    rest.walk_pages(
        Endpoint::ListInstallationRepos,
        Endpoint::ListInstallationRepos.meta().path,
        &[],
        |response| {
            let page: crate::payloads::GhInstallationRepos = serde_json::from_slice(&response.body)
                .map_err(|_| ApiError::Decode { what: "shape" })?;
            if total.is_some_and(|expected| expected != page.total_count) {
                return Err(ApiError::Decode {
                    what: "installation_repository_count",
                });
            }
            total = Some(page.total_count);
            all.extend(page.repositories);
            Ok(())
        },
    )
    .await?;
    if total != Some(all.len() as u64) {
        return Err(ApiError::Decode {
            what: "incomplete_installation_repositories",
        });
    }
    if all
        .iter()
        .map(|repo| repo.id)
        .collect::<std::collections::BTreeSet<_>>()
        .len()
        != all.len()
    {
        return Err(ApiError::Decode {
            what: "duplicate_installation_repository",
        });
    }
    Ok(all)
}

#[cfg(test)]
mod tests;
