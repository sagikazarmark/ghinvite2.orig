//! Browser admission commands. Identities are trusted caller assertions.
use crate::{InvitationLinkId, RequestId, RequestState, SecretHash, targets::Targets};
use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub const MAX_JUSTIFICATION_CHARS: usize = 500;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(try_from = "String", into = "String")]
pub struct AdmissionOperationId(RequestId);
impl TryFrom<String> for AdmissionOperationId {
    type Error = &'static str;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        crate::request_lifecycle::parse_operation_id(&value).map(Self)
    }
}
impl From<AdmissionOperationId> for String {
    fn from(id: AdmissionOperationId) -> Self {
        id.0.to_string()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Admit {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub domain_evidence: Option<crate::email_domain::DomainEvidence>,
    pub scope_evidence: Vec<ScopeEvidence>,
    pub secret_sha256: SecretHash,
    pub secret_generation: u32,
    pub link_id: InvitationLinkId,
    pub operation_id: AdmissionOperationId,
    pub requester_id: u64,
    pub justification: Option<String>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("justification is too long")]
pub struct JustificationTooLong;

impl Admit {
    /// Trim the justification (blank means none) and bound it to
    /// [`MAX_JUSTIFICATION_CHARS`].
    pub fn normalize(&mut self) -> Result<(), JustificationTooLong> {
        self.justification = self
            .justification
            .take()
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty());
        if self
            .justification
            .as_ref()
            .is_some_and(|s| s.chars().count() > MAX_JUSTIFICATION_CHARS)
        {
            return Err(JustificationTooLong);
        }
        Ok(())
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AdmissionReceipt {
    pub decided_at: DateTime<Utc>,
    pub result: AdmissionResult,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AdmissionResult {
    Accepted {
        request_id: RequestId,
        state: RequestState,
        decision_deadline: Option<DateTime<Utc>>,
    },
    Rejected {
        reason: Rejection,
    },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Rejection {
    Paused,
    NotAvailable,
    ExistingRequest,
    AlreadyPending,
    AlreadyApproved,
    RerequestLimited {
        retry_after: DateTime<Utc>,
    },
    InstallationUnavailable,
    RepositoryUnavailable,
    ScopeUnavailable,
    DomainEvidenceMissing,
    DomainEvidenceStale,
    DomainMismatch {
        required: Vec<crate::email_domain::DomainRule>,
    },
}
impl From<crate::Inactive> for Rejection {
    fn from(inactive: crate::Inactive) -> Self {
        match inactive {
            crate::Inactive::Paused => Self::Paused,
            crate::Inactive::Revoked => Self::NotAvailable,
            crate::Inactive::Expired => Self::NotAvailable,
            crate::Inactive::Exhausted => Self::NotAvailable,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AdminLinkCommand {
    pub link_id: InvitationLinkId,
    pub admin: crate::invitation_link::AccountAdmin,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AttemptQuery {
    pub secret_sha256: SecretHash,
    pub secret_generation: u32,
    pub link_id: InvitationLinkId,
    pub requester_id: u64,
    pub operation_id: Option<AdmissionOperationId>,
}

/// Anonymous offer reads possess the link capability, but assert no user identity.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OfferQuery {
    pub link_id: InvitationLinkId,
    pub secret_sha256: SecretHash,
    pub secret_generation: u32,
}

/// Public offer only: no admin metadata, counters, attempts or requester records.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LinkOffer {
    pub link_id: InvitationLinkId,
    pub account_id: u64,
    pub targets: Targets,
    pub approval_required: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub email_domains: Vec<crate::email_domain::DomainRule>,
    pub paused: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Attempt {
    pub input: Admit,
    pub receipt: Option<AdmissionReceipt>,
}
/// Deliberately excludes all admin metadata and other requesters' facts.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct RequesterPage {
    /// Advisory organization requirement only, never the requester's 2FA status.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub two_factor_required: Option<bool>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub email_domains: Vec<crate::email_domain::DomainRule>,
    /// Current valid capability only; no admin reason or abuse metadata.
    #[serde(default)]
    pub paused: bool,
    pub link_id: InvitationLinkId,
    pub targets: Targets,
    pub approval_required: bool,
    /// Advisory only: current link guardrails and requester suppression permit
    /// a fresh attempt.
    pub can_start_fresh: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rerequest_after: Option<DateTime<Utc>>,
    pub attempt: Option<Attempt>,
    pub request: Option<crate::request_lifecycle::RequestSnapshot>,
}

/// Mutable admin metadata deliberately excludes creation guardrails.
///
/// ```compile_fail
/// fn change_max_uses(mut command: ghinvite_core::admission::UpdateMetadata) {
///     command.max_uses = Some(1);
/// }
/// ```
/// ```compile_fail
/// fn change_targets(mut command: ghinvite_core::admission::UpdateMetadata) {
///     command.targets = ghinvite_core::targets::Targets::default();
/// }
/// ```
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UpdateMetadata {
    pub link_id: InvitationLinkId,
    pub admin: crate::invitation_link::AccountAdmin,
    pub description: String,
    pub internal_note: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ScopeEvidence {
    pub app: crate::account::App,
    pub observed_at: DateTime<Utc>,
    pub available: bool,
    pub unavailable_target_ids: Vec<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn metadata_wire_rejects_every_guardrail_field() {
        let input = serde_json::json!({"link_id":"01ARZ3NDEKTSV4RRFFQ69G5FAV", "admin":{"account_id":1,"user_id":2}, "description":"Updated purpose", "internal_note":null});
        assert!(serde_json::from_value::<UpdateMetadata>(input.clone()).is_ok());
        for field in [
            "max_uses",
            "expires_at",
            "approval_required",
            "targets",
            "email_domains",
            "invitation_lifetime",
            "budget_share_pct",
            "abuse_threshold_per_hour",
            "secret_sha256",
            "secret_generation",
        ] {
            let mut changed = input.clone();
            changed[field] = serde_json::Value::Null;
            assert!(
                serde_json::from_value::<UpdateMetadata>(changed).is_err(),
                "{field}"
            );
        }
    }

    #[test]
    fn justification_is_bounded_by_characters_and_capability_is_required() {
        let input = serde_json::json!({"link_id":"01ARZ3NDEKTSV4RRFFQ69G5FAV","operation_id":"01ARZ3NDEKTSV4RRFFQ69G5FAW","requester_id":1,"scope_evidence":[],"secret_sha256":"01".repeat(32),"secret_generation":1,"justification":"界".repeat(500)});
        let mut admit: Admit = serde_json::from_value(input.clone()).unwrap();
        assert!(admit.normalize().is_ok());
        admit.justification = Some("界".repeat(501));
        assert!(admit.normalize().is_err());
        for field in ["secret_sha256", "secret_generation"] {
            let mut missing = input.clone();
            missing.as_object_mut().unwrap().remove(field);
            assert!(serde_json::from_value::<Admit>(missing).is_err());
        }
    }
}
