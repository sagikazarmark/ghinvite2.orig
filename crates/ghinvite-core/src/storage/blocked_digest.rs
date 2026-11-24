//! Immutable daily observations of projected blocked deliveries. These records
//! never authorize writes or change delivery receipts. Visibility is applied
//! before aggregation using the same all-target scope as the Console.
use super::{
    Error, Result,
    operational::{BlockedCount, OperationalBlockedReason, SignalKind},
};
use async_trait::async_trait;
use chrono::{DateTime, Datelike, NaiveDate, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockedDigest {
    pub day: NaiveDate,
    pub observed_at: DateTime<Utc>,
    pub counts: Vec<BlockedCount>,
}
impl BlockedDigest {
    pub fn stale_for(&self, requested_day: NaiveDate) -> bool {
        self.day < requested_day
    }
}
#[async_trait]
pub trait DailyDigestStorage: Send + Sync + 'static {
    /// The first successful snapshot in a UTC day wins. Older calls cannot
    /// backfill or regress the retained seven-day window. No external effects.
    async fn record_blocked_digest(&self, now: DateTime<Utc>) -> Result<()>;
    /// Fold one complete sweep watchdog check into its UTC day (#86). A check
    /// dated before the newest recorded day is ignored; seven days are kept.
    async fn record_sweep_health(
        &self,
        now: DateTime<Utc>,
        report: &crate::sweep_health::WatchdogReport,
    ) -> Result<()>;
    async fn sweep_digest(
        &self,
        day: NaiveDate,
    ) -> Result<Option<super::sweep_digest::SweepDigest>>;
}
/// Validated values for ordinary positional SQL bindings (ADR 0005).
pub struct RecordParameters {
    pub day: String,
    pub observed_at: String,
}
pub fn record_parameters(now: DateTime<Utc>) -> Result<RecordParameters> {
    validate_day(now.date_naive())?;
    Ok(RecordParameters {
        day: now.date_naive().to_string(),
        observed_at: now.to_rfc3339_opts(SecondsFormat::Nanos, true),
    })
}
pub fn validate_day(day: NaiveDate) -> Result<()> {
    if !(1970..=9999).contains(&day.year()) {
        return Err(Error::ProjectionInvariant("invalid digest day".into()));
    }
    Ok(())
}
// The uncommitted complete=0 header is the insertion marker. Every adapter
// executes the whole sequence atomically; a retry sees only complete=1 and
// cannot add items even when projected facts have changed in the meantime.
pub const RECORD_HEADER: &str = "INSERT INTO blocked_digest_days(day,observed_at) SELECT ?1,?2 WHERE ?1>=COALESCE((SELECT MAX(day) FROM blocked_digest_days),'') ON CONFLICT(day) DO NOTHING";
pub const RECORD_ITEMS: &str = "INSERT INTO blocked_digest_items(day,delivery_key,step,kind,link_id,batch_id,batch_row,account_id,reason) SELECT h.day,d.id,d.step,d.kind,d.link_id,d.batch_id,d.batch_row,d.account_id,d.outcome_reason FROM blocked_digest_days h JOIN operational_delivery_steps d WHERE h.day=?1 AND h.complete=0 AND d.create_outcome='blocked' AND (d.settlement IS NULL OR d.settlement='sent') AND d.tracking_stopped_reason IS NULL ORDER BY d.id,d.step LIMIT 10001";
// Overflow aborts the entire snapshot, including its header.
pub const RECORD_BOUND: &str = "INSERT INTO projection_assertions(invariant) SELECT COUNT(*)<=10000 FROM blocked_digest_items WHERE day=?1";
pub const CLEAR_ASSERTIONS: &str = "DELETE FROM projection_assertions WHERE ?1 IS NOT NULL";
pub const RECORD_COMPLETE: &str = "UPDATE blocked_digest_days SET complete=1 WHERE day=?1";
pub const CLEANUP: &str = "DELETE FROM blocked_digest_days WHERE day<date((SELECT MAX(day) FROM blocked_digest_days),'-6 days')";
// One statement yields header + filtered counts consistently, including a
// healthy empty snapshot. The global header contains no counts or identities.
pub const SCOPED: &str = r#"WITH header AS (
    SELECT day,observed_at FROM blocked_digest_days WHERE complete=1 AND day<=?1 ORDER BY day DESC LIMIT 1
), counts AS (
    SELECT i.day,i.kind,i.reason,COUNT(*) AS count FROM blocked_digest_items i JOIN header h ON h.day=i.day
    WHERE i.account_id=?2 AND (?3 IS NULL OR EXISTS(
        SELECT 1 FROM invitation_links l WHERE l.id=i.link_id AND l.account_id=?2
        AND EXISTS(SELECT 1 FROM invitation_link_targets t WHERE t.link_id=l.id AND t.kind='repo')
        AND NOT EXISTS(SELECT 1 FROM invitation_link_targets t WHERE t.link_id=l.id
            AND (t.kind!='repo' OR t.target_id NOT IN (SELECT value FROM json_each(?3))))
    ) OR EXISTS(
        SELECT 1 FROM bulk_batches b WHERE b.id=i.batch_id AND b.account_id=?2
        AND i.kind='repo'
        AND json_extract(b.manifest,'$.targets.organization') IS NULL
        AND json_array_length(b.manifest,'$.targets.teams')=0
        AND json_array_length(b.manifest,'$.targets.repos')>0
        AND NOT EXISTS(SELECT 1 FROM json_each(b.manifest,'$.targets.repos') t
          WHERE json_extract(t.value,'$.repo_id') NOT IN (SELECT value FROM json_each(?3)))
    )) GROUP BY i.day,i.kind,i.reason
) SELECT h.day,h.observed_at,c.kind,c.reason,c.count FROM header h LEFT JOIN counts c ON c.day=h.day ORDER BY c.kind,c.reason"#;
#[derive(Deserialize)]
pub struct Row {
    day: NaiveDate,
    observed_at: DateTime<Utc>,
    kind: Option<SignalKind>,
    reason: Option<String>,
    count: Option<u64>,
}
pub fn decode(rows: Vec<Row>) -> Result<Option<BlockedDigest>> {
    let mut result: Option<BlockedDigest> = None;
    for row in rows {
        validate_day(row.day)?;
        if row.observed_at.date_naive() != row.day {
            return Err(Error::Corrupt("invalid digest timestamp".into()));
        }
        let digest = result.get_or_insert_with(|| BlockedDigest {
            day: row.day,
            observed_at: row.observed_at,
            counts: vec![],
        });
        if digest.day != row.day || digest.observed_at != row.observed_at {
            return Err(Error::Corrupt("inconsistent digest header".into()));
        }
        match (row.kind, row.reason, row.count) {
            (Some(kind), Some(reason), Some(count)) if count > 0 => {
                digest.counts.push(BlockedCount {
                    kind,
                    reason: OperationalBlockedReason::parse(kind, &reason)?,
                    count,
                })
            }
            (None, None, None) => {}
            _ => return Err(Error::Corrupt("invalid digest count".into())),
        }
    }
    Ok(result)
}
