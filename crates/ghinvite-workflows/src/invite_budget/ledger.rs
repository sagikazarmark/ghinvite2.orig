//! Pure write-budget accounting. No clock, HTTP, sleeps or invitation policy.
//!
//! Timestamps are Unix seconds, with half-open windows `(now-window, now]`.
//! A monotonic watermark fences caller clock rollback. Snapshot prunes through a
//! private mutex, preserving `Send + Sync` while retaining the specified `&self`
//! API. The mutex is never held across an await. Serialized state contains only
//! configuration, timestamps and caller-supplied non-secret attribution/keys.
//!
//! Moved verbatim from gh-toolkit's `governor` (GM-07). [`WriteLedger`] is
//! persisted in the InviteBudget object's Restate state, so its serde shape
//! must not change (PD-9; pinned by `golden_write_ledger`). The parts
//! ghinvite does not call yet (holds, snapshots, `SlidingWindow`) are kept
//! with their tests rather than trimmed.
#![allow(dead_code)]

/// Unix seconds supplied by the caller; never a floating-point identifier.
pub type UnixTime = i64;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Mutex, MutexGuard},
};

/// Documented provider ceilings used as accounting inputs; applications still choose bucket scope and policy.
pub mod caps {
    /// Documented content-creating request ceiling per minute.
    pub const CONTENT_WRITES_PER_MIN: u32 = 80;
    /// Documented content-creating request ceiling per hour.
    pub const CONTENT_WRITES_PER_HOUR: u32 = 500;
    /// Recommended minimum spacing between content-creating requests, in seconds.
    pub const MIN_WRITE_SPACING_SECS: u32 = 1;
    /// Documented REST secondary-limit points per minute.
    pub const REST_POINTS_PER_MIN: u32 = 900;
    /// Documented GraphQL secondary-limit points per minute.
    pub const GRAPHQL_POINTS_PER_MIN: u32 = 2_000;
    /// Documented maximum concurrent requests across REST and GraphQL.
    pub const MAX_CONCURRENT: u32 = 100;
    /// Documented authenticated search requests per minute.
    pub const SEARCH_PER_MIN: u32 = 30;
    /// Maximum results exposed by one GitHub search query; larger totals require explicit truncation handling.
    pub const SEARCH_MAX_RESULTS: u32 = 1_000;
    /// Daily organization invitation ceiling for the new-organization category.
    pub const ORG_INVITES_PER_DAY_NEW: u32 = 50;
    /// Daily organization invitation ceiling for the established-organization category.
    pub const ORG_INVITES_PER_DAY_ESTABLISHED: u32 = 500;
    /// Daily repository invitation ceiling used by the provider-specific application policy.
    pub const REPO_INVITES_PER_DAY: u32 = 50;
    /// Hourly OAuth token-request ceiling per app/client identity.
    pub const OAUTH_TOKEN_REQUESTS_PER_HOUR: u32 = 2_000;
}
const RECEIPT_SECS: i64 = 86_400;
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
/// Application-selected nonsecret budget identity; it must not contain a credential or mailbox.
pub struct BucketId(String);
impl BucketId {
    /// Construct a bucket identity without imposing application-specific naming policy.
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }
    /// Borrow the caller-provided bucket identity for storage or attribution.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
/// Nonsecret attribution attached to a reservation, separate from its idempotency key.
pub struct ReservationTag(String);
impl ReservationTag {
    /// Construct an attribution tag; callers must exclude credentials and personal data.
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }
    /// Borrow the attribution value retained with the reservation.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
/// Opaque identity of a granted write reservation; reuse it for exact release or refund operations.
pub struct SlotId(u64);
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
/// Opaque identity and expiry of a provisional hold.
pub struct HoldId {
    id: u64,
    expires_at: UnixTime,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
/// All rolling windows and spacing constraints enforced for one bucket.
pub struct BucketSpec {
    /// Exact bucket identity to which these limits apply.
    pub id: BucketId,
    /// Pairs of maximum count and rolling-window seconds; every configured window must permit a reservation.
    pub windows: Vec<(u32, u32)>,
    /// Minimum spacing between granted writes in this bucket.
    pub min_spacing_secs: u32,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
/// Outcome of an idempotent reservation across all requested buckets.
pub enum Decision {
    /// All requested bucket constraints permitted the reservation.
    Granted {
        /// Opaque slot identifying the exact charge set.
        slot: SlotId,
    },
    /// Earliest known retry time; it is guidance, not a preallocated future reservation.
    RetryAt(UnixTime),
    /// A requested bucket has no configured policy; no partial reservation is granted.
    UnknownBucket(BucketId),
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
/// Outcome of reserving provisional capacity before an external decision.
pub enum HoldDecision {
    /// Capacity was held across the full requested bucket set.
    Held {
        /// Opaque hold identity used for exact commit or release.
        hold: HoldId,
        /// Absolute expiry after which the provisional hold cannot be committed.
        expires_at: UnixTime,
    },
    /// Earliest known time when the hold might fit the configured limits.
    RetryAt(UnixTime),
    /// A requested bucket has no configured policy; no partial hold is retained.
    UnknownBucket(BucketId),
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, thiserror::Error)]
/// Failures when committing a provisional hold.
pub enum LedgerError {
    #[error("hold expired")]
    /// The hold passed its expiry and cannot create a new charge.
    Expired,
    #[error("unknown hold")]
    /// No retained hold or commit receipt matches this identity.
    UnknownHold,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, thiserror::Error)]
/// Failures when refunding a subset of an existing reservation.
pub enum RefundError {
    #[error("unknown slot")]
    /// The slot has no retained charge to refund.
    UnknownSlot,
    #[error("invalid refund subset")]
    /// The requested refund includes a bucket outside the original charged set.
    InvalidSubset,
}

impl LedgerError {
    /// Stable error code suitable for metrics and journals; no diagnostic values.
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Expired => "gh:ledger:expired",
            Self::UnknownHold => "gh:ledger:unknown_hold",
        }
    }
}

impl RefundError {
    /// Stable error code suitable for metrics and journals; no diagnostic values.
    pub const fn code(&self) -> &'static str {
        match self {
            Self::UnknownSlot => "gh:refund:unknown_slot",
            Self::InvalidSubset => "gh:refund:invalid_subset",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Bucket {
    spec: BucketSpec,
    closed_until: Option<UnixTime>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Charge {
    #[serde(default)]
    refunded: BTreeSet<BucketId>,
    buckets: BTreeSet<BucketId>,
    tag: Option<ReservationTag>,
    at: UnixTime,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Held {
    charge: Charge,
    expires_at: UnixTime,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Receipt<T> {
    at: UnixTime,
    value: T,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct State {
    buckets: BTreeMap<BucketId, Bucket>,
    slots: BTreeMap<SlotId, Charge>,
    holds: BTreeMap<u64, Held>,
    reservations: BTreeMap<String, Receipt<Decision>>,
    hold_keys: BTreeMap<String, Receipt<HoldDecision>>,
    commits: BTreeMap<u64, Receipt<SlotId>>,
    watermark: Option<UnixTime>,
    next_id: u64,
}
#[derive(Debug, Default, Serialize, Deserialize)]
/// Serializable pure reservation ledger with rolling windows, provisional holds and exact replay receipts.
pub struct WriteLedger {
    state: Mutex<State>,
}
impl Clone for WriteLedger {
    fn clone(&self) -> Self {
        Self {
            state: Mutex::new(self.lock().clone()),
        }
    }
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
/// Current accounting view for one configured bucket.
pub struct BucketSnapshot {
    /// Limits currently enforced for this bucket.
    pub spec: BucketSpec,
    /// Retained granted charges still relevant to this bucket's longest accounting window or spacing bound.
    pub granted: u32,
    /// Unexpired provisional holds charging this bucket.
    pub held: u32,
    /// Explicit closure deadline when one remains active.
    pub closed_until: Option<UnixTime>,
    /// Earliest known retry time under all current bucket constraints.
    pub retry_at: Option<UnixTime>,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
/// Accounting snapshot evaluated at a rollback-fenced logical time.
pub struct LedgerSnapshot {
    /// Effective evaluation time, never earlier than the retained monotonic watermark.
    pub at: UnixTime,
    /// Snapshots of every configured bucket.
    pub buckets: Vec<BucketSnapshot>,
    /// Number of active provisional holds across the ledger.
    pub holds: usize,
    /// Reserve/hold keys plus implicit commit replay receipts (24h retention).
    pub request_keys: usize,
}

fn add(at: UnixTime, seconds: u32) -> UnixTime {
    at.saturating_add(i64::from(seconds))
}
fn active(at: UnixTime, window: u32, now: UnixTime) -> bool {
    // Overflow means the reservation cannot safely age out at representable time.
    at.checked_add(i64::from(window))
        .is_none_or(|end| end > now)
}
fn longest(spec: &BucketSpec) -> u32 {
    spec.windows
        .iter()
        .map(|(_, w)| *w)
        .max()
        .unwrap_or(0)
        .max(spec.min_spacing_secs)
}
fn count(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}
impl State {
    fn gc(&mut self, supplied: UnixTime) -> UnixTime {
        let now = self.watermark.map_or(supplied, |last| last.max(supplied));
        self.watermark = Some(now);
        self.holds.retain(|_, hold| hold.expires_at > now);
        for slot in self.slots.values_mut() {
            slot.buckets.retain(|id| {
                self.buckets
                    .get(id)
                    .is_some_and(|b| active(slot.at, longest(&b.spec), now))
            });
        }
        self.slots.retain(|_, slot| !slot.buckets.is_empty());
        for bucket in self.buckets.values_mut() {
            if bucket.closed_until.is_some_and(|until| until <= now) {
                bucket.closed_until = None;
            }
        }
        let recent = |at: i64| at.checked_add(RECEIPT_SECS).is_none_or(|end| end > now);
        self.reservations.retain(|_, r| recent(r.at));
        self.hold_keys.retain(|_, r| recent(r.at));
        self.commits.retain(|_, r| recent(r.at));
        now
    }
    fn retry(&self, id: &BucketId, now: UnixTime) -> Option<UnixTime> {
        let bucket = self.buckets.get(id)?;
        let spec = &bucket.spec;
        if spec.windows.is_empty() || spec.windows.iter().any(|(cap, w)| *cap == 0 || *w == 0) {
            return Some(i64::MAX);
        }
        let mut until = bucket.closed_until.filter(|t| *t > now);
        for (cap, window) in &spec.windows {
            let mut expiries: Vec<_> = self
                .slots
                .values()
                .filter(|s| s.buckets.contains(id) && active(s.at, *window, now))
                .map(|s| add(s.at, *window))
                .chain(
                    self.holds
                        .values()
                        .filter(|h| h.charge.buckets.contains(id))
                        .map(|h| h.expires_at),
                )
                .collect();
            if expiries.len() >= *cap as usize {
                expiries.sort_unstable();
                let next = expiries[expiries.len() - *cap as usize];
                until = Some(until.map_or(next, |prior| prior.max(next)));
            }
        }
        if spec.min_spacing_secs > 0 {
            for slot in self.slots.values().filter(|s| s.buckets.contains(id)) {
                if active(slot.at, spec.min_spacing_secs, now) {
                    let next = add(slot.at, spec.min_spacing_secs);
                    until = Some(until.map_or(next, |prior| prior.max(next)));
                }
            }
            // An uncommitted write may happen at any time before expiry. Keep
            // spacing locked until then; commit starts the actual write window.
            for hold in self
                .holds
                .values()
                .filter(|h| h.charge.buckets.contains(id))
            {
                until = Some(until.map_or(hold.expires_at, |prior| prior.max(hold.expires_at)));
            }
        }
        until
    }
    fn check(&self, ids: &[BucketId], now: UnixTime) -> Result<(), Decision> {
        for id in ids {
            if !self.buckets.contains_key(id) {
                return Err(Decision::UnknownBucket(id.clone()));
            }
        }
        if let Some(until) = ids.iter().filter_map(|id| self.retry(id, now)).max() {
            Err(Decision::RetryAt(until))
        } else if self.next_id == u64::MAX {
            Err(Decision::RetryAt(i64::MAX))
        } else {
            Ok(())
        }
    }
    fn id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }
}
impl WriteLedger {
    /// Create an empty ledger with no implicitly configured budgets.
    pub fn new() -> Self {
        Self::default()
    }
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }
    /// Invalid/zero-cap specs fail closed. Changing caps keeps existing charges.
    pub fn upsert_bucket(&mut self, spec: BucketSpec) {
        let mut s = self.lock();
        let closed_until = s.buckets.get(&spec.id).and_then(|b| b.closed_until);
        s.buckets
            .insert(spec.id.clone(), Bucket { spec, closed_until });
    }
    /// Remove a bucket's configuration and its associated accounting references.
    pub fn remove_bucket(&mut self, id: &BucketId) {
        let mut s = self.lock();
        s.buckets.remove(id);
        for slot in s.slots.values_mut() {
            slot.buckets.remove(id);
        }
        s.slots.retain(|_, slot| !slot.buckets.is_empty());
        for hold in s.holds.values_mut() {
            hold.charge.buckets.remove(id);
        }
    }
    /// Request keys are opaque, non-secret receipt identities. An exact key
    /// replay returns even a prior refusal; use a new attempt key to reconsider.
    pub fn reserve(
        &mut self,
        buckets: &[BucketId],
        tag: Option<&ReservationTag>,
        request_key: Option<&str>,
        now: UnixTime,
    ) -> Decision {
        let mut s = self.lock();
        let now = s.gc(now);
        if let Some(r) = request_key.and_then(|key| s.reservations.get(key)) {
            return r.value.clone();
        }
        let value = match s.check(buckets, now) {
            Err(refusal) => refusal,
            Ok(()) => {
                let slot = SlotId(s.id());
                s.slots.insert(
                    slot,
                    Charge {
                        refunded: BTreeSet::new(),
                        buckets: buckets.iter().cloned().collect(),
                        tag: tag.cloned(),
                        at: now,
                    },
                );
                Decision::Granted { slot }
            }
        };
        if let Some(key) = request_key {
            s.reservations.insert(
                key.to_owned(),
                Receipt {
                    at: now,
                    value: value.clone(),
                },
            );
        }
        value
    }
    /// Release an unused slot; callers must positively know no external effect occurred.
    pub fn release(&mut self, slot: SlotId) {
        self.lock().slots.remove(&slot);
    }
    /// Atomically refund a nonempty subset of this slot's charged buckets.
    /// Validate the entire subset before mutation; a foreign bucket refuses the
    /// whole operation. Repeated members/refunds are idempotent. Other charges,
    /// timestamps, spacing and reservation/commit receipts remain unchanged.
    /// The caller supplies the evidence and policy authorizing the refund.
    pub fn refund_buckets(
        &mut self,
        slot: SlotId,
        buckets: &[BucketId],
    ) -> Result<(), RefundError> {
        let mut state = self.lock();
        let charge = state.slots.get_mut(&slot).ok_or(RefundError::UnknownSlot)?;
        if buckets.is_empty()
            || buckets
                .iter()
                .any(|id| !charge.buckets.contains(id) && !charge.refunded.contains(id))
        {
            return Err(RefundError::InvalidSubset);
        }
        for id in buckets {
            charge.buckets.remove(id);
            charge.refunded.insert(id.clone());
        }
        Ok(())
    }
    /// Hold and reserve have separate request-key namespaces. TTL is capped at
    /// 300 seconds; zero TTL produces an immediately expired, uncommittable hold.
    pub fn hold(
        &mut self,
        buckets: &[BucketId],
        tag: Option<&ReservationTag>,
        request_key: Option<&str>,
        ttl_secs: u32,
        now: UnixTime,
    ) -> HoldDecision {
        let mut s = self.lock();
        let now = s.gc(now);
        if let Some(r) = request_key.and_then(|key| s.hold_keys.get(key)) {
            return r.value.clone();
        }
        let value = match s.check(buckets, now) {
            Err(Decision::UnknownBucket(id)) => HoldDecision::UnknownBucket(id),
            Err(Decision::RetryAt(at)) => HoldDecision::RetryAt(at),
            Err(Decision::Granted { .. }) => unreachable!(),
            Ok(()) => {
                let expires_at = add(now, ttl_secs.min(300));
                let hold = HoldId {
                    id: s.id(),
                    expires_at,
                };
                s.holds.insert(
                    hold.id,
                    Held {
                        charge: Charge {
                            refunded: BTreeSet::new(),
                            buckets: buckets.iter().cloned().collect(),
                            tag: tag.cloned(),
                            at: now,
                        },
                        expires_at,
                    },
                );
                HoldDecision::Held { hold, expires_at }
            }
        };
        if let Some(key) = request_key {
            s.hold_keys.insert(
                key.to_owned(),
                Receipt {
                    at: now,
                    value: value.clone(),
                },
            );
        }
        value
    }
    /// Commit is idempotent for 24h. Charge from commit time so a delayed write
    /// cannot fall outside its budget window immediately after it happens.
    pub fn commit(&mut self, hold: HoldId, now: UnixTime) -> Result<SlotId, LedgerError> {
        let mut s = self.lock();
        let now = s.gc(now);
        if let Some(receipt) = s.commits.get(&hold.id) {
            return Ok(receipt.value);
        }
        if hold.expires_at <= now {
            return Err(LedgerError::Expired);
        }
        if s.holds
            .get(&hold.id)
            .is_none_or(|h| h.expires_at != hold.expires_at)
        {
            return Err(LedgerError::UnknownHold);
        }
        let mut held = s.holds.remove(&hold.id).ok_or(LedgerError::UnknownHold)?;
        held.charge.at = now;
        let slot = SlotId(hold.id);
        s.slots.insert(slot, held.charge);
        s.commits.insert(
            hold.id,
            Receipt {
                at: now,
                value: slot,
            },
        );
        Ok(slot)
    }
    /// Release the exact provisional hold without turning it into a granted charge.
    pub fn release_hold(&mut self, hold: HoldId) {
        let mut s = self.lock();
        if s.holds
            .get(&hold.id)
            .is_some_and(|h| h.expires_at == hold.expires_at)
        {
            s.holds.remove(&hold.id);
        }
    }
    /// Closures are monotonic: a later stale observation cannot shorten one.
    pub fn record_limit(&mut self, bucket: &BucketId, until: UnixTime) {
        if let Some(b) = self.lock().buckets.get_mut(bucket) {
            b.closed_until = Some(b.closed_until.map_or(until, |prior| prior.max(until)));
        }
    }
    /// Count attributed charges and active holds for this bucket at the rollback-fenced time.
    pub fn count_tagged(&self, bucket: &BucketId, tag: &ReservationTag, now: UnixTime) -> u32 {
        let mut s = self.lock();
        let now = s.gc(now);
        let Some(b) = s.buckets.get(bucket) else {
            return 0;
        };
        let window = b.spec.windows.iter().map(|(_, w)| *w).max().unwrap_or(0);
        count(
            s.slots
                .values()
                .filter(|c| {
                    c.buckets.contains(bucket)
                        && c.tag.as_ref() == Some(tag)
                        && active(c.at, window, now)
                })
                .count()
                + s.holds
                    .values()
                    .filter(|h| {
                        h.charge.buckets.contains(bucket)
                            && h.charge.tag.as_ref() == Some(tag)
                            && active(h.charge.at, window, now)
                    })
                    .count(),
        )
    }
    /// Prune expired accounting entries and return the current view without any external clock or I/O.
    pub fn snapshot(&self, now: UnixTime) -> LedgerSnapshot {
        let mut s = self.lock();
        let now = s.gc(now);
        LedgerSnapshot {
            at: now,
            holds: s.holds.len(),
            request_keys: s.reservations.len() + s.hold_keys.len() + s.commits.len(),
            buckets: s
                .buckets
                .iter()
                .map(|(id, b)| BucketSnapshot {
                    spec: b.spec.clone(),
                    granted: count(s.slots.values().filter(|c| c.buckets.contains(id)).count()),
                    held: count(
                        s.holds
                            .values()
                            .filter(|h| h.charge.buckets.contains(id))
                            .count(),
                    ),
                    closed_until: b.closed_until,
                    retry_at: s.retry(id, now),
                })
                .collect(),
        }
    }
}

/// Standalone bounded rolling counter; duplicates at the same timestamp are
/// separate events. Configure the longest window and largest cap to preserve
/// enough history for any smaller cap/window evaluated with `retry_at`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SlidingWindow {
    timestamps: Vec<UnixTime>,
    max_cap: u32,
    window_secs: u32,
    watermark: Option<UnixTime>,
}
impl SlidingWindow {
    /// Configure bounded timestamp retention for the largest cap and longest window this counter will evaluate.
    pub fn new(max_cap: u32, window_secs: u32) -> Self {
        Self {
            timestamps: Vec::new(),
            max_cap,
            window_secs,
            watermark: None,
        }
    }
    /// Remove events outside the retention window while preventing caller clock rollback.
    pub fn prune(&mut self, now: UnixTime) {
        let now = self.watermark.map_or(now, |last| last.max(now));
        self.watermark = Some(now);
        self.timestamps
            .retain(|t| active(*t, self.window_secs, now));
    }
    /// Return the earliest retry time for a supported cap/window; unsupported bounds fail closed.
    pub fn retry_at(&mut self, cap: u32, window_secs: u32, now: UnixTime) -> Option<UnixTime> {
        self.prune(now);
        let now = self.watermark.unwrap_or(now);
        if cap == 0 || cap > self.max_cap || window_secs == 0 || window_secs > self.window_secs {
            return Some(i64::MAX);
        }
        let times: Vec<_> = self
            .timestamps
            .iter()
            .filter(|t| active(**t, window_secs, now))
            .collect();
        (times.len() >= cap as usize).then(|| add(*times[times.len() - cap as usize], window_secs))
    }
    /// Record one event if capacity remains, otherwise return the earliest retry time.
    pub fn record(&mut self, now: UnixTime) -> Result<(), UnixTime> {
        if let Some(until) = self.retry_at(self.max_cap, self.window_secs, now) {
            return Err(until);
        }
        self.timestamps.push(self.watermark.unwrap_or(now));
        Ok(())
    }
    /// Return the number of retained events; pruning requires an explicit evaluation time.
    pub fn len(&self) -> usize {
        self.timestamps.len()
    }
    /// Whether no event timestamps are currently retained.
    pub fn is_empty(&self) -> bool {
        self.timestamps.is_empty()
    }
}
/// Build separate installation and optional user content-write budgets, deduplicating identical bucket identities.
pub fn content_write_buckets(installation: BucketId, user: Option<BucketId>) -> Vec<BucketSpec> {
    std::iter::once(installation)
        .chain(user)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .map(|id| BucketSpec {
            id,
            windows: vec![
                (caps::CONTENT_WRITES_PER_MIN, 60),
                (caps::CONTENT_WRITES_PER_HOUR, 3600),
            ],
            min_spacing_secs: caps::MIN_WRITE_SPACING_SECS,
        })
        .collect()
}
#[cfg(test)]
mod tests;
