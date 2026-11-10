//! Explicit partial-result metadata. No upstream header text is retained.
use super::Rest;
use crate::{error::PartialReason, ids::OrgId, transport::Response};

const MAX_ORG_IDS: usize = 100;

/// Why a successful walk omitted records. Organization IDs are unique, positive
/// numeric identities, capped at 100 across the whole walk.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct PartialInfo {
    /// Explicit reason why the listing cannot be treated as exhaustive.
    pub reason: PartialReason,
    /// Bounded, deduplicated numeric organizations affected by the provider's partial-result evidence.
    pub org_ids: Vec<OrgId>,
}
impl PartialInfo {
    pub(crate) fn from_response(response: &Response) -> Option<Self> {
        let header = response.headers.get("x-github-sso")?;
        if !header.to_ascii_lowercase().contains("partial-results") {
            return None;
        }
        let mut info = Self {
            reason: PartialReason::SsoRequired,
            org_ids: Vec::new(),
        };
        for ids in header.split(';').filter_map(|part| {
            let (key, value) = part.trim().split_once('=')?;
            key.trim()
                .eq_ignore_ascii_case("organizations")
                .then_some(value)
        }) {
            for id in ids
                .split(',')
                .filter_map(|id| id.trim().parse::<u64>().ok())
            {
                info.push(OrgId::new(id));
                if info.org_ids.len() == MAX_ORG_IDS {
                    return Some(info);
                }
            }
        }
        Some(info)
    }
    fn push(&mut self, id: OrgId) {
        if id.get() > 0 && self.org_ids.len() < MAX_ORG_IDS && !self.org_ids.contains(&id) {
            self.org_ids.push(id);
        }
    }
    pub(crate) fn merge(&mut self, other: Self) {
        for id in other.org_ids {
            self.push(id);
        }
    }
}

/// Collected items from a successful walk. Inspect `partial` or `complete()`
/// before using absence as evidence. Transport/pagination failures return `Err`,
/// even when partial SSO results were explicitly enabled.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
#[must_use]
pub struct Listing<T> {
    /// Items actually observed before the explicitly represented partial result.
    pub items: Vec<T>,
    /// None only for a complete walk; some requires the caller to handle missing visibility.
    pub partial: Option<PartialInfo>,
}
impl<T> Listing<T> {
    /// Whether the walk completed without upstream-reported omissions.
    pub fn complete(&self) -> bool {
        self.partial.is_none()
    }
}

/// Final metadata for a successful streaming walk. Earlier callbacks alone
/// cannot prove completion: the walk can still return a later `Err`.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
#[must_use]
pub struct PageCompletion {
    /// Optional incomplete-result evidence accompanying the page completion decision.
    pub partial: Option<PartialInfo>,
}
impl PageCompletion {
    /// Whether the whole walk completed without upstream-reported omissions.
    pub fn complete(&self) -> bool {
        self.partial.is_none()
    }
}

/// An explicit opt-in view for SSO-partial list reads. It cannot be passed to
/// exhaustive typed endpoints as a `Rest` or silently discard completeness.
#[must_use]
pub struct PartialRest<'r, 'a> {
    pub(crate) client: &'r Rest<'a>,
}
impl<'a> Rest<'a> {
    /// Permit SSO-partial list pages, returning explicit completeness metadata.
    /// Every linked page is still fetched; a later transport, shape, callback or
    /// pagination failure remains an error. This does not affect the original
    /// client or change exhaustive typed endpoint behavior.
    pub fn allow_partial(&self) -> PartialRest<'_, 'a> {
        PartialRest { client: self }
    }
}
