use crate::error::WebError;
use axum::response::Redirect;
use chrono::{DateTime, Utc};
use domain::{InvitationRequest, RequestState, ShareLink, ShareLinkId, Slug};
use storage::Storage;

pub(crate) enum PendingRequestPolicy {
    Ignore,
    RedirectForRequester(u64),
}

pub(crate) struct PublicShareLinkResolution {
    pub(crate) slug: Slug,
    pub(crate) link: ShareLink,
    pending_request: Option<InvitationRequest>,
}

impl PublicShareLinkResolution {
    pub(crate) fn pending_redirect(&self) -> Option<Redirect> {
        self.pending_request.as_ref().map(|request| {
            Redirect::to(&format!("/i/{}/pending/{}", self.slug.as_str(), request.id))
        })
    }
}

pub(crate) async fn resolve_public_share_link(
    storage: &dyn Storage,
    raw_slug: &str,
    now: DateTime<Utc>,
    pending_policy: PendingRequestPolicy,
) -> Result<PublicShareLinkResolution, WebError> {
    let slug = Slug::from_string(raw_slug.to_string()).map_err(|_| WebError::NotFound)?;
    let link = storage
        .get_share_link_by_slug(slug.as_str())
        .await
        .map_err(|_| WebError::NotFound)?
        .ok_or(WebError::NotFound)?;

    if !link.slug.ct_eq(&slug) || !link.is_active(now) {
        return Err(WebError::NotFound);
    }

    let pending_request = match pending_policy {
        PendingRequestPolicy::Ignore => None,
        PendingRequestPolicy::RedirectForRequester(requester_id) => {
            pending_request_for(storage, link.id, requester_id).await
        }
    };

    Ok(PublicShareLinkResolution {
        slug,
        link,
        pending_request,
    })
}

async fn pending_request_for(
    storage: &dyn Storage,
    link_id: ShareLinkId,
    requester_id: u64,
) -> Option<InvitationRequest> {
    let requests = storage.list_requests_for_link(link_id).await.ok()?;
    requests.into_iter().find(|request| {
        request.requester_id == requester_id && request.state == RequestState::Pending
    })
}
