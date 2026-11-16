//! Everything an emulator instance holds: the world behind one lock, plus
//! the fidelity manifest and activity tracking for the idle timeout.

use crate::auth::Auth;
use crate::calls::{Call, CallLog};
use crate::clock::Clock;
use crate::events::Outbox;
use crate::fidelity::{Fidelity, Manifest};
use crate::oauth::OAuthControls;
use crate::response::{OCTOEMU_DOCS, json};
use crate::seed::{Seed, SeedError};
use crate::tokens::Tokens;
use crate::user_tokens::UserTokens;
use crate::webhooks::Deliveries;
use axum::http::StatusCode;
use axum::response::Response;
use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use std::borrow::Cow;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

pub(crate) type SharedState = Arc<Shared>;

/// The emulated world. Later tasks add their resources here.
#[derive(Debug)]
pub(crate) struct State {
    /// Seeded objects, changed in place (suspension, uninstalling, …).
    pub(crate) world: Seed,
    pub(crate) clock: Clock,
    pub(crate) calls: CallLog,
    /// Installation tokens minted by this instance.
    pub(crate) tokens: Tokens,
    /// Events GitHub would announce by webhook.
    pub(crate) events: Outbox,
    /// Their deliveries: queues and recorded attempts (`crate::webhooks`).
    pub(crate) deliveries: Deliveries,
    /// Repository invitations sent through the API: (repository ID, when),
    /// for the 50-per-24-hours limit.
    pub(crate) invitations_sent: Vec<(u64, DateTime<Utc>)>,
    /// The highest repository invitation ID handed out, so a deleted
    /// invitation's ID is never reused.
    pub(crate) last_invitation_id: u64,
    /// Organization invitations sent (POST invitations, PUT memberships of
    /// non-members, team PUTs for non-members): (lowercased organization,
    /// when), for the 50-or-500-per-24-hours budget.
    pub(crate) org_invitations_sent: Vec<(String, DateTime<Utc>)>,
    /// The highest organization invitation ID handed out.
    pub(crate) last_org_invitation_id: u64,
    /// Removed organization members and their role, for `reinstate`:
    /// (lowercased organization, lowercased login, role).
    pub(crate) former_members: Vec<(String, String, crate::seed::OrgRole)>,
    /// Organizations whose invitation writes answer `spammed` (lowercased;
    /// `POST /__octoemu/orgs/{org}/spammed`).
    pub(crate) spammed_orgs: std::collections::BTreeSet<String>,
    /// OAuth codes, user access tokens, refresh tokens and PATs.
    pub(crate) user_tokens: UserTokens,
    /// How the authorize page answers (`crate::oauth`).
    pub(crate) oauth: OAuthControls,
    /// Fault rules (`crate::faults`).
    pub(crate) faults: crate::faults::Faults,
    /// The stub-compatibility shim's state (`crate::compat`; GE-09 deletes
    /// it).
    pub(crate) compat: crate::compat::CompatState,
}

/// The `message` sent instead of an `assumed` one in refuse-assumed mode:
/// the documented status stays, the guessed text does not.
pub(crate) const WITHHELD: &str = "octoemu: assumed message withheld";

/// One emulator instance's shared data.
#[derive(Debug)]
pub(crate) struct Shared {
    state: Mutex<State>,
    manifest: Arc<Manifest>,
    /// `METHOD /pattern` of every registered GitHub route, with what it
    /// accepts.
    registered: BTreeMap<String, Auth>,
    /// `settings.refuse_assumed`, readable without the lock.
    strict: AtomicBool,
    pub(crate) activity: Activity,
    sequence: AtomicU64,
    pub(crate) port: u16,
    /// `http://host:port` as bound, for absolute URLs without a `Host`.
    pub(crate) root: String,
    /// Wakes the delivery dispatcher (the outbox notifies it on every push).
    pub(crate) wake: Arc<tokio::sync::Notify>,
    /// Bumped whenever delivery work starts or finishes, for `wait`.
    pub(crate) settled: tokio::sync::watch::Sender<u64>,
    /// The HTTP client webhooks are delivered with (no redirects).
    pub(crate) client: reqwest::Client,
}

impl Shared {
    pub(crate) fn new(
        world: Seed,
        manifest: Arc<Manifest>,
        registered: BTreeMap<String, Auth>,
        root: String,
        port: u16,
    ) -> Self {
        let clock = Clock::new(world.settings.start_time);
        let initial = world.clone();
        let strict = AtomicBool::new(world.settings.refuse_assumed);
        let events = Outbox::default();
        let wake = events.waker();
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap_or_default();
        Self {
            state: Mutex::new(State {
                world,
                clock,
                calls: CallLog::default(),
                tokens: Tokens::default(),
                events,
                deliveries: Deliveries::default(),
                invitations_sent: Vec::new(),
                last_invitation_id: 0,
                org_invitations_sent: Vec::new(),
                last_org_invitation_id: 0,
                former_members: Vec::new(),
                spammed_orgs: std::collections::BTreeSet::new(),
                user_tokens: UserTokens::default(),
                oauth: OAuthControls::default(),
                faults: crate::faults::Faults::default(),
                compat: crate::compat::CompatState {
                    initial: Some(initial),
                },
            }),
            manifest,
            registered,
            strict,
            activity: Activity::new(),
            sequence: AtomicU64::new(0),
            port,
            root,
            wake,
            settled: tokio::sync::watch::Sender::new(0),
            client,
        }
    }

    /// The one lock. A panicking handler does not wedge the emulator.
    pub(crate) fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    pub(crate) fn now(&self) -> DateTime<Utc> {
        self.state().clock.now()
    }

    pub(crate) fn next_sequence(&self) -> u64 {
        self.sequence.fetch_add(1, Ordering::Relaxed) + 1
    }

    /// What a registered route accepts; `None` for an unregistered one.
    pub(crate) fn auth_of(&self, route: &str) -> Option<Auth> {
        self.registered.get(route).copied()
    }

    /// Whether `assumed` behaviour is refused. Never takes the lock, so
    /// handlers may ask while holding it.
    pub(crate) fn refuse_assumed(&self) -> bool {
        self.strict.load(Ordering::Relaxed)
    }

    /// Whether behaviour `id` may be used. Only refuse-assumed mode says no,
    /// for an `assumed` entry (or one missing from the manifest).
    pub(crate) fn allows(&self, id: &str) -> bool {
        !self.refuse_assumed() || self.manifest.behaviour(id).is_some_and(|f| !f.is_assumed())
    }

    /// The enforcement point for handlers: the response to send instead
    /// when behaviour `id` is `assumed` and assumptions are refused.
    pub(crate) fn refuse(&self, id: &str) -> Option<Response> {
        (!self.allows(id)).then(|| refusal("behaviour", id, self.manifest.behaviour(id)))
    }

    /// An error `message` whose text is behaviour `id`: the text, or
    /// [`WITHHELD`] when that is `assumed` and assumptions are refused. For
    /// errors whose status is documented but whose wording is a guess.
    pub(crate) fn message<'a>(&self, id: &str, text: &'a str) -> Cow<'a, str> {
        if self.allows(id) {
            Cow::Borrowed(text)
        } else {
            Cow::Borrowed(WITHHELD)
        }
    }

    /// The route-level enforcement point, run before every registered route.
    pub(crate) fn refuse_route(&self, route: &str) -> Option<Response> {
        let entry = self.manifest.route(route);
        (self.refuse_assumed() && entry.is_none_or(Fidelity::is_assumed))
            .then(|| refusal("route", route, entry))
    }

    pub(crate) fn record(&self, call: Call) {
        self.state().calls.record(call);
    }

    pub(crate) fn merge(&self, more: Seed) -> Result<(), SeedError> {
        let mut state = self.state();
        state.world = state.world.merged(more)?;
        self.strict
            .store(state.world.settings.refuse_assumed, Ordering::Relaxed);
        Ok(())
    }

    /// `GET /__octoemu/state`: the world, secrets redacted.
    pub(crate) fn dump(&self) -> Value {
        let state = self.state();
        let mut world = serde_json::to_value(&state.world).unwrap_or(Value::Null);
        redact_secrets(&mut world);
        json!({
            "now": state.clock.now(),
            "clock": {"advanced_seconds": state.clock.advanced_seconds()},
            "world": world,
            // Redacted IDs and scopes only, never the tokens.
            "tokens": state.tokens.records(),
            "user_tokens": state.user_tokens.records(),
            "refresh_tokens": state.user_tokens.refresh_records(),
            "oauth": state.oauth.view(&state.world),
            "calls": state.calls.calls().len(),
            "events": state.events.events().len(),
            "deliveries": state.deliveries.attempts().count(),
        })
    }
}

const REDACTED: &str = "<redacted>";

/// Replaces App client secrets and webhook secrets.
fn redact_secrets(world: &mut Value) {
    let Some(apps) = world.get_mut("apps").and_then(Value::as_array_mut) else {
        return;
    };
    for app in apps {
        if let Some(secret) = app.get_mut("client_secret") {
            *secret = REDACTED.into();
        }
        if let Some(secrets) = app
            .get_mut("webhook")
            .and_then(|webhook| webhook.get_mut("secrets"))
            .and_then(Value::as_array_mut)
        {
            for secret in secrets {
                *secret = REDACTED.into();
            }
        }
    }
}

/// The response for refused `assumed` behaviour: a 501 that names it, so a
/// test resting on a guess fails loudly.
fn refusal(kind: &str, id: &str, entry: Option<&Fidelity>) -> Response {
    let reason = match entry {
        Some(Fidelity::Assumed { reason }) => reason.as_str(),
        _ => "no fidelity.toml entry",
    };
    json(
        StatusCode::NOT_IMPLEMENTED,
        &json!({
            "message": "octoemu: refusing assumed behaviour",
            kind: id,
            "reason": reason,
            "documentation_url": OCTOEMU_DOCS,
        }),
    )
}

/// Request activity, for the idle timeout.
#[derive(Debug)]
pub(crate) struct Activity {
    last: Mutex<Instant>,
    in_flight: AtomicUsize,
}

impl Activity {
    fn new() -> Self {
        Self {
            last: Mutex::new(Instant::now()),
            in_flight: AtomicUsize::new(0),
        }
    }

    fn touch(&self) {
        *self.last.lock().unwrap_or_else(PoisonError::into_inner) = Instant::now();
    }

    /// Marks a request in flight until the guard drops.
    pub(crate) fn enter(&self) -> ActivityGuard<'_> {
        self.in_flight.fetch_add(1, Ordering::SeqCst);
        self.touch();
        ActivityGuard(self)
    }

    /// How long since the last request finished; `None` while one is in
    /// flight.
    pub(crate) fn idle_for(&self) -> Option<Duration> {
        if self.in_flight.load(Ordering::SeqCst) > 0 {
            return None;
        }
        Some(
            self.last
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .elapsed(),
        )
    }
}

pub(crate) struct ActivityGuard<'a>(&'a Activity);

impl Drop for ActivityGuard<'_> {
    fn drop(&mut self) {
        self.0.touch();
        self.0.in_flight.fetch_sub(1, Ordering::SeqCst);
    }
}
