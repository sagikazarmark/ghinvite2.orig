//! Email-addressed organization deliveries have no requester or request lifecycle.
//! The raw mailbox is absent from every type here; only the protected vault ref
//! may cross the journal boundary. A lost create response cannot be recovered by
//! guessing a user or matching an address in an invitation list.
use crate::{
    bulk::BatchRef,
    targets::{OrganizationTarget, Targets, TeamTarget},
};
use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Random 256-bit vault identity generated at the web edge, not an email hash.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(try_from = "String", into = "String")]
pub struct EmailRef([u8; 32]);
impl EmailRef {
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
    pub fn as_hex(self) -> String {
        self.0.iter().map(|b| format!("{b:02x}")).collect()
    }
}
impl std::fmt::Debug for EmailRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("EmailRef([REDACTED])")
    }
}
impl From<EmailRef> for String {
    fn from(value: EmailRef) -> Self {
        value.as_hex()
    }
}
impl TryFrom<String> for EmailRef {
    type Error = &'static str;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        if value.len() != 64
            || !value
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err("invalid email reference");
        }
        let digit = |b: u8| if b <= b'9' { b - b'0' } else { b - b'a' + 10 };
        let mut bytes = [0; 32];
        for (byte, pair) in bytes.iter_mut().zip(value.as_bytes().as_chunks::<2>().0) {
            *byte = digit(pair[0]) * 16 + digit(pair[1]);
        }
        Ok(Self(bytes))
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EmailCreateCommand {
    pub batch: BatchRef,
    pub account_id: u64,
    pub installation_id: u64,
    pub organization: OrganizationTarget,
    pub teams: Vec<TeamTarget>,
    pub email_ref: EmailRef,
    pub approved_by: u64,
    pub approved_at: DateTime<Utc>,
}
impl EmailCreateCommand {
    pub fn delivery_key(&self) -> String {
        format!(
            "{}:{}:{}",
            self.batch.batch_id, self.batch.row, self.organization.org_id
        )
    }
    pub fn storage_id(&self) -> String {
        format!("email:{}", self.delivery_key())
    }
    pub fn validate(&self) -> Result<(), &'static str> {
        self.batch.validate()?;
        let positive = |id| id > 0 && id <= crate::storage::integer::MAX_EXACT;
        if ![self.account_id, self.installation_id, self.approved_by]
            .into_iter()
            .all(positive)
            || self.account_id != self.organization.org_id
        {
            return Err("invalid email delivery identity");
        }
        Targets {
            organization: Some(self.organization.clone()),
            teams: self.teams.clone(),
            repos: vec![],
        }
        .validate()
        .map_err(|_| "invalid email delivery targets")?;
        if self.teams.windows(2).any(|p| p[0].team_id >= p[1].team_id) {
            return Err("email delivery teams are not canonical");
        }
        Ok(())
    }
}
/// Full input binding for the independent non-idempotent creation step.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EmailAttempt {
    pub create: EmailCreateCommand,
}
impl EmailAttempt {
    pub const fn step(&self) -> &'static str {
        "org-email"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reference_is_strict_opaque_and_not_a_mailbox() {
        let reference = EmailRef::from_bytes([0xab; 32]);
        assert_eq!(
            serde_json::to_value(reference).unwrap(),
            serde_json::json!("ab".repeat(32))
        );
        assert_eq!(
            serde_json::from_value::<EmailRef>(serde_json::json!("ab".repeat(32))).unwrap(),
            reference
        );
        for value in [
            "AB".repeat(32),
            "x@y.com".into(),
            "a".repeat(63),
            "g".repeat(64),
        ] {
            assert!(serde_json::from_value::<EmailRef>(serde_json::json!(value)).is_err());
        }
        assert_eq!(format!("{reference:?}"), "EmailRef([REDACTED])");
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EmailBlockedReason {
    InstallationUnavailable,
    InstallationPermissions,
    TargetUnavailable,
    MintForbidden,
    IpAllowList,
    NoSeat,
    IdpSyncedTeam,
    ManagedEnterprise,
    EmailRecoveryUnavailable,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EmailCreateOutcome {
    Planned,
    Throttled {
        until: DateTime<Utc>,
        reason: Option<crate::budget::RetryReason>,
    },
    Blocked {
        reason: EmailBlockedReason,
    },
    OutcomeUnknown,
    Created {
        upstream_id: u64,
        created_at: DateTime<Utc>,
    },
    Failed {
        reason: crate::delivery::FailureReason,
    },
    Aborted {
        reason: crate::delivery::AbortReason,
    },
}
impl EmailCreateOutcome {
    /// Uncertainty is not permission to POST again, even when lists are empty.
    pub fn permits_new_attempt(&self) -> bool {
        matches!(
            self,
            Self::Planned | Self::Throttled { .. } | Self::Blocked { .. }
        )
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EmailSettlement {
    /// Complete reads cannot distinguish accepted from declined without a
    /// verified invitation-ID-bearing webhook. Never label this Accepted.
    NoLongerPending,
    /// Positive failed-list evidence; expiry semantics remain a live probe gate.
    FailedInvitation {
        failed_at: Option<DateTime<Utc>>,
    },
    Cancelled,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EmailDeliverySnapshot {
    pub command: EmailCreateCommand,
    pub revision: u64,
    pub outcome: EmailCreateOutcome,
    pub confirmed_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<DateTime<Utc>>,
    pub settlement: Option<EmailSettlement>,
    pub settled_at: Option<DateTime<Utc>>,
    /// First retained uncertain-create observation; absent legacy values remain unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unknown_since: Option<DateTime<Utc>>,
    /// Completed, identity-bound observation watermark, including still-pending reads.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_observed_at: Option<DateTime<Utc>>,
}
impl EmailDeliverySnapshot {
    /// Input must come from completed, verified pending+failed list reads. The
    /// current installation is supplied by the authoritative account owner; a
    /// reinstall may restore observation without changing the create command.
    pub fn reconcile(
        &mut self,
        observation: &crate::org_observation::OrganizationObservation,
        current_installation: u64,
        current_login: &str,
        now: DateTime<Utc>,
    ) -> Result<bool, &'static str> {
        self.command.validate()?;
        observation.validate()?;
        if self.settlement.is_some() {
            return Ok(false);
        }
        let EmailCreateOutcome::Created { upstream_id, .. } = self.outcome else {
            return Ok(false);
        };
        let Some(confirmed) = self.confirmed_at else {
            return Ok(false);
        };
        if observation.account_id != self.command.account_id
            || observation.org_id != self.command.organization.org_id
            || !observation.org_login.eq_ignore_ascii_case(current_login)
            || observation.installation_id != current_installation
            || observation.observation_started_at < confirmed
            || observation.observed_at > now
            || self
                .last_observed_at
                .is_some_and(|previous| observation.observation_started_at < previous)
        {
            return Ok(false);
        }
        // A completed pending read is operational evidence even when the
        // immutable create and settlement receipts do not change.
        let pending = observation
            .pending
            .iter()
            .any(|i| i.upstream_id == upstream_id);
        let settlement = if pending {
            None
        } else {
            Some(
                match observation
                    .failed
                    .iter()
                    .find(|i| i.invitation.upstream_id == upstream_id)
                {
                    Some(failed) => EmailSettlement::FailedInvitation {
                        failed_at: failed.failed_at,
                    },
                    None => EmailSettlement::NoLongerPending,
                },
            )
        };
        if settlement.is_none() && self.last_observed_at == Some(observation.observed_at) {
            return Ok(false);
        }
        let revision = self
            .revision
            .checked_add(1)
            .filter(|r| *r <= crate::storage::integer::MAX_EXACT)
            .ok_or("email delivery revision exhausted")?;
        self.last_observed_at = Some(observation.observed_at);
        if let Some(settlement) = settlement {
            self.settlement = Some(settlement);
            self.settled_at = Some(observation.observed_at);
        }
        self.revision = revision;
        Ok(true)
    }
}

#[cfg(test)]
mod settlement_tests {
    use super::*;
    use crate::{
        bulk::BulkBatchId,
        org_delivery::OrganizationInvitationRole,
        org_observation::{
            FailedOrganizationInvitation, ListedOrganizationInvitation, OrganizationObservation,
        },
        targets::MemberGrant,
    };
    fn snapshot() -> EmailDeliverySnapshot {
        let at = DateTime::from_timestamp(1_800_000_000, 0).unwrap();
        EmailDeliverySnapshot {
            command: EmailCreateCommand {
                batch: BatchRef {
                    batch_id: BulkBatchId::new(),
                    row: 1,
                },
                account_id: 42,
                installation_id: 9,
                organization: OrganizationTarget {
                    org_id: 42,
                    org_login: "acme".into(),
                    grant: MemberGrant::Member,
                },
                teams: vec![],
                email_ref: EmailRef::from_bytes([7; 32]),
                approved_by: 7,
                approved_at: at,
            },
            revision: 1,
            outcome: EmailCreateOutcome::Created {
                upstream_id: 123,
                created_at: at,
            },
            completed_at: None,
            confirmed_at: Some(at),
            settlement: None,
            settled_at: None,
            unknown_since: None,
            last_observed_at: None,
        }
    }
    fn observed(s: &EmailDeliverySnapshot) -> OrganizationObservation {
        let at = s.confirmed_at.unwrap() + chrono::Duration::seconds(1);
        OrganizationObservation {
            account_id: 42,
            installation_id: 10,
            org_id: 42,
            org_login: "acme".into(),
            observation_started_at: at,
            observed_at: at,
            pending: vec![],
            failed: vec![],
        }
    }
    fn invite(id: u64, at: DateTime<Utc>) -> ListedOrganizationInvitation {
        ListedOrganizationInvitation {
            upstream_id: id,
            login: Some("not-a-requester-binding".into()),
            role: OrganizationInvitationRole::DirectMember,
            created_at: at,
            team_count: 0,
        }
    }
    #[test]
    fn only_known_upstream_identity_settles_without_inventing_acceptance() {
        let mut s = snapshot();
        let mut o = observed(&s);
        let now = o.observed_at;
        o.pending.push(invite(123, now));
        assert!(s.reconcile(&o, 10, "acme", now).unwrap());
        assert_eq!(s.last_observed_at, Some(now));
        assert!(s.settlement.is_none());
        o.failed.push(FailedOrganizationInvitation {
            invitation: invite(123, now),
            failed_at: Some(now),
        });
        assert!(!s.reconcile(&o, 10, "acme", now).unwrap());
        o.pending.clear();
        assert!(s.reconcile(&o, 10, "acme", now).unwrap());
        assert_eq!(
            s.settlement,
            Some(EmailSettlement::FailedInvitation {
                failed_at: Some(now)
            })
        );
        let retained = s.clone();
        o.failed.clear();
        assert!(!s.reconcile(&o, 10, "acme", now).unwrap());
        assert_eq!(s, retained);
        let mut absent = snapshot();
        let mut o = observed(&absent);
        o.pending.push(invite(999, now));
        assert!(absent.reconcile(&o, 10, "acme", now).unwrap());
        assert_eq!(absent.settlement, Some(EmailSettlement::NoLongerPending));
        assert!(matches!(
            absent.outcome,
            EmailCreateOutcome::Created {
                upstream_id: 123,
                ..
            }
        ));
    }
    #[test]
    fn unknown_stale_future_wrong_identity_never_authorizes_new_write_or_settlement() {
        let s = snapshot();
        let o = observed(&s);
        let now = o.observed_at;
        for n in 0..5 {
            let mut s = s.clone();
            let mut o = o.clone();
            match n {
                0 => s.outcome = EmailCreateOutcome::OutcomeUnknown,
                1 => {
                    o.observation_started_at = s.command.approved_at - chrono::Duration::seconds(1)
                }
                2 => o.observed_at = now + chrono::Duration::seconds(1),
                3 => o.org_login = "other".into(),
                _ => o.installation_id = 11,
            };
            let before = s.clone();
            assert!(!s.reconcile(&o, 10, "acme", now).unwrap());
            assert_eq!(s, before);
            assert!(!s.outcome.permits_new_attempt());
        }
    }
    #[test]
    fn command_binding_rejects_unsupported_identity_and_revision_overflow_is_atomic() {
        let mut s = snapshot();
        s.command.batch.row = 0;
        assert!(s.command.validate().is_err());
        let mut s = snapshot();
        s.command.account_id = 43;
        assert!(s.command.validate().is_err());
        let mut s = snapshot();
        s.revision = crate::storage::integer::MAX_EXACT;
        let o = observed(&s);
        let before = s.clone();
        assert!(s.reconcile(&o, 10, "acme", o.observed_at).is_err());
        assert_eq!(s, before);
        let attempt = EmailAttempt {
            create: s.command.clone(),
        };
        assert_eq!(attempt.step(), "org-email");
        assert!(s.command.storage_id().starts_with("email:"));
        let wire = serde_json::to_value(&s.command).unwrap();
        assert!(wire.get("requester_id").is_none());
        assert!(wire.get("email").is_none());
        assert!(wire.get("access_lifetime_seconds").is_none());
    }
}

impl EmailDeliverySnapshot {
    pub fn validate(&self) -> Result<(), &'static str> {
        self.command.validate()?;
        if self.revision == 0 || self.revision > crate::storage::integer::MAX_EXACT {
            return Err("invalid email delivery revision");
        }
        let created = matches!(self.outcome, EmailCreateOutcome::Created { .. });
        if self
            .unknown_since
            .into_iter()
            .chain(self.last_observed_at)
            .any(|at| at < self.command.approved_at)
            || self
                .last_observed_at
                .zip(self.confirmed_at)
                .is_some_and(|(observed, confirmed)| observed < confirmed)
        {
            return Err("invalid email observation clock");
        }
        if self
            .completed_at
            .is_some_and(|t| t < self.command.approved_at)
            || (self.completed_at.is_some()
                && !matches!(
                    self.outcome,
                    EmailCreateOutcome::Failed { .. } | EmailCreateOutcome::Aborted { .. }
                ))
            || created != self.confirmed_at.is_some()
            || self.settlement.is_some() != self.settled_at.is_some()
            || (self.settlement.is_some() && !created)
            || self
                .confirmed_at
                .is_some_and(|t| t < self.command.approved_at)
            || self
                .settled_at
                .zip(self.confirmed_at)
                .is_some_and(|(s, c)| s < c)
        {
            return Err("invalid email delivery receipt");
        }
        if let EmailCreateOutcome::Created { upstream_id, .. } = self.outcome
            && (upstream_id == 0 || upstream_id > crate::storage::integer::MAX_EXACT)
        {
            return Err("invalid email invitation identity");
        }
        Ok(())
    }
    /// False is an older immutable replay; equal revisions must be identical.
    pub fn advances(&self, previous: &Self) -> Result<bool, &'static str> {
        self.validate()?;
        previous.validate()?;
        if self.command != previous.command {
            return Err("email delivery command conflict");
        }
        if self.revision < previous.revision {
            return Ok(false);
        }
        if self.revision == previous.revision {
            return if self == previous {
                Ok(false)
            } else {
                Err("email delivery revision conflict")
            };
        }
        if previous.unknown_since.is_some() && self.unknown_since != previous.unknown_since
            || previous
                .last_observed_at
                .is_some_and(|old| self.last_observed_at.is_none_or(|new| new < old))
        {
            return Err("email observation clock cannot regress");
        }
        if previous.settlement.is_some()
            && (self.settlement != previous.settlement || self.settled_at != previous.settled_at)
            || matches!(
                previous.outcome,
                EmailCreateOutcome::Created { .. }
                    | EmailCreateOutcome::Failed { .. }
                    | EmailCreateOutcome::Aborted { .. }
            ) && (self.outcome != previous.outcome
                || self.confirmed_at != previous.confirmed_at
                || self.completed_at != previous.completed_at)
            || matches!(previous.outcome, EmailCreateOutcome::OutcomeUnknown)
                && self.outcome.permits_new_attempt()
        {
            return Err("email delivery receipt cannot regress");
        }
        Ok(true)
    }
}

#[cfg(test)]
mod telemetry_tests {
    use super::*;

    #[test]
    fn legacy_missing_clocks_remain_absent_and_temporal_updates_cannot_regress() {
        let at = DateTime::from_timestamp(1_800_000_000, 0).unwrap();
        let legacy = serde_json::json!({
            "command": {"batch":{"batch_id":crate::bulk::BulkBatchId::new(),"row":1},
                "account_id":42,"installation_id":9,"organization":{"org_id":42,"org_login":"before","grant":"member"},
                "teams":[],"email_ref":"ab".repeat(32),"approved_by":7,"approved_at":at},
            "revision":1,"outcome":{"kind":"outcome_unknown"},"confirmed_at":null,
            "settlement":null,"settled_at":null
        });
        let old: EmailDeliverySnapshot = serde_json::from_value(legacy).unwrap();
        assert_eq!(old.unknown_since, None);
        assert_eq!(old.last_observed_at, None);
        let mut current = old.clone();
        current.revision += 1;
        current.unknown_since = Some(at);
        assert!(current.advances(&old).unwrap());
        for changed in [None, Some(at + chrono::Duration::seconds(1))] {
            let mut next = current.clone();
            next.revision += 1;
            next.unknown_since = changed;
            assert!(next.advances(&current).is_err());
        }
        current.last_observed_at = Some(at + chrono::Duration::seconds(2));
        for changed in [None, Some(at + chrono::Duration::seconds(1))] {
            let mut next = current.clone();
            next.revision += 1;
            next.last_observed_at = changed;
            assert!(next.advances(&current).is_err());
        }
        let mut invalid = old;
        invalid.unknown_since = Some(at - chrono::Duration::seconds(1));
        assert!(invalid.validate().is_err());
    }
}
