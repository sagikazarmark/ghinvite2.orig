//! Metered team selection reads. A cached cursor cannot select another endpoint.
use super::*;
use crate::error::PartialReason;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
/// Opaque continuation for a validated organization-team page walk.
/// Serializes as the bare relative path (persisted in ghinvite's authority
/// cache; the form must not change) and is validated again before use.
pub struct TeamCursor(String);
#[derive(Clone, Debug, Serialize, Deserialize)]
/// Team selection facts used to bind a numeric team to an organization.
pub struct TeamSelection {
    /// GitHub's numeric team identifier; slugs and names are not stable identity.
    pub id: u64,
    /// Current path slug used in subsequent team API calls.
    pub slug: String,
    /// Current display name; not an authorization identifier.
    pub name: String,
    #[serde(default)]
    /// Positive LDAP synchronization evidence when supplied; absence alone does not prove a team is writable.
    pub ldap_dn: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
/// One bounded page of organization teams, retaining explicit continuation.
pub struct TeamPage {
    /// Team facts on this page; this vector alone is not a complete organization inventory.
    pub teams: Vec<TeamSelection>,
    /// Validated continuation cursor; none means this page supplied no next relation.
    pub next: Option<TeamCursor>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
/// A team and its owning organization's current identities from the team endpoint.
pub struct TeamIdentity {
    #[serde(flatten)]
    /// Current team identity and optional synchronization evidence.
    pub team: TeamSelection,
    /// Owning organization's numeric identity; callers must compare it with the intended organization.
    pub organization: GhUser,
}

/// Read a numeric team's current identity and organization binding.
pub async fn team_identity(
    rest: &Rest<'_>,
    org: &str,
    slug: &str,
) -> Result<TeamIdentity, ApiError> {
    Ok(call::<(), _>(rest, Endpoint::GetTeam, &[org, slug], None)
        .await?
        .body)
}
/// Read one organization-team page without hiding pagination or partial results.
pub async fn team_page(
    rest: &Rest<'_>,
    org: &str,
    cursor: Option<&TeamCursor>,
) -> Result<TeamPage, ApiError> {
    Endpoint::ListOrgTeams.authorize(rest)?;
    let expected = Endpoint::ListOrgTeams.meta().path.seg(org).build()?;
    let first = format!("{expected}?per_page=100");
    let path = cursor.map(|c| c.0.as_str()).unwrap_or(&first);
    let base = rest.env.api_base.as_str().trim_end_matches('/');
    validate(base, path, &expected)?;
    let response = rest
        .env
        .send(rest.request(Method::Get, path, Endpoint::ListOrgTeams.meta().api_version))
        .await?;
    if response.status != 200 {
        return Err(ApiError::from_response(&response));
    }
    if response
        .headers
        .get("x-github-sso")
        .is_some_and(|value| value.to_ascii_lowercase().contains("partial-results"))
    {
        return Err(ApiError::Partial {
            reason: PartialReason::SsoRequired,
            org_ids: vec![],
        });
    }
    let teams: Vec<TeamSelection> = serde_json::from_slice(&response.body)
        .map_err(|_| ApiError::Decode { what: "team_page" })?;
    if teams.len() > 100 {
        return Err(ApiError::Decode { what: "team_page" });
    }
    let next = crate::pagination::next_page_path(&response, base)
        .map_err(|_| ApiError::Decode { what: "pagination" })?;
    if let Some(next) = &next {
        validate(base, next, &expected)?;
    }
    Ok(TeamPage {
        teams,
        next: next.map(TeamCursor),
    })
}
fn validate(base: &str, path: &str, endpoint: &str) -> Result<(), ApiError> {
    let bad = || ApiError::Decode { what: "pagination" };
    let url = url::Url::parse(&format!("{base}{path}")).map_err(|_| bad())?;
    let expected = url::Url::parse(&format!("{base}{endpoint}")).map_err(|_| bad())?;
    if url.origin() != expected.origin()
        || url.path() != expected.path()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(bad());
    }
    let mut keys = std::collections::BTreeSet::new();
    for (key, value) in url.query_pairs() {
        if !keys.insert(key.to_string())
            || !matches!(key.as_ref(), "page" | "per_page")
            || value
                .parse::<u32>()
                .ok()
                .filter(|value| *value > 0)
                .is_none()
        {
            return Err(bad());
        }
    }
    Ok(())
}
