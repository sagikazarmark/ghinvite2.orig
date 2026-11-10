//! Installation-token authority reads. Cursors are opaque and remain bound to
//! the same endpoint, so callers can meter/cache each page independently.
use super::*;
use crate::error::PartialReason;

#[derive(Clone, Debug, Serialize, Deserialize)]
/// Repository identity returned by GitHub for binding later authorization decisions.
pub struct RepositoryIdentity {
    /// GitHub's numeric repository identifier, stable across repository renames.
    pub id: u64,
    /// Current owner/name path; numeric identity must still be checked by the caller.
    pub full_name: String,
    /// Current owning account, including its numeric GitHub identity.
    pub owner: GhUser,
}

/// Read a repository's current numeric identity and owner using the endpoint's required credential permissions.
pub async fn repository_identity(
    rest: &Rest<'_>,
    repo: Repository<'_>,
) -> Result<RepositoryIdentity, ApiError> {
    Ok(call::<(), _>(
        rest,
        Endpoint::GetRepository,
        &[repo.owner, repo.name],
        None,
    )
    .await?
    .body)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
/// Current organization membership facts; absence, pending membership and active membership remain distinct.
pub struct Membership {
    /// Provider membership role; unknown values must not be promoted to owner authority.
    pub role: String,
    /// Provider membership state; only explicitly active membership supports an active-membership claim.
    pub state: String,
    /// The member account whose numeric identity must match the requested actor.
    pub user: GhUser,
    /// The organization account whose numeric identity must match the requested organization.
    pub organization: GhUser,
}
/// Read organization membership for a username; callers bind returned numeric user and organization identities before granting authority.
pub async fn organization_membership(
    rest: &Rest<'_>,
    org: &str,
    login: &str,
) -> Result<Membership, ApiError> {
    Ok(call::<(), _>(
        rest,
        Endpoint::GetOrganizationMembership,
        &[org, login],
        None,
    )
    .await?
    .body)
}

/// Deserialized cache cursors are validated again before any request is sent.
/// Serializes as the bare relative path (persisted in ghinvite's authority
/// cache; the form must not change).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct RepositoryCursor(String);
#[derive(Clone, Debug, Serialize, Deserialize)]
/// One bounded page of an installation's selected repositories, with explicit continuation.
pub struct RepositoryPage {
    /// Provider-reported repository count; it does not make an unfinished page walk complete.
    pub total_count: u64,
    /// Repository identities present on this page.
    pub repositories: Vec<RepositoryIdentity>,
    /// Validated next-page cursor, or none only when this response has no next page.
    pub next: Option<RepositoryCursor>,
}
/// Read one selected-repository page; never treat a returned prefix as the installation's complete selection.
pub async fn installation_repository_page(
    rest: &Rest<'_>,
    cursor: Option<&RepositoryCursor>,
) -> Result<RepositoryPage, ApiError> {
    Endpoint::ListInstallationRepos.authorize(rest)?;
    let path = cursor
        .map(|c| c.0.as_str())
        .unwrap_or("/installation/repositories?per_page=100");
    let base = rest.env.api_base.as_str().trim_end_matches('/');
    validate_page(base, path)?;
    let response = rest
        .env
        .send(rest.request(
            Method::Get,
            path,
            Endpoint::ListInstallationRepos.meta().api_version,
        ))
        .await?;
    if response.status != 200 {
        return Err(ApiError::from_response(&response));
    }
    if response
        .headers
        .get("x-github-sso")
        .is_some_and(|v| v.to_ascii_lowercase().contains("partial-results"))
    {
        return Err(ApiError::Partial {
            reason: PartialReason::SsoRequired,
            org_ids: vec![],
        });
    }
    #[derive(Deserialize)]
    struct Wire {
        total_count: u64,
        repositories: Vec<RepositoryIdentity>,
    }
    let wire: Wire = serde_json::from_slice(&response.body).map_err(|_| ApiError::Decode {
        what: "repository_page",
    })?;
    let next = crate::pagination::next_page_path(&response, base)
        .map_err(|_| ApiError::Decode { what: "pagination" })?;
    if let Some(next) = &next {
        validate_page(base, next)?;
    }
    Ok(RepositoryPage {
        total_count: wire.total_count,
        repositories: wire.repositories,
        next: next.map(RepositoryCursor),
    })
}
fn validate_page(base: &str, path: &str) -> Result<(), ApiError> {
    let invalid = || ApiError::Decode { what: "pagination" };
    let url = url::Url::parse(&format!("{base}{path}")).map_err(|_| invalid())?;
    let expected =
        url::Url::parse(&format!("{base}/installation/repositories")).map_err(|_| invalid())?;
    if url.origin() != expected.origin()
        || url.path() != expected.path()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(invalid());
    }
    let mut names = std::collections::BTreeSet::new();
    for (key, value) in url.query_pairs() {
        if !names.insert(key.to_string())
            || !matches!(key.as_ref(), "page" | "per_page")
            || value.parse::<u32>().ok().filter(|v| *v > 0).is_none()
        {
            return Err(invalid());
        }
    }
    Ok(())
}
