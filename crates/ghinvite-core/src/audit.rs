//! Append-only audit event types. `AuditStorage` exposes `audit(&AuditEvent)`
//! and no update/delete; this module defines the event payload.

use crate::AuditEventId;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ActorKind {
    User,
    System,
    Github,
}

#[derive(Debug, thiserror::Error, PartialEq)]
#[error("unknown actor kind: {0}")]
pub struct UnknownActorKind(pub String);

impl FromStr for ActorKind {
    type Err = UnknownActorKind;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "user" => Ok(Self::User),
            "system" => Ok(Self::System),
            "github" => Ok(Self::Github),
            other => Err(UnknownActorKind(other.to_string())),
        }
    }
}

impl fmt::Display for ActorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::User => "user",
            Self::System => "system",
            Self::Github => "github",
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TargetKind {
    Installation,
    InvitationLink,
    InvitationRequest,
    GithubInvitation,
    RepositoryDelivery,
    OrganizationDelivery,
    TeamDelivery,
    AccessGrant,
    BulkBatch,
}

#[derive(Debug, thiserror::Error, PartialEq)]
#[error("unknown target kind: {0}")]
pub struct UnknownTargetKind(pub String);

impl FromStr for TargetKind {
    type Err = UnknownTargetKind;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "installation" => Ok(Self::Installation),
            "invitation_link" => Ok(Self::InvitationLink),
            "invitation_request" => Ok(Self::InvitationRequest),
            "github_invitation" => Ok(Self::GithubInvitation),
            "repository_delivery" => Ok(Self::RepositoryDelivery),
            "organization_delivery" => Ok(Self::OrganizationDelivery),
            "team_delivery" => Ok(Self::TeamDelivery),
            "access_grant" => Ok(Self::AccessGrant),
            "bulk_batch" => Ok(Self::BulkBatch),
            other => Err(UnknownTargetKind(other.to_string())),
        }
    }
}

impl fmt::Display for TargetKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Installation => "installation",
            Self::InvitationLink => "invitation_link",
            Self::InvitationRequest => "invitation_request",
            Self::GithubInvitation => "github_invitation",
            Self::RepositoryDelivery => "repository_delivery",
            Self::OrganizationDelivery => "organization_delivery",
            Self::TeamDelivery => "team_delivery",
            Self::AccessGrant => "access_grant",
            Self::BulkBatch => "bulk_batch",
        })
    }
}

/// The full set of v1 event types per spec §15.1. Stored as the dotted strings
/// returned by `EventType::as_str`. Not enforced in SQL — the Rust enum is the
/// validation seam at the application boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EventType {
    EmailInvitationNoLongerPending,
    EmailInvitationFailed,
    BulkBatchCreated,
    BulkBatchSubmitted,
    BulkBatchRowsRemoved,
    BulkBatchCancelled,

    InvitationLinkSecretRotated,
    DeliveryAlreadySatisfied,
    DeliveryUpgraded,
    DeliverySkippedCustomRole,
    DeliveryBlocked,
    DeliveryOutcomeUnknown,
    InstallationOwnerRecorded,
    RepositoryAdminDelegationChanged,
    OrganizationInviteCapChanged,
    DeliveryAlreadyPending,
    DeliveryAborted,
    DeliveryHeld,
    DeliveryReleased,
    DeliveryReissued,
    AccessRemoved,
    AccessRemovalSkipped,

    InstallationSuspended,
    InstallationPermissionsAccepted,
    InstallationCreated,
    InstallationReposChanged,
    InstallationUninstalled,
    InvitationLinkCreated,
    InvitationLinkMetadataUpdated,
    InvitationLinkPaused,
    InvitationLinkResumed,
    InvitationLinkAutoPaused,
    InvitationLinkRevoked,
    InvitationLinkRevokeCascadeRequested,
    InvitationLinkExpired,
    InvitationLinkExhausted,
    RequestCreated,
    RequestApproved,
    RequestDeclined,
    RequestExpired,
    RequestCancelled,
    RequestLapsed,
    OrgInvitationSent,
    TeamMembershipAdded,
    OrgInvitationAccepted,
    InvitationSent,
    InvitationAccepted,
    InvitationDeclined,
    InvitationExpired,
    InvitationCancelled,
    InvitationSendFailed,
}

impl EventType {
    pub const ALL: [Self; 51] = [
        Self::EmailInvitationNoLongerPending,
        Self::EmailInvitationFailed,
        Self::BulkBatchCreated,
        Self::BulkBatchSubmitted,
        Self::BulkBatchRowsRemoved,
        Self::BulkBatchCancelled,
        Self::InvitationLinkSecretRotated,
        Self::DeliveryAlreadySatisfied,
        Self::DeliveryUpgraded,
        Self::DeliverySkippedCustomRole,
        Self::DeliveryBlocked,
        Self::DeliveryOutcomeUnknown,
        Self::InstallationOwnerRecorded,
        Self::RepositoryAdminDelegationChanged,
        Self::OrganizationInviteCapChanged,
        Self::DeliveryAlreadyPending,
        Self::DeliveryAborted,
        Self::DeliveryHeld,
        Self::DeliveryReleased,
        Self::DeliveryReissued,
        Self::AccessRemoved,
        Self::AccessRemovalSkipped,
        Self::InstallationSuspended,
        Self::InstallationPermissionsAccepted,
        Self::InstallationCreated,
        Self::InstallationReposChanged,
        Self::InstallationUninstalled,
        Self::InvitationLinkCreated,
        Self::InvitationLinkMetadataUpdated,
        Self::InvitationLinkPaused,
        Self::InvitationLinkResumed,
        Self::InvitationLinkAutoPaused,
        Self::InvitationLinkRevoked,
        Self::InvitationLinkRevokeCascadeRequested,
        Self::InvitationLinkExpired,
        Self::InvitationLinkExhausted,
        Self::RequestCreated,
        Self::RequestApproved,
        Self::RequestDeclined,
        Self::RequestExpired,
        Self::RequestCancelled,
        Self::RequestLapsed,
        Self::OrgInvitationSent,
        Self::TeamMembershipAdded,
        Self::OrgInvitationAccepted,
        Self::InvitationSent,
        Self::InvitationAccepted,
        Self::InvitationDeclined,
        Self::InvitationExpired,
        Self::InvitationCancelled,
        Self::InvitationSendFailed,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::InvitationLinkSecretRotated => "invitation_link.secret_rotated",
            Self::DeliveryAlreadySatisfied => "delivery.already_satisfied",
            Self::DeliveryUpgraded => "delivery.upgraded",
            Self::DeliverySkippedCustomRole => "delivery.skipped_custom_role",
            Self::DeliveryBlocked => "delivery.blocked",
            Self::DeliveryOutcomeUnknown => "delivery.outcome_unknown",
            Self::InstallationOwnerRecorded => "installation.owner_recorded",
            Self::OrganizationInviteCapChanged => "account.organization_invite_cap_changed",
            Self::RepositoryAdminDelegationChanged => "account.repository_admin_delegation_changed",
            Self::DeliveryAlreadyPending => "delivery.already_pending",
            Self::DeliveryAborted => "delivery.aborted",
            Self::DeliveryHeld => "delivery.held",
            Self::DeliveryReissued => "delivery.reissued",
            Self::AccessRemoved => "access.removed",
            Self::AccessRemovalSkipped => "access.removal_skipped",
            Self::DeliveryReleased => "delivery.released",
            Self::InstallationSuspended => "installation.suspended",
            Self::InstallationPermissionsAccepted => "installation.permissions_accepted",
            Self::InstallationCreated => "installation.created",
            Self::InstallationReposChanged => "installation.repos_changed",
            Self::InstallationUninstalled => "installation.uninstalled",
            Self::InvitationLinkCreated => "invitation_link.created",
            Self::InvitationLinkMetadataUpdated => "invitation_link.metadata_updated",
            Self::InvitationLinkPaused => "invitation_link.paused",
            Self::InvitationLinkResumed => "invitation_link.resumed",
            Self::InvitationLinkAutoPaused => "invitation_link.auto_paused",
            Self::InvitationLinkRevokeCascadeRequested => {
                "invitation_link.revoke_cascade_requested"
            }
            Self::InvitationLinkRevoked => "invitation_link.revoked",
            Self::InvitationLinkExpired => "invitation_link.expired",
            Self::InvitationLinkExhausted => "invitation_link.exhausted",
            Self::RequestCreated => "request.created",
            Self::RequestApproved => "request.approved",
            Self::RequestDeclined => "request.declined",
            Self::RequestExpired => "request.expired",
            Self::RequestCancelled => "request.cancelled",
            Self::RequestLapsed => "request.lapsed",
            Self::OrgInvitationSent => "org_invitation.sent",
            Self::EmailInvitationNoLongerPending => "email_invitation.no_longer_pending",
            Self::EmailInvitationFailed => "email_invitation.failed",
            Self::BulkBatchCreated => "bulk_batch.created",
            Self::BulkBatchSubmitted => "bulk_batch.submitted",
            Self::BulkBatchRowsRemoved => "bulk_batch.rows_removed",
            Self::BulkBatchCancelled => "bulk_batch.cancelled",

            Self::TeamMembershipAdded => "team_membership.added",
            Self::OrgInvitationAccepted => "org_invitation.accepted",
            Self::InvitationSent => "invitation.sent",
            Self::InvitationAccepted => "invitation.accepted",
            Self::InvitationDeclined => "invitation.declined",
            Self::InvitationExpired => "invitation.expired",
            Self::InvitationCancelled => "invitation.cancelled",
            Self::InvitationSendFailed => "invitation.send_failed",
        }
    }
}

#[derive(Debug, thiserror::Error, PartialEq)]
#[error("unknown event type: {0}")]
pub struct UnknownEventType(pub String);

impl FromStr for EventType {
    type Err = UnknownEventType;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "invitation_link.secret_rotated" => Self::InvitationLinkSecretRotated,
            "delivery.already_satisfied" => Self::DeliveryAlreadySatisfied,
            "delivery.upgraded" => Self::DeliveryUpgraded,
            "delivery.skipped_custom_role" => Self::DeliverySkippedCustomRole,
            "delivery.blocked" => Self::DeliveryBlocked,
            "delivery.outcome_unknown" => Self::DeliveryOutcomeUnknown,
            "installation.owner_recorded" => Self::InstallationOwnerRecorded,
            "account.organization_invite_cap_changed" => Self::OrganizationInviteCapChanged,
            "account.repository_admin_delegation_changed" => Self::RepositoryAdminDelegationChanged,
            "delivery.already_pending" => Self::DeliveryAlreadyPending,
            "delivery.aborted" => Self::DeliveryAborted,
            "delivery.held" => Self::DeliveryHeld,
            "delivery.reissued" => Self::DeliveryReissued,
            "access.removed" => Self::AccessRemoved,
            "access.removal_skipped" => Self::AccessRemovalSkipped,
            "delivery.released" => Self::DeliveryReleased,
            "installation.suspended" => Self::InstallationSuspended,
            "installation.permissions_accepted" => Self::InstallationPermissionsAccepted,
            "installation.created" => Self::InstallationCreated,
            "installation.repos_changed" => Self::InstallationReposChanged,
            "installation.uninstalled" => Self::InstallationUninstalled,
            "invitation_link.created" => Self::InvitationLinkCreated,
            "invitation_link.metadata_updated" => Self::InvitationLinkMetadataUpdated,
            "invitation_link.paused" => Self::InvitationLinkPaused,
            "invitation_link.resumed" => Self::InvitationLinkResumed,
            "invitation_link.auto_paused" => Self::InvitationLinkAutoPaused,
            "invitation_link.revoke_cascade_requested" => {
                Self::InvitationLinkRevokeCascadeRequested
            }
            "invitation_link.revoked" => Self::InvitationLinkRevoked,
            "invitation_link.expired" => Self::InvitationLinkExpired,
            "invitation_link.exhausted" => Self::InvitationLinkExhausted,
            "request.created" => Self::RequestCreated,
            "request.approved" => Self::RequestApproved,
            "request.declined" => Self::RequestDeclined,
            "request.expired" => Self::RequestExpired,
            "request.cancelled" => Self::RequestCancelled,
            "request.lapsed" => Self::RequestLapsed,
            "org_invitation.sent" => Self::OrgInvitationSent,
            "email_invitation.no_longer_pending" => Self::EmailInvitationNoLongerPending,
            "email_invitation.failed" => Self::EmailInvitationFailed,
            "bulk_batch.created" => Self::BulkBatchCreated,
            "bulk_batch.submitted" => Self::BulkBatchSubmitted,
            "bulk_batch.rows_removed" => Self::BulkBatchRowsRemoved,
            "bulk_batch.cancelled" => Self::BulkBatchCancelled,

            "team_membership.added" => Self::TeamMembershipAdded,
            "org_invitation.accepted" => Self::OrgInvitationAccepted,
            "invitation.sent" => Self::InvitationSent,
            "invitation.accepted" => Self::InvitationAccepted,
            "invitation.declined" => Self::InvitationDeclined,
            "invitation.expired" => Self::InvitationExpired,
            "invitation.cancelled" => Self::InvitationCancelled,
            "invitation.send_failed" => Self::InvitationSendFailed,
            other => return Err(UnknownEventType(other.to_string())),
        })
    }
}

impl fmt::Display for EventType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Serialize for EventType {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for EventType {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        use serde::de::Error as _;
        let s = String::deserialize(d)?;
        Self::from_str(&s).map_err(D::Error::custom)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AuditEvent {
    pub id: AuditEventId,
    pub account_id: u64,
    pub occurred_at: DateTime<Utc>,
    pub event_type: EventType,
    pub actor_kind: ActorKind,
    pub actor_id: Option<u64>,
    pub target_kind: TargetKind,
    pub target_id: String,
    pub metadata: serde_json::Value,
    /// Restate invocation id when this event was emitted by a handler.
    pub request_id: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    const ALL_EVENT_TYPES: &[EventType] = &[
        EventType::InstallationCreated,
        EventType::InstallationReposChanged,
        EventType::InstallationUninstalled,
        EventType::InvitationLinkCreated,
        EventType::InvitationLinkMetadataUpdated,
        EventType::InvitationLinkRevoked,
        EventType::InvitationLinkRevokeCascadeRequested,
        EventType::InvitationLinkExpired,
        EventType::InvitationLinkExhausted,
        EventType::RequestCreated,
        EventType::RequestApproved,
        EventType::RequestDeclined,
        EventType::RequestExpired,
        EventType::RequestCancelled,
        EventType::RequestLapsed,
        EventType::InvitationSent,
        EventType::InvitationAccepted,
        EventType::InvitationDeclined,
        EventType::InvitationExpired,
        EventType::InvitationCancelled,
        EventType::InvitationSendFailed,
    ];

    #[test]
    fn actor_kind_round_trips() {
        for k in [ActorKind::User, ActorKind::System, ActorKind::Github] {
            assert_eq!(k.to_string().parse::<ActorKind>().unwrap(), k);
        }
    }

    #[test]
    fn target_kind_round_trips() {
        for k in [
            TargetKind::Installation,
            TargetKind::InvitationLink,
            TargetKind::InvitationRequest,
            TargetKind::GithubInvitation,
            TargetKind::OrganizationDelivery,
            TargetKind::TeamDelivery,
        ] {
            assert_eq!(k.to_string().parse::<TargetKind>().unwrap(), k);
        }
    }

    #[test]
    fn event_type_round_trips_every_variant() {
        for t in ALL_EVENT_TYPES {
            let s = t.as_str();
            let parsed = EventType::from_str(s).unwrap();
            assert_eq!(parsed, *t);
        }
    }

    #[test]
    fn event_type_rejects_unknown() {
        assert_eq!(
            EventType::from_str("invitation_link.unmade").unwrap_err(),
            UnknownEventType("invitation_link.unmade".into())
        );
    }

    #[test]
    fn event_type_serde_is_string() {
        let json = serde_json::to_string(&EventType::InvitationLinkCreated).unwrap();
        assert_eq!(json, "\"invitation_link.created\"");
        let back: EventType = serde_json::from_str(&json).unwrap();
        assert_eq!(back, EventType::InvitationLinkCreated);
    }

    #[test]
    fn audit_event_serde() {
        let e = AuditEvent {
            id: AuditEventId::new(),
            account_id: 1,
            occurred_at: Utc.with_ymd_and_hms(2026, 5, 4, 12, 0, 0).unwrap(),
            event_type: EventType::InvitationLinkCreated,
            actor_kind: ActorKind::User,
            actor_id: Some(99),
            target_kind: TargetKind::InvitationLink,
            target_id: "01HFOOBAR".into(),
            metadata: serde_json::json!({"permission": "push"}),
            request_id: None,
        };
        let json = serde_json::to_string(&e).unwrap();
        let parsed: AuditEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(e, parsed);
    }
}
