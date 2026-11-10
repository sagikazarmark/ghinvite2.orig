//! Scheduled projection-only maintenance. No GitHub or Restate traffic.
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
            digest.map_err(|_| ())
        }
        .await;
        match result {
            Ok(()) => Ok(wasm_bindgen::JsValue::UNDEFINED),
            Err(()) => {
                worker::console_error!("daily_delivery_digest_failed");
                Err(wasm_bindgen::JsValue::from_str(
                    "Daily delivery summary unavailable",
                ))
            }
        }
    }))
}
