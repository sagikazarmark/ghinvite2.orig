//! Append-only audit event types. The Storage trait exposes `audit(&AuditEvent)`
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
        })
    }
}

/// The full set of v1 event types per spec §15.1. Stored as the dotted strings
/// returned by `EventType::as_str`. Not enforced in SQL — the Rust enum is the
/// validation seam at the application boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EventType {
    InstallationCreated,
    InstallationReposChanged,
    InstallationUninstalled,
    InvitationLinkCreated,
    InvitationLinkMetadataUpdated,
    InvitationLinkRevoked,
    InvitationLinkExpired,
    InvitationLinkExhausted,
    RequestCreated,
    RequestApproved,
    RequestDeclined,
    RequestExpired,
    InvitationSent,
    InvitationAccepted,
    InvitationDeclined,
    InvitationExpired,
    InvitationCancelled,
    InvitationSendFailed,
}

impl EventType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InstallationCreated => "installation.created",
            Self::InstallationReposChanged => "installation.repos_changed",
            Self::InstallationUninstalled => "installation.uninstalled",
            Self::InvitationLinkCreated => "invitation_link.created",
            Self::InvitationLinkMetadataUpdated => "invitation_link.metadata_updated",
            Self::InvitationLinkRevoked => "invitation_link.revoked",
            Self::InvitationLinkExpired => "invitation_link.expired",
            Self::InvitationLinkExhausted => "invitation_link.exhausted",
            Self::RequestCreated => "request.created",
            Self::RequestApproved => "request.approved",
            Self::RequestDeclined => "request.declined",
            Self::RequestExpired => "request.expired",
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
            "installation.created" => Self::InstallationCreated,
            "installation.repos_changed" => Self::InstallationReposChanged,
            "installation.uninstalled" => Self::InstallationUninstalled,
            "invitation_link.created" => Self::InvitationLinkCreated,
            "invitation_link.metadata_updated" => Self::InvitationLinkMetadataUpdated,
            "invitation_link.revoked" => Self::InvitationLinkRevoked,
            "invitation_link.expired" => Self::InvitationLinkExpired,
            "invitation_link.exhausted" => Self::InvitationLinkExhausted,
            "request.created" => Self::RequestCreated,
            "request.approved" => Self::RequestApproved,
            "request.declined" => Self::RequestDeclined,
            "request.expired" => Self::RequestExpired,
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
        EventType::InvitationLinkExpired,
        EventType::InvitationLinkExhausted,
        EventType::RequestCreated,
        EventType::RequestApproved,
        EventType::RequestDeclined,
        EventType::RequestExpired,
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
