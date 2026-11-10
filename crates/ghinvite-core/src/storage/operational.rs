//! Delivery telemetry is a monotonic projection, independent of retained facts.
use super::{Error, Result};
use crate::{InvitationLinkId, RequestId};
use chrono::{DateTime, Duration, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RepositoryTelemetry {
    pub request_id: RequestId,
    pub target_id: u64,
    pub delivery_generation: u64,
    pub account_id: u64,
    pub unknown_since: Option<DateTime<Utc>>,
    pub last_observed_at: Option<DateTime<Utc>>,
}
impl RepositoryTelemetry {
    pub fn key(&self) -> String {
        crate::delivery::delivery_key(self.request_id, self.target_id, self.delivery_generation)
    }
}
pub fn encode(value: &RepositoryTelemetry) -> Result<String> {
    for id in [value.target_id, value.delivery_generation, value.account_id] {
        if id == 0 || id > super::integer::MAX_EXACT {
            return Err(Error::ProjectionInvariant(
                "invalid telemetry identity".into(),
            ));
        }
    }
    let time = |date: DateTime<Utc>| date.to_rfc3339_opts(SecondsFormat::Nanos, true);
    serde_json::to_string(&serde_json::json!({
        "key":value.key(), "request_id":value.request_id, "target_id":value.target_id,
        "delivery_generation":value.delivery_generation,"account_id":value.account_id,
        "unknown_since":value.unknown_since.map(time),"last_observed_at":value.last_observed_at.map(time)
    })).map_err(|_| Error::Corrupt("invalid telemetry encoding".into()))
}

/// UTC text stored by both adapters is normalized to fixed nanoseconds before
/// comparison. SQLite julianday would lose the boundary's submillisecond part.
fn normalized(column: &str) -> String {
    format!(
        "(substr({column},1,19) || '.' || substr(CASE WHEN substr({column},20,1) = '.' THEN replace(replace(substr({column},21),'+00:00',''),'Z','') ELSE '' END || '000000000',1,9) || 'Z')"
    )
}
pub fn statements() -> Vec<String> {
    let created = normalized("d.created_at");
    vec![
        "INSERT INTO projection_assertions(dependency) SELECT EXISTS(SELECT 1 FROM deliveries WHERE id=json_extract(?1,'$.key'))".into(),
        format!(r#"INSERT INTO projection_assertions(invariant) SELECT EXISTS(
            SELECT 1 FROM deliveries d JOIN invitation_requests r ON r.id=d.request_id
            LEFT JOIN invitation_links l ON l.id=r.invitation_link_id
            LEFT JOIN bulk_batches b ON b.id=r.batch_id
            WHERE d.id=json_extract(?1,'$.key') AND d.kind='repo'
            AND d.request_id=json_extract(?1,'$.request_id') AND d.target_id=json_extract(?1,'$.target_id')
            AND d.delivery_generation=json_extract(?1,'$.delivery_generation') AND COALESCE(l.account_id,b.account_id)=json_extract(?1,'$.account_id')
            AND (json_extract(?1,'$.unknown_since') IS NULL OR json_extract(?1,'$.unknown_since')>={created})
            AND (json_extract(?1,'$.last_observed_at') IS NULL OR json_extract(?1,'$.last_observed_at')>={created})
            AND (d.unknown_since IS NULL OR json_extract(?1,'$.unknown_since') IS NULL OR d.unknown_since=json_extract(?1,'$.unknown_since'))
        )"#),
        r#"UPDATE deliveries SET unknown_since=COALESCE(unknown_since,json_extract(?1,'$.unknown_since')),
        last_observed_at=CASE WHEN json_extract(?1,'$.last_observed_at') IS NOT NULL AND (last_observed_at IS NULL OR json_extract(?1,'$.last_observed_at')>last_observed_at)
        THEN json_extract(?1,'$.last_observed_at') ELSE last_observed_at END WHERE id=json_extract(?1,'$.key')"#.into(),
        "DELETE FROM projection_assertions WHERE ?1 IS NOT NULL".into(),
    ]
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgeSignal {
    pub total: u64,
    pub over_threshold: u64,
    pub oldest_age_seconds: Option<i64>,
    pub missing_timestamp: u64,
    pub future_timestamp: u64,
}
impl AgeSignal {
    pub fn incomplete(&self) -> bool {
        self.missing_timestamp != 0 || self.future_timestamp != 0
    }
    fn observe(&mut self, timestamp: Option<DateTime<Utc>>, now: DateTime<Utc>, threshold: i64) {
        self.total += 1;
        let Some(timestamp) = timestamp else {
            self.missing_timestamp += 1;
            return;
        };
        let age = now.signed_duration_since(timestamp);
        if age < Duration::zero() {
            self.future_timestamp += 1;
            return;
        }
        if age > Duration::seconds(threshold) {
            self.over_threshold += 1;
        }
        self.oldest_age_seconds = Some(self.oldest_age_seconds.unwrap_or(0).max(age.num_seconds()));
    }
}
/// Maximum complete report size. The extra SQL row detects overflow; no tail is hidden.
pub const MAX_REPORT_STEPS: usize = 10_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignalKind {
    Repo,
    Org,
    Team,
    Email,
}
impl SignalKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Repo => "Repository",
            Self::Org => "Organization",
            Self::Team => "Team",
            Self::Email => "Email invitation",
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationalBlockedReason {
    InstallationUnavailable,
    InstallationPermissions,
    RequesterIdentity,
    OutsideCollaboratorsRestricted,
    IpAllowList,
    TargetUnavailable,
    MintForbidden,
    NoSeat,
    IdpSyncedTeam,
    ManagedEnterprise,
    EmailRecoveryUnavailable,
}
impl OperationalBlockedReason {
    pub fn parse(kind: SignalKind, value: &str) -> Result<Self> {
        let value = serde_json::Value::String(value.into());
        let valid = match kind {
            SignalKind::Repo => {
                serde_json::from_value::<crate::delivery::BlockedReason>(value.clone()).is_ok()
            }
            SignalKind::Org | SignalKind::Team => serde_json::from_value::<
                crate::org_delivery::OrganizationBlockedReason,
            >(value.clone())
            .is_ok(),
            SignalKind::Email => {
                serde_json::from_value::<crate::email_delivery::EmailBlockedReason>(value.clone())
                    .is_ok()
            }
        };
        if !valid {
            return Err(Error::Corrupt("invalid blocked reason".into()));
        }
        serde_json::from_value(value).map_err(|_| Error::Corrupt("invalid blocked reason".into()))
    }
    pub fn description(self) -> &'static str {
        match self {
            Self::InstallationUnavailable => {
                "App installation unavailable — check installation and suspension"
            }
            Self::InstallationPermissions => "App permissions need review",
            Self::RequesterIdentity => "Requester identity needs verification",
            Self::OutsideCollaboratorsRestricted => {
                "Organization policy restricts outside collaborators"
            }
            Self::IpAllowList => "Organization IP allow list needs review",
            Self::TargetUnavailable => "Target unavailable or outside app installation",
            Self::MintForbidden => "Installation credential could not be issued",
            Self::NoSeat => "No organization seat available",
            Self::IdpSyncedTeam => "Team membership is managed by an identity provider",
            Self::ManagedEnterprise => {
                "Enterprise-managed membership requires administrator action"
            }
            Self::EmailRecoveryUnavailable => "Protected email recovery is unavailable",
        }
    }
}
impl From<crate::delivery::BlockedReason> for OperationalBlockedReason {
    fn from(reason: crate::delivery::BlockedReason) -> Self {
        use crate::delivery::BlockedReason as R;
        match reason {
            R::InstallationUnavailable => Self::InstallationUnavailable,
            R::InstallationPermissions => Self::InstallationPermissions,
            R::RequesterIdentity => Self::RequesterIdentity,
            R::OutsideCollaboratorsRestricted => Self::OutsideCollaboratorsRestricted,
            R::IpAllowList => Self::IpAllowList,
            R::TargetUnavailable => Self::TargetUnavailable,
            R::MintForbidden => Self::MintForbidden,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockedCount {
    pub kind: SignalKind,
    pub reason: OperationalBlockedReason,
    pub count: u64,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignalCounts {
    pub repository_deliveries: u64,
    pub organization_deliveries: u64,
    pub team_steps: u64,
    pub email_deliveries: u64,
    pub unknown: AgeSignal,
    pub pending: AgeSignal,
    pub blocked: Vec<BlockedCount>,
}
impl SignalCounts {
    pub fn incomplete(&self) -> bool {
        self.unknown.incomplete() || self.pending.incomplete()
    }
    pub fn total_steps(&self) -> u64 {
        self.repository_deliveries
            + self.organization_deliveries
            + self.team_steps
            + self.email_deliveries
    }
    fn observe(&mut self, row: &SignalRow, now: DateTime<Utc>) -> Result<()> {
        match row.kind {
            SignalKind::Repo => self.repository_deliveries += 1,
            SignalKind::Org => self.organization_deliveries += 1,
            SignalKind::Team => self.team_steps += 1,
            SignalKind::Email => self.email_deliveries += 1,
        }
        if row.tracking_stopped_reason.is_some()
            || row.settlement.as_deref().is_some_and(|s| s != "sent")
        {
            return Ok(());
        }
        match row.create_outcome.as_str() {
            "outcome_unknown" => self.unknown.observe(row.unknown_since, now, 3600),
            "created" | "already_pending" => {
                self.pending
                    .observe(row.last_observed_at.or(row.sent_at), now, 7200)
            }
            "blocked" => {
                let reason = OperationalBlockedReason::parse(
                    row.kind,
                    row.outcome_reason
                        .as_deref()
                        .ok_or_else(|| Error::Corrupt("missing blocked reason".into()))?,
                )?;
                if let Some(count) = self
                    .blocked
                    .iter_mut()
                    .find(|c| c.kind == row.kind && c.reason == reason)
                {
                    count.count += 1;
                } else {
                    self.blocked.push(BlockedCount {
                        kind: row.kind,
                        reason,
                        count: 1,
                    });
                }
            }
            _ => {}
        }
        Ok(())
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountSignals {
    pub account_id: u64,
    #[serde(flatten)]
    pub counts: SignalCounts,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeliverySignals {
    pub observed_at: DateTime<Utc>,
    #[serde(flatten)]
    pub counts: SignalCounts,
    pub by_account: Vec<AccountSignals>,
    /// Each identity names one independent delivery step; never inferred from counts.
    pub steps: Vec<SignalRow>,
}
impl DeliverySignals {
    pub fn incomplete(&self) -> bool {
        self.counts.incomplete()
    }
}

pub const SIGNALS: &str =
    "SELECT * FROM operational_delivery_steps ORDER BY account_id,id,step LIMIT 10001";
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignalRow {
    pub id: String,
    pub request_id: Option<RequestId>,
    pub link_id: Option<InvitationLinkId>,
    pub batch_id: Option<crate::bulk::BulkBatchId>,
    pub batch_row: Option<u16>,
    pub kind: SignalKind,
    pub step: String,
    pub target_id: u64,
    pub delivery_generation: u64,
    pub account_id: Option<u64>,
    pub create_outcome: String,
    pub outcome_reason: Option<String>,
    pub settlement: Option<String>,
    pub tracking_stopped_reason: Option<String>,
    pub sent_at: Option<DateTime<Utc>>,
    pub unknown_since: Option<DateTime<Utc>>,
    pub last_observed_at: Option<DateTime<Utc>>,
}
pub fn report(rows: Vec<SignalRow>, now: DateTime<Utc>) -> Result<DeliverySignals> {
    if rows.len() > MAX_REPORT_STEPS {
        return Err(Error::Database(
            "delivery report exceeds complete step limit".into(),
        ));
    }
    let mut counts = SignalCounts::default();
    let mut accounts = BTreeMap::<u64, SignalCounts>::new();
    for row in &rows {
        let account = row
            .account_id
            .filter(|id| *id > 0 && *id <= super::integer::MAX_EXACT)
            .ok_or_else(|| Error::Corrupt("delivery account missing".into()))?;
        let batch = match (row.batch_id, row.batch_row) {
            (Some(batch_id), Some(row)) => Some(crate::bulk::BatchRef { batch_id, row }),
            (None, None) => None,
            _ => return Err(Error::Corrupt("invalid delivery batch origin".into())),
        };
        if batch.is_some_and(|b| b.validate().is_err())
            || row.link_id.is_some() == batch.is_some()
            || row.target_id == 0
            || row.target_id > super::integer::MAX_EXACT
            || row.delivery_generation == 0
            || row.delivery_generation > super::integer::MAX_EXACT
        {
            return Err(Error::Corrupt("invalid delivery identity".into()));
        }
        if row.kind == SignalKind::Email {
            let source = batch.ok_or_else(|| Error::Corrupt("email batch missing".into()))?;
            if row.request_id.is_some()
                || row.target_id != account
                || row.delivery_generation != 1
                || row.id != format!("email:{}:{}:{account}", source.batch_id, source.row)
                || row.step != "org-email"
            {
                return Err(Error::Corrupt("invalid email delivery identity".into()));
            }
            counts.observe(row, now)?;
            accounts.entry(account).or_default().observe(row, now)?;
            continue;
        }
        let key = crate::delivery::delivery_key(
            row.request_id
                .ok_or_else(|| Error::Corrupt("delivery request missing".into()))?,
            if row.kind == SignalKind::Team {
                account
            } else {
                row.target_id
            },
            row.delivery_generation,
        );
        let (expected, step) = match row.kind {
            SignalKind::Repo => (key, "repo".into()),
            SignalKind::Org => (format!("org:{key}"), "org".into()),
            SignalKind::Team => (format!("org:{key}"), format!("team:{}", row.target_id)),
            SignalKind::Email => unreachable!("email identities validated above"),
        };
        if row.id != expected
            || row.step != step
            || (row.kind == SignalKind::Org && row.target_id != account)
        {
            return Err(Error::Corrupt("invalid delivery step key".into()));
        }
        counts.observe(row, now)?;
        accounts.entry(account).or_default().observe(row, now)?;
    }
    Ok(DeliverySignals {
        observed_at: now,
        counts,
        by_account: accounts
            .into_iter()
            .map(|(account_id, counts)| AccountSignals { account_id, counts })
            .collect(),
        steps: rows,
    })
}
