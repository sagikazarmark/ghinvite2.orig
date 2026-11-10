//! Retained transition facts travel independently of projection freshness.
//!
//! The delivery owner deduplicates semantic states and retains a bounded pending
//! batch before sending it. A successful durable send acknowledges that batch,
//! not SQL completion. Projectors validate identity and insert every fact even
//! when the accompanying snapshot has already been superseded.
use crate::{
    audit::{ActorKind, AuditEvent, EventType, TargetKind},
    delivery::{BlockedReason, DeliverySnapshot},
    org_delivery::{OrganizationBlockedReason, OrganizationDeliverySnapshot},
};
use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// The audit contract is chosen before the first retained delivery snapshot and
/// cannot change within that delivery generation. Missing fields retain legacy
/// event identities and payloads, including after a newer endpoint is deployed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryAuditVersion {
    #[default]
    Legacy,
    CanonicalV1,
    CanonicalV2,
}
impl DeliveryAuditVersion {
    pub fn is_legacy(&self) -> bool {
        *self == Self::Legacy
    }
}

/// Bind compatibility identity to its immutable audit contract.
pub fn validate_identity(
    version: DeliveryAuditVersion,
    id: Option<crate::GithubInvitationId>,
) -> Result<(), &'static str> {
    if (version == DeliveryAuditVersion::CanonicalV2) == id.is_none() {
        Ok(())
    } else {
        Err("delivery audit version/compatibility identity mismatch")
    }
}
pub fn new_owner_version(id: Option<crate::GithubInvitationId>) -> DeliveryAuditVersion {
    if id.is_some() {
        DeliveryAuditVersion::CanonicalV1
    } else {
        DeliveryAuditVersion::CanonicalV2
    }
}
pub fn repository_target(
    receipt: &crate::delivery::CreateReceipt,
) -> Result<(TargetKind, String), &'static str> {
    validate_identity(receipt.audit_version, receipt.command.invitation_id)?;
    Ok(match receipt.command.invitation_id {
        Some(id) => (TargetKind::GithubInvitation, id.to_string()),
        None => (
            TargetKind::RepositoryDelivery,
            receipt.command.delivery_key(),
        ),
    })
}
pub fn repository_settlement_id(
    receipt: &crate::delivery::CreateReceipt,
) -> Result<crate::AuditEventId, &'static str> {
    validate_identity(receipt.audit_version, receipt.command.invitation_id)?;
    if let Some(id) = receipt.command.invitation_id {
        return Ok(crate::AuditEventId::from_ulid(id.as_ulid()));
    }
    let digest = Sha256::digest(format!(
        "ghinvite/delivery/audit/v2/{}/settlement",
        receipt.command.delivery_key()
    ));
    Ok(crate::AuditEventId::from_ulid(ulid::Ulid::from_bytes(
        digest[..16].try_into().expect("digest length"),
    )))
}

pub fn organization_settlement_kind(
    version: DeliveryAuditVersion,
    state: crate::InvitationState,
) -> Result<EventType, &'static str> {
    use crate::InvitationState::*;
    Ok(match state {
        Accepted if !version.is_legacy() => EventType::OrgInvitationAccepted,
        Accepted => EventType::InvitationAccepted,
        Declined => EventType::InvitationDeclined,
        Expired => EventType::InvitationExpired,
        Cancelled => EventType::InvitationCancelled,
        _ => return Err("invalid organization settlement state"),
    })
}

pub fn organization_settlement_id(
    version: DeliveryAuditVersion,
    command: &crate::org_delivery::OrganizationCreateCommand,
) -> crate::AuditEventId {
    if version.is_legacy() {
        return crate::AuditEventId::from_ulid(
            command
                .invitation_id
                .expect("validated legacy audit identity")
                .as_ulid(),
        );
    }
    let namespace = if version == DeliveryAuditVersion::CanonicalV2 {
        "v2"
    } else {
        "v1"
    };
    let digest = Sha256::digest(format!(
        "ghinvite/org-delivery/audit/{namespace}/{}/settlement",
        command.storage_id()
    ));
    crate::AuditEventId::from_ulid(ulid::Ulid::from_bytes(
        digest[..16].try_into().expect("digest length"),
    ))
}

/// One transition can release and change all 100 team steps plus the org step.
/// This is a transport batch bound, not a lifetime audit-retention limit.
pub const MAX_AUDIT_FACTS: usize = 256;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum DeliveryAuditStep {
    Repository,
    Organization,
    Team { team_id: u64 },
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "kind",
    content = "reason",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum DeliveryBlockedReason {
    Repository(BlockedReason),
    Organization(OrganizationBlockedReason),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum DeliveryAuditKind {
    Blocked { reason: DeliveryBlockedReason },
    OutcomeUnknown,
    Held { hold_epoch: u64 },
    Released { hold_epoch: u64 },
}
impl DeliveryAuditKind {
    fn event_type(self) -> EventType {
        match self {
            Self::Blocked { .. } => EventType::DeliveryBlocked,
            Self::OutcomeUnknown => EventType::DeliveryOutcomeUnknown,
            Self::Held { .. } => EventType::DeliveryHeld,
            Self::Released { .. } => EventType::DeliveryReleased,
        }
    }
    fn phase(self) -> &'static str {
        match self {
            Self::Blocked { .. } => "blocked",
            Self::OutcomeUnknown => "unknown",
            Self::Held { .. } => "held",
            Self::Released { .. } => "released",
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DeliveryAuditFact {
    pub step: DeliveryAuditStep,
    pub transition_revision: u64,
    pub at: DateTime<Utc>,
    pub kind: DeliveryAuditKind,
}

/// Flattening preserves the previous `DeliveryProjection.apply` JSON shape.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RepositoryDeliveryProjection {
    #[serde(flatten)]
    pub snapshot: DeliverySnapshot,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub audit_events: Vec<DeliveryAuditFact>,
}
#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
pub struct OrganizationDeliveryProjection {
    #[serde(flatten)]
    pub snapshot: OrganizationDeliverySnapshot,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub audit_events: Vec<DeliveryAuditFact>,
}
// Serde's flattened ContentDeserializer does not apply JSON's numeric map-key
// conversion: a team key "5" would fail to decode as u64. Re-enter the JSON
// deserializer for the complete snapshot rather than flattening its fields.
impl<'de> Deserialize<'de> for OrganizationDeliveryProjection {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::Error as _;
        let mut value = serde_json::Value::deserialize(deserializer)?;
        let object = value
            .as_object_mut()
            .ok_or_else(|| D::Error::custom("organization projection must be an object"))?;
        let audit_events = object
            .remove("audit_events")
            .map(serde_json::from_value)
            .transpose()
            .map_err(D::Error::custom)?
            .unwrap_or_default();
        let snapshot = serde_json::from_value(value).map_err(D::Error::custom)?;
        Ok(Self {
            snapshot,
            audit_events,
        })
    }
}

impl From<DeliverySnapshot> for RepositoryDeliveryProjection {
    fn from(snapshot: DeliverySnapshot) -> Self {
        Self {
            snapshot,
            audit_events: vec![],
        }
    }
}
impl From<OrganizationDeliverySnapshot> for OrganizationDeliveryProjection {
    fn from(snapshot: OrganizationDeliverySnapshot) -> Self {
        Self {
            snapshot,
            audit_events: vec![],
        }
    }
}

fn positive(value: u64) -> bool {
    value > 0 && value <= crate::storage::integer::MAX_EXACT
}
impl DeliveryAuditFact {
    fn validate(&self, approved_at: DateTime<Utc>, revision: u64) -> Result<(), &'static str> {
        if !positive(self.transition_revision)
            || self.transition_revision > revision
            || self.at < approved_at
        {
            return Err("invalid delivery audit revision or time");
        }
        if let DeliveryAuditKind::Held { hold_epoch } | DeliveryAuditKind::Released { hold_epoch } =
            self.kind
            && !positive(hold_epoch)
        {
            return Err("invalid delivery audit hold epoch");
        }
        Ok(())
    }
    fn event(
        &self,
        physical_id: &str,
        account_id: u64,
        request_id: crate::RequestId,
        generation: u64,
        target_kind: TargetKind,
        target_id: String,
    ) -> AuditEvent {
        let step = match self.step {
            DeliveryAuditStep::Repository => "repo".into(),
            DeliveryAuditStep::Organization => "org".into(),
            DeliveryAuditStep::Team { team_id } => format!("team:{team_id}"),
        };
        // Payload, timestamp, reason and epoch are intentionally outside the
        // identity. A changed fact for the same transition must conflict.
        let digest = Sha256::digest(format!(
            "ghinvite/delivery-transition/v1/{physical_id}/{step}/{}/{}",
            self.transition_revision,
            self.kind.phase()
        ));
        AuditEvent {
            id: crate::AuditEventId::from_ulid(ulid::Ulid::from_bytes(
                digest[..16].try_into().expect("digest length"),
            )),
            account_id,
            occurred_at: self.at,
            event_type: self.kind.event_type(),
            actor_kind: ActorKind::System,
            actor_id: None,
            target_kind,
            target_id,
            metadata: serde_json::json!({"delivery_id":physical_id,"delivery_generation":generation,"step":self.step,"transition_revision":self.transition_revision,"transition":self.kind}),
            request_id: Some(request_id.to_string()),
        }
    }
}
impl RepositoryDeliveryProjection {
    pub fn events(&self) -> Result<Vec<AuditEvent>, &'static str> {
        let command = &self.snapshot.create.command;
        let (target_kind, target_id) = repository_target(&self.snapshot.create)?;
        crate::bulk::validate_origin(command.link_id, command.batch)?;
        if self.audit_events.len() > MAX_AUDIT_FACTS
            || ![
                command.account_id,
                command.installation_id,
                command.requester_id,
                command.repo_id,
                command.delivery_generation,
                self.snapshot.revision,
            ]
            .into_iter()
            .all(positive)
        {
            return Err("invalid repository audit identity or batch");
        }
        self.audit_events
            .iter()
            .map(|fact| {
                fact.validate(command.approved_at, self.snapshot.revision)?;
                if fact.step != DeliveryAuditStep::Repository
                    || matches!(
                        fact.kind,
                        DeliveryAuditKind::Blocked {
                            reason: DeliveryBlockedReason::Organization(_)
                        }
                    )
                {
                    return Err("repository audit step or reason mismatch");
                }
                Ok(fact.event(
                    &command.delivery_key(),
                    command.account_id,
                    command.request_id,
                    command.delivery_generation,
                    target_kind,
                    target_id.clone(),
                ))
            })
            .collect()
    }
}
impl OrganizationDeliveryProjection {
    pub fn events(&self) -> Result<Vec<AuditEvent>, &'static str> {
        let command = &self.snapshot.create.command;
        command.validate()?;
        if self.audit_events.len() > MAX_AUDIT_FACTS || !positive(self.snapshot.revision) {
            return Err("invalid organization audit revision or batch");
        }
        self.audit_events
            .iter()
            .map(|fact| {
                fact.validate(command.approved_at, self.snapshot.revision)?;
                let (kind, target) = match fact.step {
                    DeliveryAuditStep::Organization => {
                        (TargetKind::OrganizationDelivery, command.storage_id())
                    }
                    DeliveryAuditStep::Team { team_id }
                        if command.teams.iter().any(|team| team.team_id == team_id) =>
                    {
                        (
                            TargetKind::TeamDelivery,
                            format!("{}/team:{team_id}", command.storage_id()),
                        )
                    }
                    _ => return Err("organization audit step mismatch"),
                };
                if matches!(
                    fact.kind,
                    DeliveryAuditKind::Blocked {
                        reason: DeliveryBlockedReason::Repository(_)
                    }
                ) {
                    return Err("organization audit reason mismatch");
                }
                Ok(fact.event(
                    &command.storage_id(),
                    command.account_id,
                    command.request_id,
                    command.delivery_generation,
                    kind,
                    target,
                ))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    pub(super) fn repository() -> RepositoryDeliveryProjection {
        serde_json::from_value(serde_json::json!({"create":{"command":{
            "invitation_id":crate::GithubInvitationId::new(),"link_id":crate::InvitationLinkId::new(),"request_id":crate::RequestId::new(),
            "approval_id":"approval","account_id":9001,"installation_id":1,"requester_id":701,"repo_id":5,"repo_full_name":"acme/api","permission":"pull","approved_at":"2026-05-04T13:00:00Z"
        },"outcome":{"kind":"outcome_unknown"},"revision":10,"confirmed_at":null},"revision":10,"settlement":null})).unwrap()
    }
    pub(super) fn fact() -> DeliveryAuditFact {
        DeliveryAuditFact {
            step: DeliveryAuditStep::Repository,
            transition_revision: 3,
            at: DateTime::parse_from_rfc3339("2026-05-04T13:01:00Z")
                .unwrap()
                .with_timezone(&Utc),
            kind: DeliveryAuditKind::OutcomeUnknown,
        }
    }
    #[test]
    fn compatibility_identity_preserves_original_wire_and_v2_omits_it() {
        let wire = r#"{"invitation_id":"01ARZ3NDEKTSV4RRFFQ69G5FAV","delivery_generation":1,"link_id":"01ARZ3NDEKTSV4RRFFQ69G5FAW","request_id":"01ARZ3NDEKTSV4RRFFQ69G5FAX","approval_id":"approval","account_id":9001,"installation_id":1,"requester_id":701,"repo_id":5,"repo_full_name":"acme/api","permission":"pull","approved_at":"2026-05-04T13:00:00Z"}"#;
        let mut command: crate::delivery::CreateCommand = serde_json::from_str(wire).unwrap();
        assert_eq!(serde_json::to_string(&command).unwrap(), wire);
        for version in [
            DeliveryAuditVersion::Legacy,
            DeliveryAuditVersion::CanonicalV1,
        ] {
            assert!(validate_identity(version, command.invitation_id).is_ok());
            assert!(validate_identity(version, None).is_err());
        }
        assert!(
            validate_identity(DeliveryAuditVersion::CanonicalV2, command.invitation_id).is_err()
        );
        command.invitation_id = None;
        assert!(validate_identity(DeliveryAuditVersion::CanonicalV2, None).is_ok());
        let encoded = serde_json::to_string(&command).unwrap();
        assert!(!encoded.contains("invitation_id"));
        assert_eq!(
            serde_json::from_str::<crate::delivery::CreateCommand>(&encoded).unwrap(),
            command
        );
    }

    #[test]
    fn empty_envelope_preserves_old_wire_and_only_fact_payload_changes_conflict_identity() {
        let mut envelope = repository();
        assert_eq!(
            serde_json::to_value(&envelope).unwrap(),
            serde_json::to_value(&envelope.snapshot).unwrap()
        );
        envelope.audit_events.push(fact());
        let original = envelope.events().unwrap().remove(0);
        envelope.audit_events[0].at += chrono::Duration::seconds(1);
        let changed = envelope.events().unwrap().remove(0);
        assert_eq!(original.id, changed.id);
        assert_ne!(original, changed);
        envelope.audit_events[0].transition_revision += 1;
        assert_ne!(original.id, envelope.events().unwrap()[0].id);
        envelope.audit_events[0] = fact();
        envelope.snapshot.create.command.delivery_generation += 1;
        assert_ne!(original.id, envelope.events().unwrap()[0].id);
    }
    #[test]
    fn repository_rejects_wrong_step_reason_time_revision_and_epoch() {
        let mut envelope = repository();
        let mut invalid = vec![];
        for revision in [0, 11, crate::storage::integer::MAX_EXACT + 1] {
            invalid.push(DeliveryAuditFact {
                transition_revision: revision,
                ..fact()
            });
        }
        invalid.push(DeliveryAuditFact {
            at: envelope.snapshot.create.command.approved_at - chrono::Duration::nanoseconds(1),
            ..fact()
        });
        invalid.push(DeliveryAuditFact {
            step: DeliveryAuditStep::Organization,
            ..fact()
        });
        invalid.push(DeliveryAuditFact {
            step: DeliveryAuditStep::Team { team_id: 5 },
            ..fact()
        });
        invalid.push(DeliveryAuditFact {
            kind: DeliveryAuditKind::Blocked {
                reason: DeliveryBlockedReason::Organization(OrganizationBlockedReason::NoSeat),
            },
            ..fact()
        });
        invalid.push(DeliveryAuditFact {
            kind: DeliveryAuditKind::Held { hold_epoch: 0 },
            ..fact()
        });
        invalid.push(DeliveryAuditFact {
            kind: DeliveryAuditKind::Released {
                hold_epoch: crate::storage::integer::MAX_EXACT + 1,
            },
            ..fact()
        });
        for fact in invalid {
            envelope.audit_events = vec![fact];
            assert!(envelope.events().is_err());
        }
        envelope.audit_events = vec![fact(); MAX_AUDIT_FACTS + 1];
        assert!(envelope.events().is_err());
    }
    #[test]
    fn organization_projection_round_trips_numeric_team_keys_with_retained_facts() {
        let command: crate::org_delivery::OrganizationCreateCommand = serde_json::from_value(serde_json::json!({
            "invitation_id":crate::GithubInvitationId::new(),"link_id":crate::InvitationLinkId::new(),"request_id":crate::RequestId::new(),
            "approval_id":"approval","account_id":9001,"installation_id":1,"requester_id":701,
            "organization":{"org_id":9001,"org_login":"acme","grant":"member"},
            "teams":[{"team_id":5,"team_slug":"api","grant":"member"}],
            "approved_at":"2026-05-04T13:00:00Z"
        })).unwrap();
        let snapshot = OrganizationDeliverySnapshot::planned(command).unwrap();
        let mut envelope = OrganizationDeliveryProjection::from(snapshot.clone());
        let old_wire = serde_json::to_string(&snapshot).unwrap();
        assert_eq!(
            serde_json::from_str::<OrganizationDeliveryProjection>(&old_wire).unwrap(),
            envelope
        );
        envelope.audit_events.push(DeliveryAuditFact {
            step: DeliveryAuditStep::Team { team_id: 5 },
            transition_revision: 1,
            ..fact()
        });
        let wire = serde_json::to_string(&envelope).unwrap();
        assert_eq!(
            serde_json::from_str::<OrganizationDeliveryProjection>(&wire).unwrap(),
            envelope
        );
        let value = serde_json::to_value(&envelope).unwrap();
        assert_eq!(
            serde_json::from_value::<OrganizationDeliveryProjection>(value.clone()).unwrap(),
            envelope
        );
        let mut invalid = value.clone();
        invalid["audit_events"] = serde_json::Value::Null;
        assert!(serde_json::from_value::<OrganizationDeliveryProjection>(invalid).is_err());
        let mut invalid = value;
        let team = invalid["teams"]
            .as_object_mut()
            .unwrap()
            .remove("5")
            .unwrap();
        invalid["teams"]["not-a-numeric-team"] = team;
        assert!(serde_json::from_value::<OrganizationDeliveryProjection>(invalid).is_err());
    }

    #[test]
    fn hold_and_release_share_epoch_but_have_distinct_phase_identity() {
        let mut envelope = repository();
        envelope.audit_events = vec![
            DeliveryAuditFact {
                kind: DeliveryAuditKind::Held { hold_epoch: 4 },
                ..fact()
            },
            DeliveryAuditFact {
                kind: DeliveryAuditKind::Released { hold_epoch: 4 },
                ..fact()
            },
        ];
        let events = envelope.events().unwrap();
        assert_ne!(events[0].id, events[1].id);
        assert_eq!(
            events[0].metadata["transition"]["hold_epoch"],
            events[1].metadata["transition"]["hold_epoch"]
        );
        let id = events[0].id;
        envelope.audit_events[0].kind = DeliveryAuditKind::Held { hold_epoch: 5 };
        assert_eq!(
            id,
            envelope.events().unwrap()[0].id,
            "changed epoch must conflict, not create another event"
        );
    }
}

#[cfg(test)]
mod canonical_tests;
