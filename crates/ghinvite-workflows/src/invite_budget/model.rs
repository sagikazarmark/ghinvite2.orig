use super::ledger::{self, BucketSpec, Decision, ReservationTag, WriteLedger};
use super::*;
use ghinvite_core::budget::{
    BucketId, BucketSnapshot, BudgetKind, Reservation, RetryReason, SlotId,
};
use std::collections::{BTreeMap, BTreeSet};
const DAY: i64 = 86_400;
pub(super) const MAX_LIMIT_OBSERVATIONS: usize = 10_000;
fn date(at: i64) -> DateTime<Utc> {
    DateTime::from_timestamp(at, 0).unwrap_or(DateTime::<Utc>::MAX_UTC)
}
fn key(id: &BucketId) -> ledger::BucketId {
    ledger::BucketId::new(id.key())
}
fn invalid() -> TerminalError {
    TerminalError::new_with_code(400, "invalid budget command")
}
fn conflict() -> TerminalError {
    TerminalError::new_with_code(409, "budget operation identity conflict")
}
fn valid_key(key: &str) -> bool {
    !key.is_empty() && key.len() <= 256 && key.bytes().all(|b| b.is_ascii_graphic())
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Receipt<T> {
    at: i64,
    input: T,
    result: Reservation,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct PolicyReceipt {
    at: i64,
    input: Configure,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Slot {
    ledger_slot: ledger::SlotId,
    #[serde(default)]
    invitation_refunded: bool,
    at: i64,
    bucket: Option<BucketId>,
    link_id: Option<ghinvite_core::InvitationLinkId>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(super) struct State {
    #[serde(default)]
    limit_observations: BTreeMap<String, RecordLimit>,
    #[serde(default)]
    limit_coverage_started_at: Option<i64>,
    #[serde(default)]
    limit_overflow_until: Option<i64>,
    #[serde(default)]
    unclassified_limit_until: Option<i64>,
    ledger: WriteLedger,
    revision: u64,
    next_slot: u64,
    now: Option<i64>,
    confirmed_established: bool,
    #[serde(default)]
    cap_policy: Option<ghinvite_core::budget::OrgCapPolicy>,
    shares: BTreeMap<String, u8>,
    #[serde(default)]
    link_policy_bound: bool,
    receipts: BTreeMap<String, Receipt<Reserve>>,
    policy_receipts: BTreeMap<String, PolicyReceipt>,
    slots: BTreeMap<SlotId, Slot>,
    buckets: BTreeSet<BucketId>,
}
impl State {
    pub(super) fn prepare(&mut self, owner: AccountInstallationRef, now: DateTime<Utc>) {
        let now = self
            .now
            .map_or(now.timestamp(), |prior| prior.max(now.timestamp()));
        self.now = Some(now);
        self.limit_coverage_started_at.get_or_insert(now);
        self.limit_observations.retain(|_, limit| {
            limit
                .observation
                .as_ref()
                .is_some_and(|event| event.observed_at.timestamp().saturating_add(DAY) > now)
        });
        self.receipts.retain(|_, r| r.at.saturating_add(DAY) > now);
        self.policy_receipts
            .retain(|_, r| r.at.saturating_add(DAY) > now);
        self.slots.retain(|_, s| s.at.saturating_add(DAY) > now);
        self.ledger.snapshot(now);
        self.ledger
            .upsert_bucket(ledger::content_write_buckets(key(&BucketId::Content), None).remove(0));
        if owner.app == App::Org {
            self.ensure_bucket(BucketId::Org);
        }
        // Per-repository families are app configuration created on demand. Drop
        // idle ones after their full daily window, including inactive closures.
        let view = self.ledger.snapshot(now);
        let idle: Vec<_> = self
            .buckets
            .iter()
            .filter(|id| matches!(id, BucketId::Repo { .. }))
            .filter(|id| {
                view.buckets.iter().any(|b| {
                    b.spec.id == key(id)
                        && b.granted == 0
                        && b.held == 0
                        && b.closed_until.is_none()
                })
            })
            .cloned()
            .collect();
        for id in idle {
            self.buckets.remove(&id);
            self.ledger.remove_bucket(&key(&id));
        }
    }
    fn ensure_bucket(&mut self, id: BucketId) {
        let cap = match id {
            BucketId::Org if self.confirmed_established => 500,
            _ => 50,
        };
        self.ledger.upsert_bucket(BucketSpec {
            id: key(&id),
            windows: vec![(cap, 86400)],
            min_spacing_secs: 0,
        });
        self.buckets.insert(id);
    }
    pub(super) fn set_org_cap(
        &mut self,
        owner: AccountInstallationRef,
        policy: ghinvite_core::budget::OrgCapPolicy,
    ) -> Result<(), TerminalError> {
        if owner.app != App::Org
            || policy.account_id != owner.account_id
            || policy.revision == 0
            || policy.revision > ghinvite_core::storage::integer::MAX_EXACT
            || policy.command.actor_user_id == 0
            || policy.command.actor_user_id > ghinvite_core::storage::integer::MAX_EXACT
        {
            return Err(invalid());
        }
        if let Some(current) = &self.cap_policy {
            if policy.revision < current.revision {
                return Ok(());
            }
            if policy.revision == current.revision {
                return if policy == *current {
                    Ok(())
                } else {
                    Err(conflict())
                };
            }
        }
        self.confirmed_established = policy.command.confirmed_established;
        self.cap_policy = Some(policy);
        self.ensure_bucket(BucketId::Org);
        Ok(())
    }
    pub(super) fn set_link_share(
        &mut self,
        input: ghinvite_core::budget::LinkShare,
    ) -> Result<(), TerminalError> {
        if !(1..=100).contains(&input.percent) {
            return Err(invalid());
        }
        let key = input.link_id.to_string();
        if let Some(percent) = self.shares.get(&key) {
            if *percent != input.percent {
                return Err(conflict());
            }
        } else {
            self.shares.insert(key, input.percent);
        }
        self.link_policy_bound = true;
        Ok(())
    }
    pub(super) fn configure(
        &mut self,
        owner: AccountInstallationRef,
        input: Configure,
    ) -> Result<(), TerminalError> {
        if !valid_key(&input.request_key)
            || (owner.app != App::Org && input.confirmed_established)
            || input.link_shares.len() > 5000
            || input
                .link_shares
                .iter()
                .any(|s| !(1..=100).contains(&s.percent))
        {
            return Err(invalid());
        }
        let shares: BTreeMap<_, _> = input
            .link_shares
            .iter()
            .map(|s| (s.link_id.to_string(), s.percent))
            .collect();
        if shares.len() != input.link_shares.len() {
            return Err(invalid());
        }
        if let Some(prior) = self.policy_receipts.get(&input.request_key) {
            return if prior.input == input {
                Ok(())
            } else {
                Err(conflict())
            };
        }
        // The old bulk policy command is retained for isolated fixtures. Once
        // authenticated cap policy exists it cannot replace that authority.
        if self.cap_policy.is_some() {
            return Err(conflict());
        }
        if self.link_policy_bound {
            return Err(conflict());
        }
        self.confirmed_established = input.confirmed_established;
        self.shares = shares;
        if owner.app == App::Org {
            self.ensure_bucket(BucketId::Org);
        }
        self.policy_receipts.insert(
            input.request_key.clone(),
            PolicyReceipt {
                at: self.now.unwrap_or(0),
                input,
            },
        );
        Ok(())
    }
    pub(super) fn reserve(
        &mut self,
        owner: AccountInstallationRef,
        input: Reserve,
    ) -> Result<Reservation, TerminalError> {
        if !valid_key(&input.request_key) {
            return Err(invalid());
        }
        if let Some(prior) = self.receipts.get(&input.request_key) {
            return if prior.input == input {
                Ok(prior.result.clone())
            } else {
                Err(conflict())
            };
        }
        let now = self.now.unwrap_or(0);
        let daily = match input.kind {
            BudgetKind::OrgInvite => Some(BucketId::Org),
            BudgetKind::RepoInvite { repo_id } => Some(BucketId::Repo { repo_id }),
            BudgetKind::OtherWrite => None,
        };
        if daily.as_ref().is_some_and(|b| !b.valid_for(owner.app)) {
            return Err(invalid());
        }
        if let Some(bucket) = &daily {
            self.ensure_bucket(bucket.clone());
        }
        let mut buckets = vec![key(&BucketId::Content)];
        if let Some(b) = &daily {
            buckets.push(key(b));
        }
        let tag = input
            .link_id
            .map(|link| ReservationTag::new(link.to_string()));
        let mut refusal = None;
        if let (Some(bucket), Some(tag), Some(link)) = (&daily, &tag, input.link_id) {
            let cap = if *bucket == BucketId::Org && self.confirmed_established {
                500
            } else {
                50
            };
            let pct = u32::from(*self.shares.get(&link.to_string()).unwrap_or(&50));
            let share = (cap * pct / 100).max(1);
            if self.ledger.count_tagged(&key(bucket), tag, now) >= share {
                let mut times: Vec<_> = self
                    .slots
                    .values()
                    .filter(|s| {
                        !s.invitation_refunded
                            && s.bucket.as_ref() == Some(bucket)
                            && s.link_id == Some(link)
                    })
                    .map(|s| s.at)
                    .collect();
                times.sort_unstable();
                let expiry = times
                    .get(times.len().saturating_sub(share as usize))
                    .copied()
                    .unwrap_or(now)
                    .saturating_add(DAY);
                refusal = Some(Reservation::RetryAt {
                    at: date(expiry),
                    why: RetryReason::LinkShare,
                });
            }
        }
        let result = if let Some(refusal) = refusal {
            refusal
        } else {
            match self
                .ledger
                .reserve(&buckets, tag.as_ref(), Some(&input.request_key), now)
            {
                Decision::Granted { slot } => {
                    self.next_slot = self
                        .next_slot
                        .checked_add(1)
                        .filter(|n| *n <= ghinvite_core::storage::integer::MAX_EXACT)
                        .ok_or_else(invalid)?;
                    let slot_id = SlotId(self.next_slot);
                    self.slots.insert(
                        slot_id,
                        Slot {
                            ledger_slot: slot,
                            invitation_refunded: false,
                            at: now,
                            bucket: daily,
                            link_id: input.link_id,
                        },
                    );
                    Reservation::Granted { slot_id }
                }
                Decision::RetryAt(at) => {
                    let view = self.ledger.snapshot(now);
                    let closed = view.buckets.iter().any(|b| {
                        buckets.contains(&b.spec.id) && b.closed_until.is_some_and(|t| t > now)
                    });
                    let spacing = self.slots.values().any(|s| s.at == now);
                    Reservation::RetryAt {
                        at: date(at),
                        why: if closed {
                            RetryReason::Closed
                        } else if spacing && at <= now + 1 {
                            RetryReason::Spacing
                        } else {
                            RetryReason::Bucket
                        },
                    }
                }
                Decision::UnknownBucket(_) => return Err(invalid()),
            }
        };
        self.receipts.insert(
            input.request_key.clone(),
            Receipt {
                at: now,
                input,
                result: result.clone(),
            },
        );
        Ok(result)
    }
    /// Pre-fence unused slots only. A known completed write's content charges
    /// cannot be removed by a late/replayed all-bucket release.
    pub(super) fn release(&mut self, slot: SlotId) {
        if self.slots.get(&slot).is_some_and(|s| s.invitation_refunded) {
            return;
        }
        if let Some(slot) = self.slots.remove(&slot) {
            self.ledger.release(slot.ledger_slot);
        }
    }
    pub(super) fn refund_invitation_after_put204(
        &mut self,
        input: RefundInvitationAfterPut204,
    ) -> Result<InvitationRefund, TerminalError> {
        if !valid_key(&input.reservation.request_key) {
            return Err(invalid());
        }
        let Some(receipt) = self.receipts.get(&input.reservation.request_key) else {
            return Ok(InvitationRefund::NotApplicable);
        };
        if receipt.input != input.reservation {
            return Err(conflict());
        }
        let Reservation::Granted { slot_id } = receipt.result else {
            return Ok(InvitationRefund::NotApplicable);
        };
        if slot_id != input.slot_id {
            return Err(conflict());
        }
        // Only repository collaborator PUT204 has this contract. Org invitation
        // creation and OtherWrite cannot supply a qualifying success response.
        if !matches!(input.reservation.kind, BudgetKind::RepoInvite { .. }) {
            return Ok(InvitationRefund::NotApplicable);
        }
        let Some(slot) = self.slots.get_mut(&slot_id) else {
            return Ok(InvitationRefund::NotApplicable);
        };
        if !slot.invitation_refunded {
            let Some(bucket) = &slot.bucket else {
                return Ok(InvitationRefund::NotApplicable);
            };
            self.ledger
                .refund_buckets(slot.ledger_slot, &[key(bucket)])
                .map_err(|_| invalid())?;
            slot.invitation_refunded = true;
        }
        Ok(InvitationRefund::Refunded)
    }
    pub(super) fn record_limit(
        &mut self,
        owner: AccountInstallationRef,
        input: RecordLimit,
    ) -> Result<(), TerminalError> {
        if !input.bucket.valid_for(owner.app) {
            return Err(invalid());
        }
        let now = self.now.unwrap_or(0);
        if let Some(event) = &input.observation {
            let id = event
                .id
                .as_ref()
                .filter(|id| {
                    id.len() == 64
                        && id
                            .bytes()
                            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                })
                .ok_or_else(invalid)?;
            if let Some(previous) = self.limit_observations.get(id) {
                if previous != &input {
                    return Err(conflict());
                }
                return Ok(());
            }
            if event.observed_at.timestamp().saturating_add(DAY) > now {
                if self.limit_observations.len() < MAX_LIMIT_OBSERVATIONS {
                    self.limit_observations.insert(id.clone(), input.clone());
                } else {
                    let expires = event.observed_at.timestamp().saturating_add(DAY);
                    self.limit_overflow_until =
                        Some(self.limit_overflow_until.unwrap_or(0).max(expires));
                }
            }
        } else {
            self.unclassified_limit_until = Some(now.saturating_add(DAY));
        }
        if input.bucket != BucketId::Content {
            self.ensure_bucket(input.bucket.clone());
        }
        let until = input.until.map_or_else(
            || {
                input
                    .observation
                    .as_ref()
                    .map_or(now, |e| e.observed_at.timestamp())
                    .saturating_add(DAY)
            },
            |t| t.timestamp(),
        );
        self.ledger.record_limit(&key(&input.bucket), until);
        Ok(())
    }
    pub(super) fn advance(&mut self) -> Result<(), TerminalError> {
        self.revision = self
            .revision
            .checked_add(1)
            .filter(|n| *n <= ghinvite_core::storage::integer::MAX_EXACT)
            .ok_or_else(invalid)?;
        Ok(())
    }
    pub(super) fn snapshot(&self, owner: AccountInstallationRef) -> BudgetSnapshot {
        let now = self.now.unwrap_or(0);
        let view = self.ledger.snapshot(now);
        let content = view
            .buckets
            .iter()
            .find(|b| b.spec.id == key(&BucketId::Content));
        let mut minute = self.ledger.clone();
        minute.upsert_bucket(BucketSpec {
            id: key(&BucketId::Content),
            windows: vec![(80, 60)],
            min_spacing_secs: 0,
        });
        let minute = minute
            .snapshot(now)
            .buckets
            .into_iter()
            .find(|b| b.spec.id == key(&BucketId::Content))
            .map_or(0, |b| b.granted);
        BudgetSnapshot {
            limit_counts: self.limit_counts(now),
            app: owner.app,
            account_id: owner.account_id,
            revision: self.revision,
            observed_at: date(now),
            confirmed_established: self.confirmed_established,
            buckets: self
                .buckets
                .iter()
                .filter_map(|id| {
                    view.buckets
                        .iter()
                        .find(|b| b.spec.id == key(id))
                        .map(|b| BucketSnapshot {
                            bucket: id.clone(),
                            used_24h: b.granted,
                            cap: b.spec.windows[0].0,
                            closed_until: b.closed_until.map(date),
                            next_slot_at: b
                                .retry_at
                                .into_iter()
                                .chain(content.and_then(|c| c.retry_at))
                                .max()
                                .map(date),
                        })
                })
                .collect(),
            content_used_minute: minute,
            content_used_hour: content.map_or(0, |b| b.granted),
            content_closed_until: content.and_then(|b| b.closed_until).map(date),
            next_content_write_at: content.and_then(|b| b.retry_at).map(date),
        }
    }
    fn limit_counts(&self, now: i64) -> ghinvite_core::budget::LimitCounts {
        use ghinvite_core::budget::{LimitCounts, LimitReason};
        let mut counts = LimitCounts {
            coverage_started_at: self.limit_coverage_started_at.map(date),
            incomplete: self
                .limit_coverage_started_at
                .is_none_or(|at| at > now.saturating_sub(DAY))
                || self.unclassified_limit_until.is_some_and(|at| at > now)
                || self.limit_overflow_until.is_some_and(|at| at > now),
            ..Default::default()
        };
        for event in self
            .limit_observations
            .values()
            .filter_map(|limit| limit.observation.as_ref())
        {
            if event.observed_at.timestamp() > now {
                counts.future_observations += 1;
                counts.incomplete = true;
            } else if event.observed_at.timestamp() > now.saturating_sub(DAY) {
                match event.reason {
                    LimitReason::Spammed => counts.spammed += 1,
                    LimitReason::RateLimited => counts.rate_limited += 1,
                }
            }
        }
        counts
    }
}
