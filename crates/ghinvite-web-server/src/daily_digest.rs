//! Native equivalent of the Worker's hourly cron: daily digests, cleanup and
//! the sweep watchdog (#86).
use chrono::{DateTime, Duration, Timelike, Utc};
use ghinvite_web::WebStorage;
use std::sync::Arc;

pub trait EmailMaintenanceStorage:
    ghinvite_core::storage::email_vault::EmailVaultStorage
    + ghinvite_core::storage::email_review::EmailReviewStorage
{
}
impl<
    T: ghinvite_core::storage::email_vault::EmailVaultStorage
        + ghinvite_core::storage::email_review::EmailReviewStorage,
> EmailMaintenanceStorage for T
{
}

fn next_check(now: DateTime<Utc>) -> std::time::Duration {
    let boundary = now
        .date_naive()
        .and_hms_opt(now.hour(), 5, 0)
        .unwrap()
        .and_utc();
    let next = if boundary > now {
        boundary
    } else {
        boundary + Duration::hours(1)
    };
    (next - now).to_std().expect("future hourly boundary")
}

/// Run immediately after startup to recover a missed cron. Repeated runs within
/// a UTC day preserve the first committed snapshot. A failed write retries in a
/// minute; successful/no-op writes wait for the next hourly minute-five boundary.
pub async fn maintain_once(
    storage: &dyn ghinvite_core::storage::blocked_digest::DailyDigestStorage,
    emails: &dyn EmailMaintenanceStorage,
    now: DateTime<Utc>,
) -> ghinvite_core::storage::Result<()> {
    let result = storage.record_blocked_digest(now).await;
    if emails.prune_emails(now.timestamp()).await.is_err() {
        tracing::warn!(
            alert = "email_recovery_cleanup_failed",
            "Email recovery cleanup unavailable"
        );
    }
    if emails.prune_review_emails(now.timestamp()).await.is_err() {
        tracing::warn!(
            alert = "email_review_cleanup_failed",
            "Email review cleanup unavailable"
        );
    }
    result
}
/// The hourly sweep watchdog (#86), when `GH_SWEEP_WATCHDOG_ENABLED` allows it.
pub struct SweepWatchdog {
    pub restate: Arc<ghinvite_web::restate_client::RestateClient>,
    pub organization_enabled: bool,
}

pub async fn run(
    storage: Arc<dyn WebStorage>,
    emails: Arc<dyn EmailMaintenanceStorage>,
    watchdog: Option<SweepWatchdog>,
) {
    loop {
        let now = Utc::now();
        let digest = maintain_once(storage.as_ref(), emails.as_ref(), now).await;
        if digest.is_err() {
            tracing::warn!(
                alert = "daily_delivery_digest_failed",
                "Daily delivery summary unavailable"
            );
        }
        // Independent of the digest, and safe to repeat after a failure.
        let watchdog = match &watchdog {
            Some(watchdog) => ghinvite_web::sweep_watchdog::check_and_record(
                storage.as_ref(),
                &watchdog.restate,
                watchdog.organization_enabled,
                now,
            )
            .await
            .map(|_| ())
            .inspect_err(|_| {
                tracing::warn!(
                    alert = "sweep_watchdog_failed",
                    "Sweep watchdog check unavailable"
                );
            }),
            None => Ok(()),
        };
        let wait = if digest.is_ok() && watchdog.is_ok() {
            next_check(Utc::now())
        } else {
            std::time::Duration::from_secs(60)
        };
        tokio::time::sleep(wait).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn hourly_checks_cross_utc_midnight_and_do_not_spin_at_the_boundary() {
        for (at, seconds) in [
            ("2026-10-16T00:04:59Z", 1),
            ("2026-10-16T00:05:00Z", 3600),
            ("2026-10-16T23:59:59Z", 301),
            ("2026-12-31T23:05:01Z", 3599),
        ] {
            assert_eq!(next_check(at.parse().unwrap()).as_secs(), seconds);
        }
    }
}

#[cfg(test)]
mod email_maintenance_tests {
    use super::*;
    use ghinvite_core::{
        bulk::{BatchRef, BulkBatchId},
        email_delivery::EmailRef,
        storage::{
            blocked_digest::DailyDigestStorage,
            email_vault::{EmailVaultBinding, EmailVaultStorage, NewEmailRecord},
        },
    };
    struct FailedDigest;
    #[async_trait::async_trait]
    impl DailyDigestStorage for FailedDigest {
        async fn record_blocked_digest(
            &self,
            _: DateTime<Utc>,
        ) -> ghinvite_core::storage::Result<()> {
            Err(ghinvite_core::storage::Error::Database(
                "test outage".into(),
            ))
        }
        async fn record_sweep_health(
            &self,
            _: DateTime<Utc>,
            _: &ghinvite_core::sweep_health::WatchdogReport,
        ) -> ghinvite_core::storage::Result<()> {
            Err(ghinvite_core::storage::Error::Database(
                "test outage".into(),
            ))
        }
        async fn sweep_digest(
            &self,
            _: chrono::NaiveDate,
        ) -> ghinvite_core::storage::Result<Option<ghinvite_core::storage::sweep_digest::SweepDigest>>
        {
            Ok(None)
        }
    }
    #[tokio::test]
    async fn scheduler_clock_preserves_active_rows_and_purges_expired_even_when_digest_fails() {
        let storage = ghinvite_storage_sqlx::SqlxStorage::in_memory()
            .await
            .unwrap();
        let now = 1800000000;
        let batch_id = BulkBatchId::new();
        let mut bindings = Vec::new();
        for n in 1..=3 {
            let binding = EmailVaultBinding {
                email_ref: EmailRef::from_bytes([n; 32]),
                batch: BatchRef {
                    batch_id,
                    row: u16::from(n),
                },
                account_id: 42,
                installation_id: 9,
            };
            storage
                .stage_email(&NewEmailRecord {
                    binding: binding.clone(),
                    ciphertext: "b3BhcXVl".into(),
                    created_at: now,
                })
                .await
                .unwrap();
            bindings.push(binding);
        }
        storage.activate_email(&bindings[1], now + 1).await.unwrap();
        storage.activate_email(&bindings[2], now + 1).await.unwrap();
        storage.retire_email(&bindings[2], now + 2).await.unwrap();
        maintain_once(
            &storage,
            &storage,
            DateTime::from_timestamp(now + 86399, 0).unwrap(),
        )
        .await
        .unwrap();
        assert!(
            storage
                .email_record(bindings[0].email_ref)
                .await
                .unwrap()
                .unwrap()
                .ciphertext
                .is_some()
        );
        assert!(
            maintain_once(
                &FailedDigest,
                &storage,
                DateTime::from_timestamp(now + 86400, 0).unwrap()
            )
            .await
            .is_err()
        );
        assert!(
            storage
                .email_record(bindings[0].email_ref)
                .await
                .unwrap()
                .unwrap()
                .ciphertext
                .is_none()
        );
        assert!(
            storage
                .email_record(bindings[1].email_ref)
                .await
                .unwrap()
                .unwrap()
                .ciphertext
                .is_some()
        );
        assert!(
            storage
                .email_record(bindings[2].email_ref)
                .await
                .unwrap()
                .unwrap()
                .ciphertext
                .is_some()
        );
        maintain_once(
            &storage,
            &storage,
            DateTime::from_timestamp(now + 86402, 0).unwrap(),
        )
        .await
        .unwrap();
        assert!(
            storage
                .email_record(bindings[2].email_ref)
                .await
                .unwrap()
                .unwrap()
                .ciphertext
                .is_none()
        );
    }
}

#[cfg(test)]
mod review_maintenance_tests {
    use super::*;
    use ghinvite_core::{
        bulk::{BatchRef, BulkBatchId},
        email_delivery::EmailRef,
        storage::{
            email_review::{EmailReviewStorage, NewReviewEmail},
            email_vault::EmailVaultBinding,
        },
    };
    #[tokio::test]
    async fn hourly_review_cleanup_is_bounded_at_original_expiry() {
        let storage = ghinvite_storage_sqlx::SqlxStorage::in_memory()
            .await
            .unwrap();
        let now = 1_800_000_000;
        let batch = BulkBatchId::new();
        let mut refs = vec![];
        for row in 1..=101u16 {
            let mut bytes = [0; 32];
            bytes[..2].copy_from_slice(&row.to_be_bytes());
            let email_ref = EmailRef::from_bytes(bytes);
            refs.push(email_ref);
            storage
                .retain_review_email(&NewReviewEmail {
                    binding: EmailVaultBinding {
                        email_ref,
                        batch: BatchRef {
                            batch_id: batch,
                            row,
                        },
                        account_id: 42,
                        installation_id: 9,
                    },
                    ciphertext: "b3BhcXVl".into(),
                    created_at: now,
                })
                .await
                .unwrap();
        }
        maintain_once(
            &storage,
            &storage,
            DateTime::from_timestamp(now + 3599, 0).unwrap(),
        )
        .await
        .unwrap();
        for r in &refs {
            assert!(
                storage
                    .review_email(*r)
                    .await
                    .unwrap()
                    .unwrap()
                    .ciphertext
                    .is_some()
            );
        }
        maintain_once(
            &storage,
            &storage,
            DateTime::from_timestamp(now + 3600, 0).unwrap(),
        )
        .await
        .unwrap();
        let mut cleared = 0;
        for r in &refs {
            cleared += usize::from(
                storage
                    .review_email(*r)
                    .await
                    .unwrap()
                    .unwrap()
                    .ciphertext
                    .is_none(),
            );
        }
        assert_eq!(cleared, 100);
        maintain_once(
            &storage,
            &storage,
            DateTime::from_timestamp(now + 7200, 0).unwrap(),
        )
        .await
        .unwrap();
        for r in refs {
            let record = storage.review_email(r).await.unwrap().unwrap();
            assert!(record.ciphertext.is_none());
            assert_eq!(record.expires_at, now + 3600);
        }
    }
}
