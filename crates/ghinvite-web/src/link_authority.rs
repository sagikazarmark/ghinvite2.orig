//! The web's only path to the `InvitationLink` Restate authority.
//! Never reads SQL eligibility.
//!
//! Every call waits for the authority's answer, because that answer is the
//! decision itself. The authority's documented terminal statuses become
//! [`AuthorityError::Invalid`], [`AuthorityError::Missing`] and
//! [`AuthorityError::Conflict`]; every other failure leaves the command's
//! outcome unknown.
use crate::error::IngressFailure;
use crate::{RestateClient, WebError};
use ghinvite_core::InvitationLinkId;
use ghinvite_core::admission::{
    AdminLinkCommand, AdmissionOperationId, AdmissionReceipt, Admit, Attempt, AttemptQuery,
    RequesterPage, UpdateMetadata,
};
use ghinvite_core::delivery::RepositoryProgress;
use ghinvite_core::request_lifecycle::{DecideRequest, DecisionReceipt, RequestStatus};
use ghinvite_core::storage::projection::{CreateLink, LinkSnapshot};
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::sync::Arc;
use thiserror::Error;

const LINK: &str = "InvitationLink";

/// Why the authority did not answer with a result.
#[derive(Debug, Error)]
pub enum AuthorityError {
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
    /// No usable answer arrived, so whether the command took effect is
    /// unknown. Always an [`IngressFailure::OutcomeUnknown`].
    #[error(transparent)]
    Unknown(IngressFailure),
}

impl AuthorityError {
    fn from_ingress(failure: IngressFailure) -> Self {
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
            AuthorityError::Invalid => WebError::BadRequest("Invalid command.".into()),
            AuthorityError::Missing => WebError::NotFound,
            AuthorityError::Conflict => WebError::Conflict,
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

    pub async fn create(&self, command: CreateLink) -> Result<LinkSnapshot> {
        self.link(command.link_id, "create", &command).await
    }

    pub async fn link_status(&self, command: AdminLinkCommand) -> Result<LinkSnapshot> {
        self.link(command.link_id, "link_status", &command).await
    }

    pub async fn revoke(&self, command: AdminLinkCommand) -> Result<LinkSnapshot> {
        self.link(command.link_id, "revoke", &command).await
    }

    pub async fn update_metadata(&self, command: UpdateMetadata) -> Result<LinkSnapshot> {
        self.link(command.link_id, "update_metadata", &command)
            .await
    }

    /// What the requester may see of the link `code` names, including the
    /// attempt `operation_id` identifies when given.
    pub async fn requester_page(
        &self,
        code: &str,
        requester_id: u64,
        operation_id: Option<AdmissionOperationId>,
    ) -> Result<RequesterPage> {
        let link_id = code.parse().map_err(|_| AuthorityError::Missing)?;
        let query = AttemptQuery {
            link_id,
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
        self.link(command.link_id, "admit", &command).await
    }

    pub async fn decide(&self, command: DecideRequest) -> Result<DecisionReceipt> {
        self.link(command.link_id, "decide", &command).await
    }

    /// Read the retained receipt without applying an undecided command on GET.
    pub async fn decision_status(&self, command: DecideRequest) -> Result<Option<DecisionReceipt>> {
        self.link(command.link_id, "decision_status", &command)
            .await
    }

    pub async fn delivery_progress(&self, query: RequestStatus) -> Result<Vec<RepositoryProgress>> {
        self.link(query.link_id, "delivery_progress", &query).await
    }

    /// Call only after the enclosing route has authorized this request/scope.
    pub async fn delivery_snapshot(
        &self,
        request_id: ghinvite_core::RequestId,
        repo_id: u64,
    ) -> Result<Option<ghinvite_core::delivery::DeliverySnapshot>> {
        self.call(
            "RepositoryDelivery",
            &ghinvite_core::delivery::delivery_key(request_id, repo_id),
            "status",
            &(),
        )
        .await
    }

    async fn link<I: Serialize, O: DeserializeOwned>(
        &self,
        link_id: InvitationLinkId,
        method: &str,
        input: &I,
    ) -> Result<O> {
        self.call(LINK, &link_id.to_string(), method, input).await
    }

    async fn call<I: Serialize, O: DeserializeOwned>(
        &self,
        service: &str,
        key: &str,
        method: &str,
        input: &I,
    ) -> Result<O> {
        self.client
            .invoke(service, key, method, input)
            .await
            .map_err(AuthorityError::from_ingress)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const CODE_VALUE: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAV";

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
                .requester_page(CODE_VALUE, 2, None)
                .await
                .unwrap_err();
            let actual = match error {
                AuthorityError::Invalid => "Invalid",
                AuthorityError::Missing => "Missing",
                AuthorityError::Conflict => "Conflict",
                AuthorityError::Unknown(_) => "Unknown",
            };
            assert_eq!(actual, expected, "HTTP {status}");
        }
    }

    #[tokio::test]
    async fn any_other_status_leaves_the_outcome_unknown() {
        for status in [401, 403, 429, 500, 502, 503] {
            let (_ingress, authority) = authority_answering(
                ResponseTemplate::new(status).set_body_string("upstream diagnostics"),
            )
            .await;
            let error = authority
                .requester_page(CODE_VALUE, 2, None)
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
            .requester_page(CODE_VALUE, 2, None)
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
            .requester_page(CODE_VALUE, 2, None)
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

    #[tokio::test]
    async fn a_malformed_code_is_missing_without_asking_the_authority() {
        let (ingress, authority) = authority_answering(ResponseTemplate::new(200)).await;
        for code in ["not a code", "81ARZ3NDEKTSV4RRFFQ69G5FAV"] {
            assert!(matches!(
                authority.requester_page(code, 2, None).await,
                Err(AuthorityError::Missing)
            ));
        }
        assert!(ingress.received_requests().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn public_lookup_routes_textual_variants_directly_to_the_canonical_link() {
        let ingress = MockServer::start().await;
        let link: InvitationLinkId = CODE_VALUE.parse().unwrap();
        Mock::given(path(format!("/{LINK}/{link}/requester_page")))
            .respond_with(ResponseTemplate::new(404))
            .expect(1)
            .mount(&ingress)
            .await;
        Mock::given(path(format!("/{LINK}/{link}/decision_status")))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&ingress)
            .await;
        let authority = LinkAuthority::new(Arc::new(RestateClient::new(ingress.uri()).unwrap()));
        assert!(matches!(
            authority
                .requester_page(&CODE_VALUE.to_lowercase(), 2, None)
                .await,
            Err(AuthorityError::Missing)
        ));
        let command: DecideRequest = serde_json::from_value(serde_json::json!({
            "link_id": link, "request_id": ghinvite_core::RequestId::new(),
            "operation_id": ghinvite_core::RequestId::new(),
            "admin": {"account_id": 1, "user_id": 2}, "action": {"kind": "approve"}}))
        .unwrap();
        // An empty body is the authority's "no retained receipt".
        assert!(authority.decision_status(command).await.unwrap().is_none());
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
        let failure = IngressFailure::unreachable("ingress unreachable");
        assert!(matches!(
            WebError::from(AuthorityError::Unknown(failure.clone())),
            WebError::Restate(f) if f == failure
        ));
    }
}
