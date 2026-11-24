//! The web's hourly watchdog over retained sweep chains (#86).
//!
//! One check reads each named account's schedule through its shared `status`
//! handler, so a run holding the schedule's lock never blocks it, and re-arms a
//! missing or overdue chain with a one-way `ensure_scheduled`. That handler
//! repeats the assessment under the schedule's exclusivity, so duplicate or
//! overlapping checks are no-ops on a chain another trigger already repaired.
//! The check reads no GitHub state and changes no delivery.
use crate::sweep_schedule::{Assessment, SweepScheduleClient, assess, recent_failure};
use chrono::{DateTime, Utc};
use ghinvite_core::{
    account::App,
    sweep_health::{
        FailedSchedule, MAX_WATCHDOG_ACCOUNTS, OverdueSchedule, WatchdogInput, WatchdogReport,
    },
};
use restate_sdk::{
    context::{Context, ContextClient},
    errors::TerminalError,
    serde::Json,
};
use std::collections::BTreeSet;

#[derive(Default)]
pub struct SweepWatchdog {
    pub organization_enabled: bool,
    #[cfg(feature = "integration")]
    pub clock: Option<std::sync::Arc<std::sync::atomic::AtomicI64>>,
}

impl SweepWatchdog {
    async fn now(&self, ctx: &Context<'_>) -> Result<DateTime<Utc>, TerminalError> {
        #[cfg(feature = "integration")]
        if let Some(clock) = &self.clock {
            return crate::clock::with_clock(ctx, "watchdog_time", || {
                DateTime::from_timestamp_millis(clock.load(std::sync::atomic::Ordering::SeqCst))
                    .expect("fixture clock")
            })
            .await;
        }
        crate::clock::now(ctx, "watchdog_time").await
    }
}

#[restate_sdk::service]
impl SweepWatchdog {
    #[handler]
    async fn check(
        &self,
        ctx: Context<'_>,
        Json(input): Json<WatchdogInput>,
    ) -> Result<Json<WatchdogReport>, TerminalError> {
        if input.accounts.len() > MAX_WATCHDOG_ACCOUNTS {
            return Err(TerminalError::new_with_code(
                400,
                "too many watchdog accounts",
            ));
        }
        let mut keys = BTreeSet::new();
        let mut accounts = Vec::new();
        for account in input.accounts {
            let key = account
                .key()
                .map_err(|_| TerminalError::new_with_code(400, "invalid watchdog account"))?;
            if keys.insert(key.clone()) {
                accounts.push((account, key));
            }
        }
        let now = self.now(&ctx).await?;
        let mut report = WatchdogReport::default();
        for (account, key) in accounts {
            if account.app == App::Org && !self.organization_enabled {
                report.unavailable += 1;
                continue;
            }
            let schedule = ctx
                .object_client::<SweepScheduleClient>(&key)
                .status()
                .call()
                .await;
            let Ok(Json(schedule)) = schedule else {
                report.unavailable += 1;
                continue;
            };
            report.checked += 1;
            let rearm = match assess(schedule.as_ref(), now) {
                Assessment::Missing => {
                    report.missing += 1;
                    true
                }
                Assessment::Overdue { due } => {
                    report.overdue += 1;
                    if report
                        .oldest_overdue
                        .as_ref()
                        .is_none_or(|oldest| due < oldest.due_at)
                    {
                        report.oldest_overdue = Some(OverdueSchedule {
                            account,
                            due_at: due,
                        });
                    }
                    true
                }
                Assessment::Scheduled { .. } => false,
            };
            if rearm {
                ctx.object_client::<SweepScheduleClient>(&key)
                    .ensure_scheduled()
                    .send()
                    .await?;
            }
            if let Some(schedule) = &schedule
                && let Some(last_failed_at) = recent_failure(schedule, now)
            {
                report.failed.push(FailedSchedule {
                    account,
                    failed_runs: schedule.health.failed_runs,
                    last_failed_at,
                });
            }
        }
        Ok(Json(report))
    }
}
