//! Browser admission commands. Identities are trusted caller assertions.
//!
//! The pure admission rules live here too, so the link authority and the web
//! tests' fake authority apply the same ones.
use crate::invitation_link::LinkSnapshot;
use crate::request_lifecycle::{PENDING_LIFETIME, RequestSnapshot, TerminalDecision};
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

    /// Accept this attempt: consume one of `link`'s uses and create the
    /// admitted request, pending until its decision deadline when it needs
    /// approval and approved at once otherwise. Answers none, changing
    /// nothing, when the use count would overflow.
    pub fn accept(
        &self,
        link: &mut LinkSnapshot,
        request_id: RequestId,
        approval_required: bool,
        now: DateTime<Utc>,
    ) -> Option<RequestSnapshot> {
        link.uses = link.uses.checked_add(1)?;
        link.revision += 1;
        let state = if approval_required {
            RequestState::Pending
        } else {
            RequestState::Approved
        };
        Some(RequestSnapshot {
            domain_evidence: (!link.creation.email_domains.is_empty())
                .then(|| self.domain_evidence.clone())
                .flatten(),
            request_id,
            link_id: Some(link.link_id),
            batch: None,
            account_id: link.creation.account_id,
            requester_id: self.requester_id,
            justification: self.justification.clone(),
            state,
            admitted_at: now,
            decision_deadline: approval_required.then_some(now + PENDING_LIFETIME),
            revision: 1,
            decision: (!approval_required).then(|| TerminalDecision {
                decision_id: format!("request.approved/{request_id}"),
                decided_by: None,
                effective_at: now,
                evaluated_at: now,
                cancellation_reason: None,
                decline_reason: None,
                requester_message: None,
            }),
        })
    }

    /// A link with email-domain rules admits only on fresh evidence, for this
    /// link, of a verified address in one of them.
    pub fn domain_refusal(
        &self,
        rules: &[crate::email_domain::DomainRule],
        now: DateTime<Utc>,
    ) -> Option<Rejection> {
        use crate::email_domain::DomainEvidenceError;
        if rules.is_empty() {
            return None;
        }
        let Some(evidence) = &self.domain_evidence else {
            return Some(Rejection::DomainEvidenceMissing);
        };
        match evidence.check(rules, self.link_id, now) {
            Ok(()) => None,
            Err(DomainEvidenceError::Stale) => Some(Rejection::DomainEvidenceStale),
            Err(DomainEvidenceError::Mismatch) => Some(Rejection::DomainMismatch {
                required: rules.to_vec(),
            }),
        }
    }

    /// Caller-supplied observations are advisory: only a recent negative for an
    /// application required by these targets can refuse admission. Missing, stale,
    /// and future-dated observations cannot establish current unavailability.
    pub fn scope_unavailable(&self, targets: &Targets, now: DateTime<Utc>) -> bool {
        self.scope_evidence.iter().any(|e| {
            let required = match e.app {
                crate::account::App::Repo => !targets.repos.is_empty(),
                crate::account::App::Org => targets.organization.is_some(),
            };
            required
                && !e.available
                && e.observed_at <= now
                && now - e.observed_at <= chrono::Duration::minutes(5)
        })
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
impl Rejection {
    /// One pending or approved request per requester: the rejection that the
    /// requester's latest request, in `state`, gives a new attempt.
    pub fn open_request(state: RequestState) -> Option<Self> {
        match state {
            RequestState::Pending => Some(Self::AlreadyPending),
            RequestState::Approved => Some(Self::AlreadyApproved),
            _ => None,
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

impl LinkOffer {
    /// The offer of a link that can still admit: none once it is revoked,
    /// expired or exhausted, while a paused link still offers, marked paused.
    pub fn current(link: &LinkSnapshot, now: DateTime<Utc>) -> Option<Self> {
        let paused = match link.inactive(now) {
            None => false,
            Some(crate::Inactive::Paused) => true,
            Some(_) => return None,
        };
        Some(Self {
            link_id: link.link_id,
            account_id: link.creation.account_id,
            targets: link.creation.targets.clone(),
            approval_required: link.creation.approval_required,
            email_domains: link.creation.email_domains.clone(),
            paused,
        })
    }
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

    #[test]
    fn scope_refusal_requires_a_recent_negative_for_a_required_app() {
        use crate::targets::{RepoRole, RepoTarget};
        let now = chrono::DateTime::from_timestamp(1_800_000_000, 0).unwrap();
        let targets = Targets {
            organization: None,
            teams: vec![],
            repos: vec![RepoTarget {
                repo_id: 10,
                repo_full_name: "acme/api".into(),
                grant: RepoRole::Read,
            }],
        };
        let mut input = Admit {
            domain_evidence: None,
            link_id: InvitationLinkId::new(),
            operation_id: AdmissionOperationId::try_from(RequestId::new().to_string()).unwrap(),
            requester_id: 8,
            justification: None,
            secret_sha256: [42; 32].into(),
            secret_generation: 1,
            scope_evidence: vec![],
        };
        assert!(!input.scope_unavailable(&targets, now));
        for (age, available, app, refused) in [
            (120, false, crate::account::App::Repo, true),
            (300, false, crate::account::App::Repo, true),
            (301, false, crate::account::App::Repo, false),
            (-1, false, crate::account::App::Repo, false),
            (120, true, crate::account::App::Repo, false),
            (120, false, crate::account::App::Org, false),
        ] {
            input.scope_evidence = vec![ScopeEvidence {
                app,
                observed_at: now - chrono::Duration::seconds(age),
                available,
                unavailable_target_ids: vec![10],
            }];
            assert_eq!(input.scope_unavailable(&targets, now), refused);
        }
    }

    fn now() -> DateTime<Utc> {
        DateTime::from_timestamp(1_800_000_000, 0).unwrap()
    }

    fn link(email_domains: &[&str]) -> LinkSnapshot {
        let mut link: LinkSnapshot = serde_json::from_value(serde_json::json!({
            "link_id": "01ARZ3NDEKTSV4RRFFQ69G5FAV", "revision": 1, "uses": 0,
            "created_at": "2026-09-14T00:00:00Z", "revoked_at": null, "revoked_by": null,
            "secret": {"secret_generation": 1, "sha256": "01".repeat(32)},
            "creation": {
                "link_id": "01ARZ3NDEKTSV4RRFFQ69G5FAV",
                "admin": {"account_id": 100, "user_id": 7}, "account_id": 100,
                "installation_id": 1, "description": "Workshop", "internal_note": null,
                "expires_at": null, "max_uses": 2, "secret_sha256": "01".repeat(32),
                "approval_required": true,
                "targets": {"organization": null, "teams": [], "repos": [
                    {"repo_id": 10, "repo_full_name": "acme/api", "grant": "read"}
                ]}
            }
        }))
        .unwrap();
        link.creation.email_domains = email_domains
            .iter()
            .map(|rule| crate::email_domain::DomainRule::parse(rule).unwrap())
            .collect();
        link
    }

    fn attempt(link: &LinkSnapshot) -> Admit {
        Admit {
            domain_evidence: Some(crate::email_domain::DomainEvidence {
                link_id: link.link_id,
                matched_domains: vec![crate::email_domain::AsciiDomain::parse("acme.dev").unwrap()],
                checked_at: now(),
            }),
            link_id: link.link_id,
            operation_id: AdmissionOperationId::try_from(RequestId::new().to_string()).unwrap(),
            requester_id: 8,
            justification: Some("Workshop attendee".into()),
            secret_sha256: link.secret.sha256,
            secret_generation: 1,
            scope_evidence: vec![],
        }
    }

    #[test]
    fn acceptance_consumes_one_use_and_creates_a_pending_request() {
        let mut link = link(&[]);
        let input = attempt(&link);
        let request_id = RequestId::new();
        let request = input.accept(&mut link, request_id, true, now()).unwrap();
        assert_eq!((link.uses, link.revision), (1, 2));
        assert_eq!(
            request,
            RequestSnapshot {
                // A link without domain rules keeps no domain evidence.
                domain_evidence: None,
                request_id,
                link_id: Some(link.link_id),
                batch: None,
                account_id: 100,
                requester_id: 8,
                justification: Some("Workshop attendee".into()),
                state: RequestState::Pending,
                admitted_at: now(),
                decision_deadline: Some(now() + PENDING_LIFETIME),
                revision: 1,
                decision: None,
            }
        );
    }

    #[test]
    fn acceptance_without_approval_approves_at_once_with_no_deadline() {
        let mut link = link(&["acme.dev"]);
        let input = attempt(&link);
        let request_id = RequestId::new();
        let request = input.accept(&mut link, request_id, false, now()).unwrap();
        assert_eq!(request.state, RequestState::Approved);
        assert_eq!(request.decision_deadline, None);
        assert_eq!(request.domain_evidence, input.domain_evidence);
        assert_eq!(
            request.decision,
            Some(TerminalDecision {
                decision_id: format!("request.approved/{request_id}"),
                decided_by: None,
                effective_at: now(),
                evaluated_at: now(),
                cancellation_reason: None,
                decline_reason: None,
                requester_message: None,
            })
        );
    }

    #[test]
    fn a_use_counter_overflow_accepts_nothing() {
        let mut link = link(&[]);
        link.uses = u64::MAX;
        let before = link.clone();
        let input = attempt(&link);
        assert_eq!(input.accept(&mut link, RequestId::new(), true, now()), None);
        assert_eq!(link, before);
    }

    #[test]
    fn domain_rules_require_fresh_matching_evidence_for_this_link() {
        let link = link(&["acme.dev"]);
        let rules = &link.creation.email_domains;
        let mut input = attempt(&link);
        assert_eq!(input.domain_refusal(rules, now()), None);
        assert_eq!(input.domain_refusal(&[], now()), None);
        assert_eq!(
            input.domain_refusal(rules, now() + chrono::Duration::minutes(11)),
            Some(Rejection::DomainEvidenceStale)
        );
        let other = vec![crate::email_domain::DomainRule::parse("example.com").unwrap()];
        assert_eq!(
            input.domain_refusal(&other, now()),
            Some(Rejection::DomainMismatch { required: other })
        );
        input.domain_evidence = None;
        assert_eq!(
            input.domain_refusal(rules, now()),
            Some(Rejection::DomainEvidenceMissing)
        );
    }

    #[test]
    fn a_link_offers_until_it_can_no_longer_admit() {
        let mut link = link(&[]);
        assert_eq!(
            LinkOffer::current(&link, now()).map(|o| o.paused),
            Some(false)
        );
        link.pause = Some(crate::link_lifecycle::LinkPause {
            epoch: 2,
            at: now(),
            by: Some(7),
            reason: crate::link_lifecycle::PauseReason::Manual,
        });
        assert_eq!(
            LinkOffer::current(&link, now()).map(|o| o.paused),
            Some(true)
        );
        link.uses = 2;
        assert!(LinkOffer::current(&link, now()).is_none());
    }

    #[test]
    fn link_commands_apply_the_authority_rules() {
        let mut link = link(&[]);
        assert!(link.creation.admin.acts_for(100));
        for (account_id, user_id, link_account) in [(101, 7, 100), (0, 7, 0), (100, 0, 100)] {
            let admin = crate::invitation_link::AccountAdmin {
                account_id,
                user_id,
            };
            assert!(!admin.acts_for(link_account), "{admin:?}");
        }

        let saved = crate::invitation_link::LinkMetadata {
            description: "Workshop".into(),
            internal_note: None,
        };
        assert!(
            !link.update_metadata(saved.clone()),
            "saving again changes nothing"
        );
        assert_eq!((link.revision, link.metadata.as_ref()), (1, None));
        let renamed = crate::invitation_link::LinkMetadata {
            description: "Renamed".into(),
            ..saved
        };
        assert!(link.update_metadata(renamed.clone()));
        assert_eq!((link.revision, link.metadata.as_ref()), (2, Some(&renamed)));

        let rotated = link.rotate_secret([2; 32].into()).unwrap();
        assert_eq!(rotated.secret_generation, 2);
        assert_eq!(link.secret.secret_generation, 2);
        assert_eq!(link.secret.sha256, [2; 32].into());
        assert_eq!(link.revision, 3);
        link.secret.secret_generation = u32::MAX;
        let before = link.clone();
        assert_eq!(
            link.rotate_secret([3; 32].into()),
            Err(crate::invitation_link::RotationRefusal::Exhausted)
        );
        link.revoked_at = Some(now());
        assert_eq!(
            link.rotate_secret([3; 32].into()),
            Err(crate::invitation_link::RotationRefusal::Revoked)
        );
        link.revoked_at = None;
        assert_eq!(link, before);
    }

    #[test]
    fn only_a_pending_or_approved_request_blocks_a_new_attempt() {
        assert_eq!(
            Rejection::open_request(RequestState::Pending),
            Some(Rejection::AlreadyPending)
        );
        assert_eq!(
            Rejection::open_request(RequestState::Approved),
            Some(Rejection::AlreadyApproved)
        );
        for state in [
            RequestState::Lapsed,
            RequestState::Declined,
            RequestState::Expired,
            RequestState::Cancelled,
        ] {
            assert_eq!(Rejection::open_request(state), None, "{state}");
        }
    }
}
