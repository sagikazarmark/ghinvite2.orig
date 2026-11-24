//! Watchdog wire between the web's hourly cron and `SweepWatchdog` (#86).
//! A report describes retained schedule state at one check; it never
//! authorizes a delivery transition or a GitHub effect.
use crate::account::AccountInstallationRef;
use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Accounts one watchdog check may inspect. The web splits its account set
/// into batches of at most this size, one ingress call each.
pub const MAX_WATCHDOG_ACCOUNTS: usize = 100;

/// A failed sweep counts in reports for this long after it happened.
pub const FAILURE_WINDOW_SECONDS: i64 = 24 * 60 * 60;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WatchdogInput {
    pub accounts: Vec<AccountInstallationRef>,
}

/// A chain whose due run had not started by the check.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OverdueSchedule {
    pub account: AccountInstallationRef,
    pub due_at: DateTime<Utc>,
}

/// A chain with at least one failed sweep inside [`FAILURE_WINDOW_SECONDS`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FailedSchedule {
    pub account: AccountInstallationRef,
    pub failed_runs: u64,
    pub last_failed_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WatchdogReport {
    /// Schedules whose retained state was read.
    pub checked: u64,
    /// Never armed; re-armed by this check.
    pub missing: u64,
    /// Due and not started within the grace period; re-armed by this check.
    pub overdue: u64,
    /// Schedules the workflow endpoint refused to read (for example an
    /// organization account while organization sweeps are disabled).
    pub unavailable: u64,
    pub oldest_overdue: Option<OverdueSchedule>,
    pub failed: Vec<FailedSchedule>,
}

impl WatchdogReport {
    /// Combine the reports of one cron's batches.
    pub fn merge(&mut self, other: WatchdogReport) {
        self.checked += other.checked;
        self.missing += other.missing;
        self.overdue += other.overdue;
        self.unavailable += other.unavailable;
        if let Some(candidate) = other.oldest_overdue
            && self
                .oldest_overdue
                .as_ref()
                .is_none_or(|current| candidate.due_at < current.due_at)
        {
            self.oldest_overdue = Some(candidate);
        }
        self.failed.extend(other.failed);
    }

    /// The most recent failed sweep among the reported schedules.
    pub fn latest_failure(&self) -> Option<&FailedSchedule> {
        self.failed.iter().max_by_key(|f| f.last_failed_at)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account::App;
    fn account(account_id: u64) -> AccountInstallationRef {
        AccountInstallationRef {
            app: App::Repo,
            account_id,
        }
    }
    #[test]
    fn batches_merge_into_one_oldest_overdue_and_all_failures() {
        let at: DateTime<Utc> = "2026-11-24T10:00:00Z".parse().unwrap();
        let mut total = WatchdogReport {
            checked: 2,
            overdue: 1,
            oldest_overdue: Some(OverdueSchedule {
                account: account(1),
                due_at: at,
            }),
            ..Default::default()
        };
        total.merge(WatchdogReport {
            checked: 3,
            missing: 1,
            overdue: 1,
            unavailable: 1,
            oldest_overdue: Some(OverdueSchedule {
                account: account(2),
                due_at: at - chrono::Duration::hours(5),
            }),
            failed: vec![FailedSchedule {
                account: account(3),
                failed_runs: 2,
                last_failed_at: at,
            }],
        });
        total.merge(WatchdogReport {
            oldest_overdue: Some(OverdueSchedule {
                account: account(4),
                due_at: at,
            }),
            ..Default::default()
        });
        assert_eq!(
            (
                total.checked,
                total.missing,
                total.overdue,
                total.unavailable
            ),
            (5, 1, 2, 1)
        );
        assert_eq!(total.oldest_overdue.as_ref().unwrap().account, account(2));
        assert_eq!(total.latest_failure().unwrap().account, account(3));
    }
}
