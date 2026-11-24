use super::*;
use crate::{
    account::{AccountInstallationRef, App},
    storage::blocked_digest::DailyDigestStorage,
    sweep_health::{FailedSchedule, OverdueSchedule, WatchdogReport},
};
use chrono::Duration;

fn account(app: App, account_id: u64) -> AccountInstallationRef {
    AccountInstallationRef { app, account_id }
}

/// Hourly watchdog checks fold into one row per UTC day: the oldest overdue
/// chain and the latest failure win, counts accumulate, a late check for an
/// older day cannot backfill, and seven days are kept.
pub async fn scenario_sweep_digest<S: DailyDigestStorage>(s: S) {
    let now = dt("2026-05-04T10:05:00Z");
    let day = now.date_naive();
    assert!(s.sweep_digest(day).await.unwrap().is_none());
    let healthy = WatchdogReport {
        checked: 3,
        ..Default::default()
    };
    s.record_sweep_health(now, &healthy).await.unwrap();
    let first = s.sweep_digest(day).await.unwrap().unwrap();
    assert_eq!(
        (
            first.checks,
            first.schedules,
            first.rearmed,
            first.failed_schedules
        ),
        (1, 3, 0, 0)
    );
    assert_eq!(
        (first.first_observed_at, first.last_observed_at),
        (now, now)
    );
    assert_eq!((first.oldest_overdue, first.latest_failure), (None, None));

    let overdue_at = now - Duration::days(3);
    let failed_at = now + Duration::minutes(30);
    let degraded = WatchdogReport {
        checked: 4,
        missing: 1,
        overdue: 2,
        unavailable: 1,
        oldest_overdue: Some(OverdueSchedule {
            account: account(App::Repo, 100),
            due_at: overdue_at,
        }),
        failed: vec![
            FailedSchedule {
                account: account(App::Repo, 200),
                failed_runs: 3,
                last_failed_at: failed_at - Duration::hours(2),
            },
            FailedSchedule {
                account: account(App::Org, 300),
                failed_runs: 1,
                last_failed_at: failed_at,
            },
        ],
    };
    let later = now + Duration::hours(1);
    s.record_sweep_health(later, &degraded).await.unwrap();
    // A newer but less severe check keeps the day's worst observations.
    let recovering = WatchdogReport {
        checked: 2,
        overdue: 1,
        oldest_overdue: Some(OverdueSchedule {
            account: account(App::Repo, 101),
            due_at: overdue_at + Duration::hours(1),
        }),
        failed: vec![FailedSchedule {
            account: account(App::Repo, 200),
            failed_runs: 4,
            last_failed_at: failed_at - Duration::minutes(1),
        }],
        ..Default::default()
    };
    let latest = now + Duration::hours(2);
    s.record_sweep_health(latest, &recovering).await.unwrap();
    let digest = s.sweep_digest(day).await.unwrap().unwrap();
    assert_eq!(
        (
            digest.checks,
            digest.schedules,
            digest.rearmed,
            digest.unavailable,
            digest.failed_schedules
        ),
        (3, 4, 4, 1, 2)
    );
    assert_eq!(
        (digest.first_observed_at, digest.last_observed_at),
        (now, latest)
    );
    assert_eq!(
        digest.oldest_overdue,
        Some((account(App::Repo, 100), overdue_at))
    );
    assert_eq!(
        digest.latest_failure,
        Some((account(App::Org, 300), failed_at))
    );

    // The next day starts fresh; the previous day can no longer change.
    let tomorrow = now + Duration::days(1);
    s.record_sweep_health(tomorrow, &healthy).await.unwrap();
    s.record_sweep_health(later, &degraded).await.unwrap();
    assert_eq!(s.sweep_digest(day).await.unwrap().unwrap(), digest);
    let next = s
        .sweep_digest(tomorrow.date_naive())
        .await
        .unwrap()
        .unwrap();
    assert_eq!((next.checks, next.oldest_overdue), (1, None));

    // Seven UTC days are retained.
    s.record_sweep_health(now + Duration::days(7), &healthy)
        .await
        .unwrap();
    assert!(s.sweep_digest(day).await.unwrap().is_none());
    assert!(
        s.sweep_digest(tomorrow.date_naive())
            .await
            .unwrap()
            .is_some()
    );
}
