//! List the accounts a GitHub App is installed on, and the repositories
//! each installation can reach.
//!
//! The two steps use the two credentials every GitHub App has:
//!
//! 1. **As the App** (its credential, through
//!    [`octoauth::App::with_credential`]): `GET /app/installations`. Each
//!    installation names its account, an organization or a user. A 401 that
//!    a skewed host clock explains gets one retry with a corrected
//!    credential.
//! 2. **As each installation** (an installation token from an
//!    [`AppTokenProvider`], through [`with_token`]):
//!    `GET /installation/repositories`. Only an installation token can list
//!    these. A 401 (e.g. a revoked token) gets one replacement token and one
//!    retry.
//!
//! The GitHub client is octocrab, set up by [`octoauth_octocrab`]: one base
//! client without auth for the App's API, a client derived per credential,
//! errors reduced to fixed codes before they are printed.
//!
//! Both lists are paged by page number rather than through `Link` headers:
//! octocrab's `all_pages` follows `next` links without checking that they
//! stay on GitHub's host, and `GET /installation/repositories` is not an
//! octocrab `Page` at all.
#![cfg(not(target_arch = "wasm32"))]

use octoauth_octocrab::{authenticated, is_unauthorized, unauthorized};
use octocrab::Octocrab;
use octotoken::{
    Access, AppTokenProvider, InstallationId, Permissions, RepositoryPermission, TokenError,
    TokenScope, octoauth, with_token,
};

/// GitHub's maximum page size for both lists.
const MAX_PER_PAGE: u8 = 100;

/// An installation of the App, as `GET /app/installations` reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Installation {
    /// The installation's ID.
    pub id: InstallationId,
    /// The account's login.
    pub account: String,
    /// `Organization` or `User`, as GitHub names it.
    pub kind: String,
    /// `all` or `selected`, when GitHub reports it.
    pub selection: Option<String>,
}

/// A repository an installation can reach.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Repository {
    /// `owner/name`.
    pub full_name: String,
    /// Whether the repository is private.
    pub private: bool,
}

/// Why a listing failed. Every variant prints as a fixed code: none carries
/// upstream text, a token or a credential.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The App could not sign its credential (a bad key, a broken clock).
    #[error("{}", .0.code())]
    Credential(#[from] octoauth::Error),

    /// The installation token could not be created (suspended, uninstalled,
    /// permissions not granted, rate limited, …).
    #[error("{}", .0.code())]
    Token(#[from] TokenError),

    /// GitHub answered with an error, or did not answer.
    #[error(transparent)]
    GitHub(#[from] octoauth_octocrab::Error),
}

impl From<octocrab::Error> for Error {
    fn from(error: octocrab::Error) -> Self {
        Self::GitHub(error.into())
    }
}

/// The scope of the tokens [`Lister::repositories`] uses: `metadata: read`,
/// the least a token can carry and enough to list repositories.
pub fn repository_scope() -> TokenScope {
    Permissions::new([(RepositoryPermission::Metadata, Access::Read)])
        .expect("a valid permission set")
        .into()
}

/// Lists installations and their repositories for one App.
pub struct Lister {
    tokens: AppTokenProvider,
    github: Octocrab,
    per_page: u8,
}

impl Lister {
    /// `tokens` holds the App. The octocrab client is built for the App's
    /// API by [`octoauth_octocrab::builder`].
    pub fn new(tokens: AppTokenProvider) -> Result<Self, Error> {
        let github = octoauth_octocrab::builder(tokens.app())?.build()?;
        Ok(Self {
            tokens,
            github,
            per_page: MAX_PER_PAGE,
        })
    }

    /// Page size, 1 to 100 (default 100). Smaller pages only make sense in
    /// tests.
    pub fn per_page(mut self, per_page: u8) -> Self {
        self.per_page = per_page.clamp(1, MAX_PER_PAGE);
        self
    }

    /// Every installation of the App, in GitHub's order, as the App.
    pub async fn installations(&self) -> Result<Vec<Installation>, Error> {
        let app = self.tokens.app();
        let mut installations = Vec::new();
        for page in 1u32.. {
            let items = app
                .with_credential(unauthorized, |credential| async move {
                    let github = authenticated(&self.github, credential.expose_secret())?;
                    github
                        .apps()
                        .installations()
                        .per_page(self.per_page)
                        .page(page)
                        .send()
                        .await
                })
                .await
                .map_err(|error| error.map(Error::from).into_error())?
                .take_items();
            let last = items.len() < usize::from(self.per_page);
            installations.extend(items.into_iter().map(|installation| Installation {
                id: InstallationId::new(installation.id.0),
                account: installation.account.login,
                kind: installation.account.r#type,
                selection: installation.repository_selection,
            }));
            if last {
                break;
            }
        }
        Ok(installations)
    }

    /// The repositories `installation` can reach, as the installation.
    pub async fn repositories(
        &self,
        installation: InstallationId,
    ) -> Result<Vec<Repository>, Error> {
        let scope = repository_scope();
        let mut repositories = Vec::new();
        for page in 1u32.. {
            // One token per request: a cache hit after the first.
            let listing = with_token(
                &self.tokens,
                installation,
                &scope,
                is_unauthorized,
                |token| async move {
                    let github = authenticated(&self.github, token.expose_secret())?;
                    github
                        .apps()
                        .installation_repositories()
                        .per_page(self.per_page)
                        .page(page)
                        .send()
                        .await
                },
            )
            .await
            .map_err(|error| error.map(Error::from).into_error())?;
            let empty = listing.repositories.is_empty();
            repositories.extend(
                listing
                    .repositories
                    .into_iter()
                    .map(|repository| Repository {
                        full_name: repository.full_name.unwrap_or(repository.name),
                        private: repository.private.unwrap_or(false),
                    }),
            );
            if empty || repositories.len() >= listing.total_count {
                break;
            }
        }
        Ok(repositories)
    }
}
