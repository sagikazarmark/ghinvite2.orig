//! The hourly cron's watchdog over retained sweep chains (#86).
//!
//! The web knows the account set from its installation projection; the
//! workflow's `SweepWatchdog/check` reads each retained schedule and re-arms a
//! missing or overdue chain. Both are safe to repeat: a re-armed chain catches
//! up with one sweep, and checking a healthy chain changes nothing. A lagging
//! projection can only delay a re-arm until a later hour; it never schedules
//! work by itself. No GitHub traffic.
use crate::restate_client::RestateClient;
use ghinvite_core::{
    account::{AccountInstallationRef, App},
    storage::{AppInstallationReadStorage, blocked_digest::DailyDigestStorage},
    sweep_health::{MAX_WATCHDOG_ACCOUNTS, WatchdogInput, WatchdogReport},
};
use std::collections::BTreeSet;

/// Why a check produced no complete report. Fixed text only.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WatchdogFailure {
    /// The installation projection could not be read.
    Accounts,
    /// At least one batch's ingress call failed. Batches that succeeded may
    /// still have re-armed their chains.
    Ingress,
    /// The report could not be recorded in the daily digest.
    Digest,
}

/// Every account with an active installation of the swept apps, once.
pub async fn accounts(
    installations: &(impl AppInstallationReadStorage + ?Sized),
    organization_enabled: bool,
) -> ghinvite_core::storage::Result<Vec<AccountInstallationRef>> {
    let mut apps = vec![App::Repo];
    if organization_enabled {
        apps.push(App::Org);
    }
    let mut seen = BTreeSet::new();
    let mut accounts = Vec::new();
    for app in apps {
        for installation in installations.list_active_app_installations(app).await? {
            let account_id = installation.account.account_id;
            if seen.insert((app.as_str(), account_id)) {
                accounts.push(AccountInstallationRef { app, account_id });
            }
        }
    }
    Ok(accounts)
}

/// One check over every account, in ingress batches. All batches are tried
/// even when one fails, so one unreachable batch cannot starve the others.
pub async fn check(
    installations: &(impl AppInstallationReadStorage + ?Sized),
    restate: &RestateClient,
    organization_enabled: bool,
) -> Result<WatchdogReport, WatchdogFailure> {
    let accounts = accounts(installations, organization_enabled)
        .await
        .map_err(|_| WatchdogFailure::Accounts)?;
    let mut report = WatchdogReport::default();
    let mut failed = false;
    for batch in accounts.chunks(MAX_WATCHDOG_ACCOUNTS) {
        let input = WatchdogInput {
            accounts: batch.to_vec(),
        };
        match restate
            .call::<_, WatchdogReport>("SweepWatchdog", "", "check", &input)
            .await
        {
            Ok(batch) => report.merge(batch),
            Err(_) => failed = true,
        }
    }
    if failed {
        return Err(WatchdogFailure::Ingress);
    }
    Ok(report)
}

/// [`check`], then fold the complete report into the daily digest.
pub async fn check_and_record(
    storage: &(impl AppInstallationReadStorage + DailyDigestStorage + ?Sized),
    restate: &RestateClient,
    organization_enabled: bool,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<WatchdogReport, WatchdogFailure> {
    let report = check(storage, restate, organization_enabled).await?;
    storage
        .record_sweep_health(now, &report)
        .await
        .map_err(|_| WatchdogFailure::Digest)?;
    Ok(report)
}
