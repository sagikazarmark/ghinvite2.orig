//! Daily record of the hourly sweep watchdog (#86). Each complete check
//! upserts its UTC day: the day keeps its oldest overdue chain, its most recent
//! failed sweep and running counts. Observations only; nothing here authorizes
//! a schedule or delivery change.
use super::{Error, Result};
use crate::{account::AccountInstallationRef, sweep_health::WatchdogReport};
use chrono::{DateTime, NaiveDate, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SweepDigest {
    pub day: NaiveDate,
    pub first_observed_at: DateTime<Utc>,
    pub last_observed_at: DateTime<Utc>,
    /// Complete watchdog checks recorded this day.
    pub checks: u64,
    /// Most schedules one check read.
    pub schedules: u64,
    /// Chains re-armed (missing or overdue), summed over the day's checks.
    pub rearmed: u64,
    /// Most schedules one check could not read.
    pub unavailable: u64,
    pub oldest_overdue: Option<(AccountInstallationRef, DateTime<Utc>)>,
    /// Most schedules with a failed sweep in the previous day, in one check.
    pub failed_schedules: u64,
    pub latest_failure: Option<(AccountInstallationRef, DateTime<Utc>)>,
}

/// Validated values for ordinary positional SQL bindings (ADR 0005).
pub struct RecordParameters {
    pub day: String,
    pub observed_at: String,
    pub schedules: u64,
    pub rearmed: u64,
    pub unavailable: u64,
    pub oldest_overdue_schedule: Option<String>,
    pub oldest_overdue_due_at: Option<String>,
    pub failed_schedules: u64,
    pub latest_failed_schedule: Option<String>,
    pub latest_failed_at: Option<String>,
}

fn timestamp(at: DateTime<Utc>) -> String {
    // Fixed-width nanosecond UTC text: the upsert compares it as text.
    at.to_rfc3339_opts(SecondsFormat::Nanos, true)
}
fn key(account: AccountInstallationRef) -> Result<String> {
    account
        .key()
        .map_err(|_| Error::ProjectionInvariant("invalid sweep digest schedule".into()))
}
fn count(value: u64) -> Result<u64> {
    super::integer::validate(&[value])?;
    Ok(value)
}

pub fn record_parameters(now: DateTime<Utc>, report: &WatchdogReport) -> Result<RecordParameters> {
    super::blocked_digest::validate_day(now.date_naive())?;
    let oldest = report
        .oldest_overdue
        .as_ref()
        .map(|o| Ok::<_, Error>((key(o.account)?, timestamp(o.due_at))))
        .transpose()?;
    let latest = report
        .latest_failure()
        .map(|f| Ok::<_, Error>((key(f.account)?, timestamp(f.last_failed_at))))
        .transpose()?;
    Ok(RecordParameters {
        day: now.date_naive().to_string(),
        observed_at: timestamp(now),
        schedules: count(report.checked)?,
        rearmed: count(
            report
                .missing
                .checked_add(report.overdue)
                .ok_or_else(|| Error::ProjectionInvariant("sweep digest overflow".into()))?,
        )?,
        unavailable: count(report.unavailable)?,
        oldest_overdue_schedule: oldest.as_ref().map(|(k, _)| k.clone()),
        oldest_overdue_due_at: oldest.map(|(_, at)| at),
        failed_schedules: count(report.failed.len() as u64)?,
        latest_failed_schedule: latest.as_ref().map(|(k, _)| k.clone()),
        latest_failed_at: latest.map(|(_, at)| at),
    })
}

// A check older than the newest recorded day cannot backfill the window.
// SQLite evaluates every SET expression against the row before the update.
pub const RECORD: &str = "INSERT INTO sweep_digest_days(day,first_observed_at,last_observed_at,checks,schedules,rearmed,unavailable,oldest_overdue_schedule,oldest_overdue_due_at,failed_schedules,latest_failed_schedule,latest_failed_at) SELECT ?1,?2,?2,1,?3,?4,?5,?6,?7,?8,?9,?10 WHERE ?1>=COALESCE((SELECT MAX(day) FROM sweep_digest_days),'') ON CONFLICT(day) DO UPDATE SET last_observed_at=MAX(last_observed_at,excluded.last_observed_at),checks=checks+1,schedules=MAX(schedules,excluded.schedules),rearmed=rearmed+excluded.rearmed,unavailable=MAX(unavailable,excluded.unavailable),oldest_overdue_schedule=CASE WHEN excluded.oldest_overdue_due_at IS NOT NULL AND (oldest_overdue_due_at IS NULL OR excluded.oldest_overdue_due_at<oldest_overdue_due_at) THEN excluded.oldest_overdue_schedule ELSE oldest_overdue_schedule END,oldest_overdue_due_at=CASE WHEN excluded.oldest_overdue_due_at IS NOT NULL AND (oldest_overdue_due_at IS NULL OR excluded.oldest_overdue_due_at<oldest_overdue_due_at) THEN excluded.oldest_overdue_due_at ELSE oldest_overdue_due_at END,failed_schedules=MAX(failed_schedules,excluded.failed_schedules),latest_failed_schedule=CASE WHEN excluded.latest_failed_at IS NOT NULL AND (latest_failed_at IS NULL OR excluded.latest_failed_at>latest_failed_at) THEN excluded.latest_failed_schedule ELSE latest_failed_schedule END,latest_failed_at=CASE WHEN excluded.latest_failed_at IS NOT NULL AND (latest_failed_at IS NULL OR excluded.latest_failed_at>latest_failed_at) THEN excluded.latest_failed_at ELSE latest_failed_at END";
pub const CLEANUP: &str = "DELETE FROM sweep_digest_days WHERE day<date((SELECT MAX(day) FROM sweep_digest_days),'-6 days')";
pub const READ: &str = "SELECT day,first_observed_at,last_observed_at,checks,schedules,rearmed,unavailable,oldest_overdue_schedule,oldest_overdue_due_at,failed_schedules,latest_failed_schedule,latest_failed_at FROM sweep_digest_days WHERE day=?1";

#[derive(Deserialize)]
pub struct Row {
    day: NaiveDate,
    first_observed_at: DateTime<Utc>,
    last_observed_at: DateTime<Utc>,
    checks: u64,
    schedules: u64,
    rearmed: u64,
    unavailable: u64,
    oldest_overdue_schedule: Option<String>,
    oldest_overdue_due_at: Option<DateTime<Utc>>,
    failed_schedules: u64,
    latest_failed_schedule: Option<String>,
    latest_failed_at: Option<DateTime<Utc>>,
}
fn pair(
    schedule: Option<String>,
    at: Option<DateTime<Utc>>,
) -> Result<Option<(AccountInstallationRef, DateTime<Utc>)>> {
    match (schedule, at) {
        (None, None) => Ok(None),
        (Some(schedule), Some(at)) => Ok(Some((
            schedule
                .parse()
                .map_err(|_| Error::Corrupt("invalid sweep digest schedule".into()))?,
            at,
        ))),
        _ => Err(Error::Corrupt("incomplete sweep digest schedule".into())),
    }
}
pub fn decode(row: Option<Row>) -> Result<Option<SweepDigest>> {
    let Some(row) = row else {
        return Ok(None);
    };
    super::blocked_digest::validate_day(row.day)?;
    if row.first_observed_at.date_naive() != row.day
        || row.last_observed_at.date_naive() != row.day
        || row.last_observed_at < row.first_observed_at
        || row.checks == 0
    {
        return Err(Error::Corrupt("invalid sweep digest day".into()));
    }
    Ok(Some(SweepDigest {
        day: row.day,
        first_observed_at: row.first_observed_at,
        last_observed_at: row.last_observed_at,
        checks: row.checks,
        schedules: row.schedules,
        rearmed: row.rearmed,
        unavailable: row.unavailable,
        oldest_overdue: pair(row.oldest_overdue_schedule, row.oldest_overdue_due_at)?,
        failed_schedules: row.failed_schedules,
        latest_failure: pair(row.latest_failed_schedule, row.latest_failed_at)?,
    }))
}
