//! Scheduled maintenance: projection-only digests and cleanup, then the sweep
//! watchdog (#86), which is the cron's only Restate traffic. No GitHub traffic.
use ghinvite_core::storage::{
    blocked_digest::DailyDigestStorage, email_review::EmailReviewStorage,
    email_vault::EmailVaultStorage,
};
use worker::{Env, js_sys, wasm_bindgen};

// worker 0.8's event(scheduled) wrapper discards a returned Result and resolves
// its promise. Export the same signature explicitly so a storage failure rejects
// the scheduled invocation and remains visible to the platform's cron reporting.
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn scheduled(
    _event: worker::worker_sys::ScheduledEvent,
    env: Env,
    _ctx: worker::worker_sys::ScheduleContext,
) -> js_sys::Promise {
    js_sys::futures::future_to_promise(std::panic::AssertUnwindSafe(async move {
        let result = async {
            let database = env.d1("DB").map_err(|_| ())?;
            let storage = ghinvite_storage_d1::D1Storage::new(database);
            // A delayed cron cannot backdate today's database contents. Daily
            // identity and the observation time both use actual processing time.
            let now = chrono::Utc::now();
            let digest = storage.record_blocked_digest(now).await;
            if storage.prune_emails(now.timestamp()).await.is_err() {
                worker::console_error!("email_recovery_cleanup_failed");
            }
            if storage.prune_review_emails(now.timestamp()).await.is_err() {
                worker::console_error!("email_review_cleanup_failed");
            }
            if digest.is_err() {
                worker::console_error!("daily_delivery_digest_failed");
            }
            // Independent of the digest: a projection outage must not also
            // leave a dead sweep chain unrepaired for another hour.
            let watchdog = sweep_watchdog(&env, &storage, now).await;
            if watchdog.is_err() {
                worker::console_error!("sweep_watchdog_failed");
            }
            digest.map_err(|_| ())?;
            watchdog
        }
        .await;
        match result {
            Ok(()) => Ok(wasm_bindgen::JsValue::UNDEFINED),
            Err(()) => Err(wasm_bindgen::JsValue::from_str(
                "Scheduled maintenance unavailable",
            )),
        }
    }))
}

/// Re-arm missing or overdue sweep chains and record the check in the daily
/// digest. Off unless enabled for this deployment (#61 owns production).
async fn sweep_watchdog(
    env: &Env,
    storage: &ghinvite_storage_d1::D1Storage,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<(), ()> {
    let setting = env
        .var(ghinvite_config::sweep::WATCHDOG_FIELD)
        .ok()
        .map(|v| v.to_string());
    if !ghinvite_config::sweep::watchdog_enabled(crate::DEPLOYMENT, setting.as_deref())
        .map_err(|_| ())?
    {
        return Ok(());
    }
    let config = crate::config_from_env(env).map_err(|_| ())?;
    let restate = ghinvite_web::restate_client::RestateClient::with_auth(
        &config.restate_ingress,
        config.restate_auth.clone(),
    )
    .map_err(|_| ())?;
    ghinvite_web::sweep_watchdog::check_and_record(
        storage,
        &restate,
        config.organization_delivery_enabled,
        now,
    )
    .await
    .map(|_| ())
    .map_err(|_| ())
}
