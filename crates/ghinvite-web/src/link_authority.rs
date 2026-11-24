//! The web's only path to the `InvitationLink` Restate authority.
//! Never reads SQL eligibility.
//!
//! Every call waits for the authority's answer, because that answer is the
//! decision itself. The authority's documented terminal statuses become
//! [`AuthorityError::Invalid`], [`AuthorityError::Missing`] and
//! [`AuthorityError::Conflict`], and for the admin commands a revoked link
//! refuses, [`AuthorityError::Revoked`]; every other failure leaves the
//! command's outcome unknown.
use crate::error::IngressFailure;
use crate::{RestateClient, WebError};
use ghinvite_core::InvitationLinkId;
use ghinvite_core::admission::{
    AdminLinkCommand, AdmissionOperationId, AdmissionReceipt, Admit, Attempt, AttemptQuery,
    RequesterPage, UpdateMetadata,
};
use ghinvite_core::invitation_link::{
    CreateLink, CreateLinkOutcome, LINK_REVOKED_STATUS, LinkSnapshot, RotateSecret, SecretRotated,
};
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::sync::Arc;
use thiserror::Error;

const LINK: &str = "InvitationLink";

/// Why the authority did not answer with a result.
#[derive(Debug, Error)]
pub enum AuthorityError {
    #[error("Console access could not be verified")]
    AuthorizationUnavailable,
    /// The authority refused the command's input (400); nothing was applied.
    #[error("the authority rejected the command's input")]
    Invalid,
    /// No such link or code for this caller (404), including links of another
    /// account.
    #[error("not found")]
    Missing,
    /// The command's identity is bound to different input (409).
    #[error("operation conflict")]
    Conflict,
    /// The link is revoked, so the command changed nothing and bound no
    /// identity ([`LINK_REVOKED_STATUS`]). It has its own status so that a
    /// revoked link is never mistaken for a conflicting operation identity.
    #[error("link revoked")]
    Revoked,
    /// No usable answer arrived, so whether the command took effect is
    /// unknown. Always an [`IngressFailure::OutcomeUnknown`].
    #[error(transparent)]
    Unknown(IngressFailure),
}

impl AuthorityError {
    pub(crate) fn from_ingress(failure: IngressFailure) -> Self {
        match failure {
            IngressFailure::Rejected { status: 400 } => Self::Invalid,
            IngressFailure::Rejected { status: 404 } => Self::Missing,
            IngressFailure::Rejected { status: 409 } => Self::Conflict,
            IngressFailure::Rejected { status } => Self::Unknown(IngressFailure::OutcomeUnknown {
                detail: "ingress returned an unhandled status",
                status: Some(status),
            }),
            unknown => Self::Unknown(unknown),
        }
    }
}

impl From<AuthorityError> for WebError {
    fn from(error: AuthorityError) -> Self {
        match error {
            AuthorityError::AuthorizationUnavailable => WebError::ConsoleAuthority,
            AuthorityError::Invalid => WebError::BadRequest("Invalid command.".into()),
            AuthorityError::Missing => WebError::NotFound,
            AuthorityError::Conflict => WebError::Conflict,
            AuthorityError::Revoked => {
                WebError::BadRequest("This invitation link is revoked. Nothing was changed.".into())
            }
            AuthorityError::Unknown(failure) => WebError::Restate(failure),
        }
    }
}

pub type Result<T> = std::result::Result<T, AuthorityError>;

#[derive(Clone, Debug)]
pub struct LinkAuthority {
    client: Arc<RestateClient>,
}

impl LinkAuthority {
    pub fn new(client: Arc<RestateClient>) -> Self {
        Self { client }
    }

    pub async fn create(&self, command: CreateLink) -> Result<CreateLinkOutcome> {
        self.link(command.link_id, "create", &command).await
    }

    pub async fn link_status(&self, command: AdminLinkCommand) -> Result<LinkSnapshot> {
        self.link(command.link_id, "link_status", &command).await
    }

    pub async fn prepare_access_removal(
        &self,
        command: ghinvite_core::revocation::PrepareAccessRemoval,
    ) -> Result<ghinvite_core::revocation::AccessRemovalPreview> {
        self.unless_revoked(command.link_id, "prepare_access_removal", &command)
            .await
    }

    pub async fn revoke(
        &self,
        command: ghinvite_core::revocation::RevokeCommand,
    ) -> Result<ghinvite_core::revocation::RevokeOutcome> {
        self.link(command.link_id, "revoke", &command).await
    }
    pub async fn revoke_status(
        &self,
        command: ghinvite_core::revocation::RevokeCommand,
    ) -> Result<Option<ghinvite_core::revocation::RevokeOutcome>> {
        self.link(command.link_id, "revoke_status", &command).await
    }
    pub async fn cascade_progress(
        &self,
        command: AdminLinkCommand,
    ) -> Result<Option<ghinvite_core::revocation::CascadeProgress>> {
        self.link(command.link_id, "cascade_progress", &command)
            .await
    }

    pub async fn pause(
        &self,
        command: ghinvite_core::link_lifecycle::LifecycleCommand,
    ) -> Result<ghinvite_core::link_lifecycle::LifecycleOutcome> {
        self.link(command.link_id, "pause", &command).await
    }

    pub async fn resume(
        &self,
        command: ghinvite_core::link_lifecycle::LifecycleCommand,
    ) -> Result<ghinvite_core::link_lifecycle::LifecycleOutcome> {
        self.link(command.link_id, "resume", &command).await
    }

    /// A retained rotation replays its receipt even after revocation; a fresh
    /// one on a revoked link is [`AuthorityError::Revoked`].
    pub async fn rotate_secret(&self, command: RotateSecret) -> Result<SecretRotated> {
        self.unless_revoked(command.link_id, "rotate_secret", &command)
            .await
    }

    pub async fn update_metadata(&self, command: UpdateMetadata) -> Result<LinkSnapshot> {
        self.link(command.link_id, "update_metadata", &command)
            .await
    }

    /// What the capability authorizes the requester to see, including the
    /// attempt `operation_id` identifies when given.
    pub async fn offer(
        &self,
        capability: &crate::session::PendingLink,
    ) -> Result<ghinvite_core::admission::LinkOffer> {
        self.link(
            capability.link_id,
            "offer",
            &ghinvite_core::admission::OfferQuery {
                link_id: capability.link_id,
                secret_sha256: capability.secret_sha256,
                secret_generation: capability.secret_generation,
            },
        )
        .await
    }

    pub async fn requester_page(
        &self,
        capability: &crate::session::PendingLink,
        requester_id: u64,
        operation_id: Option<AdmissionOperationId>,
    ) -> Result<RequesterPage> {
        let link_id = capability.link_id;
        let query = AttemptQuery {
            link_id,
            secret_sha256: capability.secret_sha256,
            secret_generation: capability.secret_generation,
            requester_id,
            operation_id,
        };
        self.link(link_id, "requester_page", &query).await
    }

    /// Retain the attempt's input before admitting it, so a lost response
    /// stays recoverable.
    pub async fn prepare(&self, command: Admit) -> Result<Attempt> {
        self.link(command.link_id, "prepare_attempt", &command)
            .await
    }

    pub async fn admit(&self, command: Admit) -> Result<AdmissionReceipt> {
        let observation = crate::admission_telemetry::Observation::start();
        let result = self.link(command.link_id, "admit", &command).await;
        observation.complete(&result);
        result
    }

    async fn link<I: Serialize, O: DeserializeOwned>(
        &self,
        link_id: InvitationLinkId,
        method: &str,
        input: &I,
    ) -> Result<O> {
        self.client
            .invoke(LINK, &link_id.to_string(), method, input)
            .await
            .map_err(AuthorityError::from_ingress)
    }

    /// [`LinkAuthority::link`] for an admin command the link refuses once it
    /// is revoked. Only these commands read [`LINK_REVOKED_STATUS`] as
    /// [`AuthorityError::Revoked`]; a requester's view never learns of it.
    async fn unless_revoked<I: Serialize, O: DeserializeOwned>(
        &self,
        link_id: InvitationLinkId,
        method: &str,
        input: &I,
    ) -> Result<O> {
        self.client
            .invoke(LINK, &link_id.to_string(), method, input)
            .await
            .map_err(|failure| match failure {
                IngressFailure::Rejected {
                    status: LINK_REVOKED_STATUS,
                } => AuthorityError::Revoked,
                failure => AuthorityError::from_ingress(failure),
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const LINK_ID: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAV";

    fn capability() -> crate::session::PendingLink {
        crate::session::PendingLink {
            link_id: LINK_ID.parse().unwrap(),
            secret_sha256: [42; 32].into(),
            secret_generation: 7,
        }
    }

    async fn authority_answering(response: ResponseTemplate) -> (MockServer, LinkAuthority) {
        let ingress = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(response)
            .mount(&ingress)
            .await;
        let client = RestateClient::new(ingress.uri()).unwrap();
        (ingress, LinkAuthority::new(Arc::new(client)))
    }

    #[tokio::test]
    async fn documented_terminal_statuses_are_definitive() {
        for (status, expected) in [(400, "Invalid"), (404, "Missing"), (409, "Conflict")] {
            let (_ingress, authority) = authority_answering(
                ResponseTemplate::new(status).set_body_string("upstream diagnostics"),
            )
            .await;
            let error = authority
                .requester_page(&capability(), 2, None)
                .await
                .unwrap_err();
            assert_eq!(name(&error), expected, "HTTP {status}");
            let error = authority.rotate_secret(rotation()).await.unwrap_err();
            assert_eq!(name(&error), expected, "HTTP {status}");
        }
    }

    fn name(error: &AuthorityError) -> &'static str {
        match error {
            AuthorityError::AuthorizationUnavailable => "AuthorizationUnavailable",
            AuthorityError::Invalid => "Invalid",
            AuthorityError::Missing => "Missing",
            AuthorityError::Conflict => "Conflict",
            AuthorityError::Revoked => "Revoked",
            AuthorityError::Unknown(_) => "Unknown",
        }
    }

    fn rotation() -> RotateSecret {
        RotateSecret {
            link_id: LINK_ID.parse().unwrap(),
            operation_id: ghinvite_core::RequestId::new()
                .to_string()
                .try_into()
                .unwrap(),
            admin: ghinvite_core::invitation_link::AccountAdmin {
                account_id: 1,
                user_id: 2,
            },
            new_sha256: [42; 32].into(),
        }
    }

    #[tokio::test]
    async fn only_the_commands_a_revoked_link_refuses_read_it_as_revoked() {
        let (_ingress, authority) =
            authority_answering(ResponseTemplate::new(LINK_REVOKED_STATUS)).await;
        let error = authority.rotate_secret(rotation()).await.unwrap_err();
        assert_eq!(name(&error), "Revoked");
        let error = authority
            .prepare_access_removal(ghinvite_core::revocation::PrepareAccessRemoval {
                link_id: LINK_ID.parse().unwrap(),
                admin: rotation().admin,
                operation_id: ghinvite_core::RequestId::new()
                    .to_string()
                    .try_into()
                    .unwrap(),
            })
            .await
            .unwrap_err();
        assert_eq!(name(&error), "Revoked");
        // No requester handler answers it, so it stays an unknown outcome there.
        let error = authority
            .requester_page(&capability(), 2, None)
            .await
            .unwrap_err();
        assert_eq!(name(&error), "Unknown");
    }

    #[tokio::test]
    async fn any_other_status_leaves_the_outcome_unknown() {
        for status in [401, 403, 410, 429, 500, 502, 503] {
            let (_ingress, authority) = authority_answering(
                ResponseTemplate::new(status).set_body_string("upstream diagnostics"),
            )
            .await;
            let error = authority
                .requester_page(&capability(), 2, None)
                .await
                .unwrap_err();
            let AuthorityError::Unknown(failure) = &error else {
                panic!("HTTP {status}: expected an unknown outcome, got {error:?}")
            };
            assert!(
                matches!(failure, IngressFailure::OutcomeUnknown { .. }),
                "HTTP {status}: {failure:?}"
            );
            assert_eq!(failure.upstream_status(), Some(status));
            assert!(!format!("{error} {error:?}").contains("upstream diagnostics"));
        }
    }

    #[tokio::test]
    async fn an_undecodable_answer_leaves_the_outcome_unknown() {
        let (_ingress, authority) =
            authority_answering(ResponseTemplate::new(200).set_body_string("not json")).await;
        let error = authority
            .requester_page(&capability(), 2, None)
            .await
            .unwrap_err();
        assert!(
            matches!(
                error,
                AuthorityError::Unknown(IngressFailure::OutcomeUnknown {
                    status: Some(200),
                    ..
                })
            ),
            "{error:?}"
        );
    }

    #[tokio::test]
    async fn an_unreachable_authority_leaves_the_outcome_unknown() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        drop(listener);
        let authority = LinkAuthority::new(Arc::new(RestateClient::new(url).unwrap()));
        let error = authority
            .requester_page(&capability(), 2, None)
            .await
            .unwrap_err();
        assert!(
            matches!(
                error,
                AuthorityError::Unknown(IngressFailure::OutcomeUnknown { status: None, .. })
            ),
            "{error:?}"
        );
    }

    #[test]
    fn pending_capability_requires_hash_and_generation_in_serialized_input() {
        let full = serde_json::to_value(capability()).unwrap();
        for field in ["secret_sha256", "secret_generation"] {
            let mut missing = full.clone();
            missing.as_object_mut().unwrap().remove(field);
            assert!(serde_json::from_value::<crate::session::PendingLink>(missing).is_err());
        }
    }

    #[tokio::test]
    async fn capability_lookup_uses_canonical_internal_id_and_hash_body() {
        let ingress = MockServer::start().await;
        let link: InvitationLinkId = LINK_ID.parse().unwrap();
        Mock::given(path(format!("/{LINK}/{link}/requester_page")))
            .respond_with(ResponseTemplate::new(404))
            .expect(1)
            .mount(&ingress)
            .await;
        let request_id = ghinvite_core::RequestId::new();
        Mock::given(path(format!(
            "/InvitationRequest/{request_id}/decision_status"
        )))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&ingress)
        .await;
        let authority = LinkAuthority::new(Arc::new(RestateClient::new(ingress.uri()).unwrap()));
        assert!(matches!(
            authority.requester_page(&capability(), 2, None).await,
            Err(AuthorityError::Missing)
        ));
        let requests = ingress.received_requests().await.unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests[0].url.path(),
            format!("/{LINK}/{link}/requester_page")
        );
        assert!(requests[0].url.query().is_none());
        let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
        assert_eq!(body["link_id"], LINK_ID);
        assert_eq!(body["secret_sha256"], "2a".repeat(32));
        assert_eq!(body["secret_generation"], 7);
        assert_eq!(body["requester_id"], 2);
        assert!(body.get("secret").is_none());
        let command: ghinvite_core::request_lifecycle::DecideRequest =
            serde_json::from_value(serde_json::json!({
            "link_id": link, "request_id": request_id,
            "operation_id": ghinvite_core::RequestId::new(),
            "admin": {"account_id": 1, "user_id": 2}, "action": {"kind": "approve"}}))
            .unwrap();
        // An empty body is the authority's "no retained receipt".
        assert!(
            crate::request_authority::RequestAuthority::new(authority.client)
                .decision_status(command)
                .await
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn routes_that_propagate_keep_the_existing_web_errors() {
        assert!(matches!(
            WebError::from(AuthorityError::Invalid),
            WebError::BadRequest(message) if message == "Invalid command."
        ));
        assert!(matches!(
            WebError::from(AuthorityError::Missing),
            WebError::NotFound
        ));
        assert!(matches!(
            WebError::from(AuthorityError::Conflict),
            WebError::Conflict
        ));
        assert!(matches!(
            WebError::from(AuthorityError::Revoked),
            WebError::BadRequest(message) if message.contains("revoked")
        ));
        let failure = IngressFailure::unreachable("ingress unreachable");
        assert!(matches!(
            WebError::from(AuthorityError::Unknown(failure.clone())),
            WebError::Restate(f) if f == failure
        ));
    }
}
