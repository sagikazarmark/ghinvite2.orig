//! Webhook delivery (GE-04): every outbox event (`crate::events`) becomes
//! one delivery per target whose App has an active webhook, POSTed to the
//! App's URL with GitHub's headers and signatures, and every attempt is
//! recorded for the deliveries API (`GET /app/hook/deliveries…`) and the
//! admin API (`GET /__octoemu/deliveries`).
//!
//! - **Order.** One sender per App sends that App's deliveries one at a
//!   time in event order (behaviour `webhook-ordering`); different Apps
//!   send concurrently.
//! - **Dispatch.** [`Dispatch::Auto`] sends as soon as an event is
//!   recorded; [`Dispatch::Manual`] holds deliveries until
//!   `POST /__octoemu/deliveries/dispatch`. `POST /__octoemu/deliveries/wait`
//!   blocks until nothing is being sent or waiting to be sent (held
//!   deliveries excepted), so tests are deterministic.
//! - **Failure.** A response outside 2xx, a connection failure or no
//!   complete response within the timeout (10 s, documented) is a failed
//!   delivery; nothing is retried (documented). A redelivery
//!   (`POST /app/hook/deliveries/{id}/attempts`) resends the same body with
//!   the same GUID.
//! - **Secrets.** A delivery is signed with the App's current secret, the
//!   first of `webhook.secrets`, at send time (behaviour
//!   `webhook-secret-rotation`); without one it goes unsigned.

use crate::events::Event;
use crate::state::{SharedState, State};
use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::time::{Duration, Instant};

/// How long GitHub waits for a response (documented: "takes longer than
/// 10 seconds to respond, GitHub will record the delivery as a failure").
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

/// The most of a response body an attempt records.
pub(crate) const RESPONSE_BODY_LIMIT: usize = 64 * 1024;

/// `X-GitHub-Hook-ID` is this plus the App ID (behaviour `webhook-hook-id`).
pub(crate) const HOOK_ID_OFFSET: u64 = 400_000_000;

/// `User-Agent` of every delivery (documented prefix `GitHub-Hookshot/`).
pub(crate) const USER_AGENT: &str = "GitHub-Hookshot/octoemu";

/// When recorded events are sent.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Dispatch {
    /// As soon as the event is recorded (the default).
    #[default]
    Auto,
    /// Held until dispatched (`POST /__octoemu/deliveries/dispatch`,
    /// [`Emulator::dispatch_deliveries`](crate::Emulator::dispatch_deliveries)).
    Manual,
}

/// One delivery attempt, as `GET /app/hook/deliveries/{id}` reports it,
/// plus octoemu's `app_id` and `event_id`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Delivery {
    /// Sequential from 1 per emulator instance; a redelivery gets its own.
    pub id: u64,
    /// `X-GitHub-Delivery`; a redelivery keeps the original's.
    pub guid: String,
    /// When it was sent, on the virtual clock.
    pub delivered_at: DateTime<Utc>,
    /// Whether this is a redelivery.
    pub redelivery: bool,
    /// Seconds until the response (or the failure), real time.
    pub duration: f64,
    /// `OK`, or what went wrong (behaviour `webhook-status-texts`).
    pub status: String,
    /// The response's status code; 0 without a response.
    pub status_code: u16,
    /// `X-GitHub-Event`.
    pub event: String,
    /// The payload's `action`.
    pub action: Option<String>,
    /// The installation it was for.
    pub installation_id: Option<u64>,
    /// The payload's `repository.id`, if any.
    pub repository_id: Option<u64>,
    /// Always `null`: octoemu does not throttle.
    pub throttled_at: Option<DateTime<Utc>>,
    /// Where it was sent.
    pub url: String,
    /// The request's headers and payload.
    pub request: DeliveryRequest,
    /// The response's headers and body (the first 64 KiB).
    pub response: DeliveryResponse,
    /// The App it was for (octoemu's addition).
    pub app_id: u64,
    /// The outbox event it delivered (octoemu's addition).
    pub event_id: u64,
}

/// What a delivery sent.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DeliveryRequest {
    /// Request headers, by GitHub's spelling (`X-GitHub-Event`, …).
    pub headers: BTreeMap<String, String>,
    /// The JSON body.
    pub payload: Value,
}

/// What a delivery got back.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct DeliveryResponse {
    /// Response headers (lowercase names); empty without a response.
    pub headers: BTreeMap<String, String>,
    /// The body as text, truncated; `null` without a response.
    pub payload: Option<String>,
}

impl Delivery {
    /// The list form (`simple-delivery`): no URL, request or response.
    pub(crate) fn summary(&self) -> Value {
        json!({
            "id": self.id,
            "guid": self.guid,
            "delivered_at": crate::views::timestamp(self.delivered_at),
            "redelivery": self.redelivery,
            "duration": self.duration,
            "status": self.status,
            "status_code": self.status_code,
            "event": self.event,
            "action": self.action,
            "installation_id": self.installation_id,
            "repository_id": self.repository_id,
            "throttled_at": self.throttled_at,
        })
    }

    /// The detail form (`hook-delivery`).
    pub(crate) fn detail(&self) -> Value {
        let mut body = self.summary();
        body["url"] = self.url.clone().into();
        body["request"] = json!({"headers": self.request.headers, "payload": self.request.payload});
        body["response"] =
            json!({"headers": self.response.headers, "payload": self.response.payload});
        body
    }

    /// Whether the deliveries API's `status=success` matches it (documented:
    /// a status code from 200 to 399); everything else, including no
    /// response, is a `failure`.
    pub(crate) fn succeeded(&self) -> bool {
        (200..400).contains(&self.status_code)
    }
}

/// A delivery waiting to be sent.
#[derive(Clone, Debug)]
struct Job {
    app_id: u64,
    event_id: u64,
    guid: String,
    redelivery: bool,
    event: String,
    action: Option<String>,
    installation_id: Option<u64>,
    repository_id: Option<u64>,
    /// The exact body; a redelivery resends these bytes.
    body: String,
}

/// Delivery state, behind the emulator's lock.
#[derive(Debug)]
pub(crate) struct Deliveries {
    /// Outbox events already turned into jobs.
    cursor: usize,
    /// Jobs waiting for a manual dispatch, in order.
    held: Vec<Job>,
    /// Jobs to send, per App, in order.
    ready: BTreeMap<u64, VecDeque<Job>>,
    /// Apps whose sender is running.
    busy: BTreeSet<u64>,
    /// Every attempt, oldest first, with its exact body.
    attempts: Vec<(Delivery, String)>,
    pub(crate) mode: Dispatch,
    pub(crate) timeout: Duration,
    /// `insecure_ssl` per App as last set through `PATCH /app/hook/config`.
    pub(crate) insecure_ssl: BTreeMap<u64, String>,
}

impl Default for Deliveries {
    fn default() -> Self {
        Self {
            cursor: 0,
            held: Vec::new(),
            ready: BTreeMap::new(),
            busy: BTreeSet::new(),
            attempts: Vec::new(),
            mode: Dispatch::Auto,
            timeout: DEFAULT_TIMEOUT,
            insecure_ssl: BTreeMap::new(),
        }
    }
}

impl Deliveries {
    /// Every attempt, oldest first.
    pub(crate) fn attempts(&self) -> impl DoubleEndedIterator<Item = &Delivery> {
        self.attempts.iter().map(|(delivery, _)| delivery)
    }

    /// Jobs held for a manual dispatch.
    pub(crate) fn held(&self) -> usize {
        self.held.len()
    }

    /// Jobs ready or being sent.
    pub(crate) fn pending(&self) -> usize {
        self.ready.values().map(VecDeque::len).sum::<usize>() + self.busy.len()
    }

    fn enqueue(&mut self, job: Job) {
        match self.mode {
            Dispatch::Auto => self.ready.entry(job.app_id).or_default().push_back(job),
            Dispatch::Manual => self.held.push(job),
        }
    }

    /// Moves every held job to the send queues; returns how many.
    pub(crate) fn release(&mut self) -> usize {
        let held = std::mem::take(&mut self.held);
        let count = held.len();
        for job in held {
            self.ready.entry(job.app_id).or_default().push_back(job);
        }
        count
    }

    /// Sets the mode; switching to auto releases held jobs.
    pub(crate) fn set_mode(&mut self, mode: Dispatch) {
        self.mode = mode;
        if mode == Dispatch::Auto {
            self.release();
        }
    }
}

/// A random version-4 GUID, `X-GitHub-Delivery`'s format.
fn guid() -> String {
    let mut bytes = [0u8; 16];
    if getrandom_02::getrandom(&mut bytes).is_err() {
        // Unique per process at least.
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        bytes = nanos.to_le_bytes();
    }
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

/// Whether App `app_id` has an active webhook.
fn hooked(state: &State, app_id: u64) -> bool {
    state
        .world
        .apps
        .iter()
        .find(|app| app.id == app_id)
        .and_then(|app| app.webhook.as_ref())
        .is_some_and(|hook| hook.active)
}

/// Turns outbox events not seen yet into jobs.
pub(crate) fn pump(state: &mut State) {
    let start = state.deliveries.cursor;
    let events: Vec<Event> = state.events.events()[start..].to_vec();
    state.deliveries.cursor += events.len();
    for event in events {
        for target in &event.targets {
            if !hooked(state, target.app_id) {
                continue;
            }
            let mut payload = event.payload.clone();
            if payload.get("installation").is_none() {
                payload["installation"] = json!({
                    "id": target.installation_id,
                    "node_id": crate::views::node_id("IntegrationInstallation", target.installation_id),
                });
            }
            let repository_id = payload
                .get("repository")
                .and_then(|r| r.get("id"))
                .and_then(Value::as_u64);
            let job = Job {
                app_id: target.app_id,
                event_id: event.id,
                guid: guid(),
                redelivery: false,
                event: event.event.clone(),
                action: event.action.clone(),
                installation_id: Some(target.installation_id),
                repository_id,
                body: payload.to_string(),
            };
            state.deliveries.enqueue(job);
        }
    }
}

/// Queues a redelivery of attempt `id` of App `app_id`: the same body and
/// GUID, `redelivery: true`. `false` when there is no such attempt.
pub(crate) fn redeliver(shared: &SharedState, app_id: u64, id: u64) -> bool {
    let mut state = shared.state();
    let Some((original, body)) = state
        .deliveries
        .attempts
        .iter()
        .find(|(d, _)| d.id == id && d.app_id == app_id)
    else {
        return false;
    };
    let job = Job {
        app_id,
        event_id: original.event_id,
        guid: original.guid.clone(),
        redelivery: true,
        event: original.event.clone(),
        action: original.action.clone(),
        installation_id: original.installation_id,
        repository_id: original.repository_id,
        body: body.clone(),
    };
    state.deliveries.enqueue(job);
    drop(state);
    shared.wake.notify_one();
    true
}

/// The dispatcher: wakes on every outbox push (and dispatch, redelivery,
/// mode change) and starts a sender for each App with work and none
/// running. Ends when `stop` turns true.
pub(crate) async fn dispatcher(shared: SharedState, mut stop: tokio::sync::watch::Receiver<bool>) {
    loop {
        tokio::select! {
            () = shared.wake.notified() => {}
            _ = stop.wait_for(|stop| *stop) => return,
        }
        let starting: Vec<u64> = {
            let mut state = shared.state();
            pump(&mut state);
            let deliveries = &mut state.deliveries;
            let apps: Vec<u64> = deliveries
                .ready
                .iter()
                .filter(|(app, jobs)| !jobs.is_empty() && !deliveries.busy.contains(app))
                .map(|(app, _)| *app)
                .collect();
            deliveries.busy.extend(apps.iter().copied());
            apps
        };
        for app_id in starting {
            tokio::spawn(sender(shared.clone(), app_id));
        }
        shared.settled.send_modify(|generation| *generation += 1);
    }
}

/// Sends App `app_id`'s ready jobs one at a time, then stops.
async fn sender(shared: SharedState, app_id: u64) {
    loop {
        let next = {
            let mut state = shared.state();
            let job = state
                .deliveries
                .ready
                .get_mut(&app_id)
                .and_then(VecDeque::pop_front);
            match job {
                Some(job) => {
                    let target = state
                        .world
                        .apps
                        .iter()
                        .find(|app| app.id == app_id)
                        .and_then(|app| app.webhook.clone())
                        .filter(|hook| hook.active);
                    Some((job, target, state.clock.now(), state.deliveries.timeout))
                }
                None => {
                    state.deliveries.busy.remove(&app_id);
                    None
                }
            }
        };
        let Some((job, hook, delivered_at, timeout)) = next else {
            shared.settled.send_modify(|generation| *generation += 1);
            return;
        };
        // The webhook went away or was deactivated since: nothing is sent.
        let Some(hook) = hook else { continue };
        let attempt = send(&shared, &job, &hook, delivered_at, timeout).await;
        let mut state = shared.state();
        let id = state.deliveries.attempts.len() as u64 + 1;
        state
            .deliveries
            .attempts
            .push((Delivery { id, ..attempt }, job.body));
        drop(state);
        shared.settled.send_modify(|generation| *generation += 1);
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// `X-Hub-Signature-256`'s value: `sha256=` and the hex HMAC-SHA256 of
/// `body` under `secret` (documented).
pub(crate) fn signature_256(secret: &str, body: &[u8]) -> String {
    let mut mac = Hmac::<sha2::Sha256>::new_from_slice(secret.as_bytes())
        .expect("HMAC takes keys of any length");
    mac.update(body);
    format!("sha256={}", hex(&mac.finalize().into_bytes()))
}

/// `X-Hub-Signature`'s value (legacy, documented): `sha1=` and the hex
/// HMAC-SHA1.
pub(crate) fn signature_1(secret: &str, body: &[u8]) -> String {
    let mut mac = Hmac::<sha1::Sha1>::new_from_slice(secret.as_bytes())
        .expect("HMAC takes keys of any length");
    mac.update(body);
    format!("sha1={}", hex(&mac.finalize().into_bytes()))
}

/// Sends one job; the attempt (with ID 0, set by the caller).
async fn send(
    shared: &SharedState,
    job: &Job,
    hook: &crate::seed::Webhook,
    delivered_at: DateTime<Utc>,
    timeout: Duration,
) -> Delivery {
    let mut headers = BTreeMap::from([
        ("Accept".to_owned(), "*/*".to_owned()),
        ("Content-Type".to_owned(), "application/json".to_owned()),
        ("User-Agent".to_owned(), USER_AGENT.to_owned()),
        ("X-GitHub-Delivery".to_owned(), job.guid.clone()),
        ("X-GitHub-Event".to_owned(), job.event.clone()),
        (
            "X-GitHub-Hook-ID".to_owned(),
            (HOOK_ID_OFFSET + job.app_id).to_string(),
        ),
        (
            "X-GitHub-Hook-Installation-Target-ID".to_owned(),
            job.app_id.to_string(),
        ),
        (
            "X-GitHub-Hook-Installation-Target-Type".to_owned(),
            "integration".to_owned(),
        ),
    ]);
    if let Some(secret) = hook.secrets.first().filter(|s| !s.is_empty()) {
        headers.insert(
            "X-Hub-Signature".to_owned(),
            signature_1(secret, job.body.as_bytes()),
        );
        headers.insert(
            "X-Hub-Signature-256".to_owned(),
            signature_256(secret, job.body.as_bytes()),
        );
    }
    let mut request = shared
        .client
        .post(&hook.url)
        .timeout(timeout)
        .body(job.body.clone());
    for (name, value) in &headers {
        request = request.header(name, value);
    }
    let started = Instant::now();
    let message = |text: &str| shared.message("webhook-status-texts", text).into_owned();
    let (status, status_code, response) = match request.send().await {
        Ok(mut reply) => {
            let code = reply.status().as_u16();
            let response_headers = reply
                .headers()
                .iter()
                .map(|(name, value)| {
                    (
                        name.as_str().to_owned(),
                        String::from_utf8_lossy(value.as_bytes()).into_owned(),
                    )
                })
                .collect();
            let mut body = Vec::new();
            let mut timed_out = false;
            loop {
                match reply.chunk().await {
                    Ok(Some(chunk)) => {
                        let room = RESPONSE_BODY_LIMIT.saturating_sub(body.len());
                        body.extend_from_slice(&chunk[..chunk.len().min(room)]);
                    }
                    Ok(None) => break,
                    Err(error) => {
                        timed_out = error.is_timeout();
                        break;
                    }
                }
            }
            let status = if timed_out {
                message("timed out")
            } else if reply.status().is_success() {
                "OK".to_owned()
            } else {
                message(&format!("Invalid HTTP Response: {code}"))
            };
            let code = if timed_out { 0 } else { code };
            (
                status,
                code,
                DeliveryResponse {
                    headers: response_headers,
                    payload: Some(String::from_utf8_lossy(&body).into_owned()),
                },
            )
        }
        Err(error) => {
            let status = if error.is_timeout() {
                message("timed out")
            } else if error.is_connect() {
                message("Couldn't connect to server")
            } else {
                message("Invalid HTTP Response")
            };
            (status, 0, DeliveryResponse::default())
        }
    };
    Delivery {
        id: 0,
        guid: job.guid.clone(),
        delivered_at,
        redelivery: job.redelivery,
        duration: started.elapsed().as_secs_f64(),
        status,
        status_code,
        event: job.event.clone(),
        action: job.action.clone(),
        installation_id: job.installation_id,
        repository_id: job.repository_id,
        throttled_at: None,
        url: hook.url.clone(),
        request: DeliveryRequest {
            headers,
            payload: serde_json::from_str(&job.body).unwrap_or(Value::Null),
        },
        response,
        app_id: job.app_id,
        event_id: job.event_id,
    }
}

/// Waits until no delivery is ready or being sent (held ones excepted), at
/// most `timeout`. `true` once idle.
pub(crate) async fn wait_idle(shared: &SharedState, timeout: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + timeout;
    let mut settled = shared.settled.subscribe();
    shared.wake.notify_one();
    loop {
        let idle = {
            let mut state = shared.state();
            pump(&mut state);
            state.deliveries.pending() == 0
        };
        if idle {
            return true;
        }
        match tokio::time::timeout_at(deadline, settled.changed()).await {
            Ok(Ok(())) => {}
            Ok(Err(_)) | Err(_) => return false,
        }
    }
}

/// Releases held deliveries (manual mode); returns how many.
pub(crate) fn dispatch(shared: &SharedState) -> usize {
    let released = {
        let mut state = shared.state();
        pump(&mut state);
        state.deliveries.release()
    };
    shared.wake.notify_one();
    released
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The test vector of GitHub's "Validating webhook deliveries" page.
    #[test]
    fn signatures_match_githubs_test_vector() {
        assert_eq!(
            signature_256("It's a Secret to Everybody", b"Hello, World!"),
            "sha256=757107ea0eb2509fc211221cce984b8a37570b6d7586c22c46f4379c8b043e17"
        );
        assert!(signature_1("It's a Secret to Everybody", b"Hello, World!").starts_with("sha1="));
    }

    #[test]
    fn guids_are_version_4() {
        let id = guid();
        assert_eq!(id.len(), 36);
        assert_eq!(&id[14..15], "4");
        assert_ne!(id, guid());
    }
}
