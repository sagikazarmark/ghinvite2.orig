//! Invitation request reads, shared by SQLite and D1. Rows decode as
//! [`crate::InvitationRequest`].

pub const GET: &str = "SELECT id, invitation_link_id, batch_id, batch_row, requester_id, justification, state, decided_by, decided_at, decline_reason, requester_message, cancellation_reason, domain_evidence_json, created_at, decision_deadline FROM invitation_requests WHERE id = ?1";

#[derive(serde::Deserialize)]
pub struct Row {
    #[serde(flatten)]
    pub request: crate::InvitationRequest,
    pub domain_evidence_json: Option<String>,
    pub batch_id: Option<crate::bulk::BulkBatchId>,
    pub batch_row: Option<u16>,
}
impl Row {
    pub fn into_request(mut self) -> super::Result<crate::InvitationRequest> {
        self.request.batch = match (self.batch_id, self.batch_row) {
            (None, None) => None,
            (Some(batch_id), Some(row)) => Some(crate::bulk::BatchRef { batch_id, row }),
            _ => return Err(super::Error::Corrupt("invalid request source".into())),
        };
        crate::bulk::validate_origin(self.request.invitation_link_id, self.request.batch)
            .map_err(|_| super::Error::Corrupt("invalid request source".into()))?;
        self.request.domain_evidence = decode_evidence(
            self.domain_evidence_json.as_deref(),
            self.request.invitation_link_id,
            self.request.created_at,
        )?;
        validate_requester_message(&self.request)?;
        Ok(self.request)
    }
}

pub fn validate_evidence(
    evidence: &crate::email_domain::DomainEvidence,
    link: impl Into<Option<crate::InvitationLinkId>>,
    admitted_at: chrono::DateTime<chrono::Utc>,
) -> super::Result<()> {
    if Some(evidence.link_id) != link.into()
        || evidence.checked_at > admitted_at
        || admitted_at - evidence.checked_at > chrono::Duration::minutes(10)
        || evidence.matched_domains.is_empty()
        || evidence
            .matched_domains
            .iter()
            .any(crate::email_domain::AsciiDomain::is_noreply)
    {
        return Err(super::Error::Corrupt(
            "invalid retained domain evidence".into(),
        ));
    }
    Ok(())
}

pub fn decode_evidence(
    input: Option<&str>,
    link: impl Into<Option<crate::InvitationLinkId>>,
    admitted_at: chrono::DateTime<chrono::Utc>,
) -> super::Result<Option<crate::email_domain::DomainEvidence>> {
    let link = link.into();
    input
        .map(|value| {
            let evidence = serde_json::from_str(value)
                .map_err(|_| super::Error::Corrupt("invalid retained domain evidence".into()))?;
            validate_evidence(&evidence, link, admitted_at)?;
            Ok(evidence)
        })
        .transpose()
}

/// Both direct and history reads reject malformed public-message projection rows.
pub fn validate_requester_message(request: &crate::InvitationRequest) -> super::Result<()> {
    if let Some(message) = &request.requester_message
        && (request.state != crate::RequestState::Declined
            || crate::request_lifecycle::normalize_requester_message(Some(message.clone()))
                .ok()
                .flatten()
                .as_ref()
                != Some(message))
    {
        return Err(super::Error::Corrupt("invalid requester message".into()));
    }
    Ok(())
}
