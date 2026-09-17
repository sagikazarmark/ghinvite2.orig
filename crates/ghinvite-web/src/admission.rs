//! Explicitly gated browser-to-authority facade. Never reads SQL eligibility.
use crate::{RestateClient, Result, WebError};
use ghinvite_core::InvitationLinkId;
use ghinvite_core::admission::*;
use std::sync::Arc;

pub struct RestateAdmission {
    client: Arc<RestateClient>,
}
impl RestateAdmission {
    pub async fn create(
        &self,
        command: ghinvite_core::storage::projection::CreateLink,
    ) -> Result<ghinvite_core::storage::projection::LinkSnapshot> {
        self.client
            .authoritative_call(
                "InvitationLinkV1",
                &command.link_id.to_string(),
                "create",
                &command,
            )
            .await
    }
    pub async fn link_status(
        &self,
        command: AdminLinkCommand,
    ) -> Result<ghinvite_core::storage::projection::LinkSnapshot> {
        self.client
            .authoritative_call(
                "InvitationLinkV1",
                &command.link_id.to_string(),
                "link_status",
                &command,
            )
            .await
    }
    pub async fn revoke(
        &self,
        command: AdminLinkCommand,
    ) -> Result<ghinvite_core::storage::projection::LinkSnapshot> {
        self.client
            .authoritative_call(
                "InvitationLinkV1",
                &command.link_id.to_string(),
                "revoke",
                &command,
            )
            .await
    }
    pub async fn update_metadata(
        &self,
        command: UpdateMetadata,
    ) -> Result<ghinvite_core::storage::projection::LinkSnapshot> {
        self.client
            .authoritative_call(
                "InvitationLinkV1",
                &command.link_id.to_string(),
                "update_metadata",
                &command,
            )
            .await
    }
    pub fn new(client: Arc<RestateClient>) -> Self {
        Self { client }
    }
    pub fn command(
        &self,
        link_id: InvitationLinkId,
        operation_id: &str,
        requester_id: u64,
        justification: Option<String>,
    ) -> Result<Admit> {
        let operation_id = AdmissionOperationId::try_from(operation_id.to_owned())
            .map_err(|_| WebError::BadRequest("Missing or invalid operation ID. Return to the invitation link to start a fresh attempt.".into()))?;
        let mut command = Admit {
            version: 1,
            link_id,
            operation_id,
            requester_id,
            justification,
        };
        command.normalize();
        if command
            .justification
            .as_ref()
            .is_some_and(|s| s.len() > MAX_JUSTIFICATION_BYTES)
        {
            return Err(WebError::BadRequest("Justification is too long.".into()));
        }
        Ok(command)
    }
    pub async fn resolve(&self, code: &str) -> Result<InvitationLinkId> {
        ghinvite_core::Slug::from_string(code.to_owned()).map_err(|_| WebError::NotFound)?;
        self.client
            .authoritative_call("InvitationCodeV1", code, "resolve", &())
            .await
    }
    pub async fn lookup(
        &self,
        code: &str,
        requester_id: u64,
        operation: Option<&str>,
    ) -> Result<RequesterPage> {
        let operation_id = operation
            .map(|id| {
                AdmissionOperationId::try_from(id.to_owned())
                    .map_err(|_| WebError::BadRequest("Invalid operation ID.".into()))
            })
            .transpose()?;
        let link_id = self.resolve(code).await?;
        self.client
            .authoritative_call(
                "InvitationLinkV1",
                &link_id.to_string(),
                "requester_page",
                &AttemptQuery {
                    link_id,
                    requester_id,
                    operation_id,
                },
            )
            .await
    }
    pub async fn prepare(&self, command: Admit) -> Result<Attempt> {
        self.client
            .authoritative_call(
                "InvitationLinkV1",
                &command.link_id.to_string(),
                "prepare_attempt",
                &command,
            )
            .await
    }
    pub async fn admit(&self, command: Admit) -> Result<AdmissionReceipt> {
        self.client
            .authoritative_call(
                "InvitationLinkV1",
                &command.link_id.to_string(),
                "admit",
                &command,
            )
            .await
    }
}
