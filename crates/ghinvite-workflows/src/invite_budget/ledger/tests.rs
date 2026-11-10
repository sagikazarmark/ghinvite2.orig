use super::*;
fn id(s: &str) -> BucketId {
    BucketId::new(s)
}
fn configured(cap: u32, window: u32, spacing: u32) -> WriteLedger {
    let mut ledger = WriteLedger::new();
    ledger.upsert_bucket(BucketSpec {
        id: id("a"),
        windows: vec![(cap, window)],
        min_spacing_secs: spacing,
    });
    ledger
}
fn grant(d: Decision) -> SlotId {
    match d {
        Decision::Granted { slot } => slot,
        other => panic!("{other:?}"),
    }
}
fn held(d: HoldDecision) -> HoldId {
    match d {
        HoldDecision::Held { hold, .. } => hold,
        other => panic!("{other:?}"),
    }
}
fn reserve(l: &mut WriteLedger, now: i64) -> Decision {
    l.reserve(&[id("a")], None, None, now)
}

#[test]
fn simultaneous_events_and_exact_window_boundary() {
    let mut l = configured(2, 60, 0);
    assert_ne!(grant(reserve(&mut l, 100)), grant(reserve(&mut l, 100)));
    assert_eq!(reserve(&mut l, 100), Decision::RetryAt(160));
    assert_eq!(reserve(&mut l, 159), Decision::RetryAt(160));
    grant(reserve(&mut l, 160));
    assert_eq!(l.snapshot(160).buckets[0].granted, 1);
}
#[test]
fn nested_windows_choose_latest_required_expiry() {
    let mut l = configured(3, 100, 0);
    l.upsert_bucket(BucketSpec {
        id: id("a"),
        windows: vec![(1, 10), (3, 100)],
        min_spacing_secs: 0,
    });
    for now in [0, 10, 20] {
        grant(reserve(&mut l, now));
    }
    assert_eq!(reserve(&mut l, 21), Decision::RetryAt(100));
    grant(reserve(&mut l, 100));
    assert_eq!(reserve(&mut l, 100), Decision::RetryAt(110));
}
#[test]
// covers: RL-06
fn multi_bucket_denial_is_atomic_and_duplicates_are_one_charge() {
    let mut l = configured(2, 60, 0);
    l.upsert_bucket(BucketSpec {
        id: id("b"),
        windows: vec![(1, 60)],
        min_spacing_secs: 0,
    });
    grant(l.reserve(&[id("b")], None, None, 0));
    assert_eq!(
        l.reserve(&[id("a"), id("b")], None, None, 1),
        Decision::RetryAt(60)
    );
    assert_eq!(l.snapshot(1).buckets[0].granted, 0);
    assert_eq!(
        l.reserve(&[id("a"), id("missing")], None, None, 1),
        Decision::UnknownBucket(id("missing"))
    );
    grant(l.reserve(&[id("a"), id("a")], None, None, 1));
    assert_eq!(l.snapshot(1).buckets[0].granted, 1);
}
#[test]
// covers: RL-07
fn raise_and_lower_caps_keep_every_existing_timestamp() {
    let mut l = configured(1, 60, 0);
    grant(reserve(&mut l, 0));
    assert_eq!(reserve(&mut l, 1), Decision::RetryAt(60));
    l.upsert_bucket(BucketSpec {
        id: id("a"),
        windows: vec![(3, 60)],
        min_spacing_secs: 0,
    });
    grant(reserve(&mut l, 1));
    grant(reserve(&mut l, 2));
    l.upsert_bucket(BucketSpec {
        id: id("a"),
        windows: vec![(1, 60)],
        min_spacing_secs: 0,
    });
    assert_eq!(reserve(&mut l, 3), Decision::RetryAt(62));
    grant(reserve(&mut l, 62));
}
#[test]
fn exact_receipts_survive_release_refusal_cap_change_and_journal_roundtrip() {
    let mut l = configured(1, 60, 0);
    let first = l.reserve(&[id("a")], None, Some("one"), 0);
    let denied = l.reserve(&[id("a")], None, Some("denied"), 1);
    l.release(grant(first.clone()));
    let mut l: WriteLedger = serde_json::from_slice(&serde_json::to_vec(&l).unwrap()).unwrap();
    assert_eq!(l.reserve(&[], None, Some("one"), 2), first);
    assert_eq!(l.reserve(&[id("a")], None, Some("denied"), 100), denied);
    assert_eq!(l.snapshot(100).buckets[0].granted, 0);
    let next = l.reserve(&[id("a")], None, Some("one"), 86_400);
    assert_ne!(next, first);
    grant(next);
    assert!(matches!(
        l.reserve(&[id("a")], None, Some("denied"), 86_401),
        Decision::RetryAt(86_460)
    ));
}
#[test]
fn unknown_bucket_refusal_is_also_retained() {
    let mut l = WriteLedger::new();
    let original = l.reserve(&[id("a")], None, Some("key"), 0);
    l.upsert_bucket(BucketSpec {
        id: id("a"),
        windows: vec![(1, 60)],
        min_spacing_secs: 0,
    });
    assert_eq!(l.reserve(&[id("a")], None, Some("key"), 1), original);
    grant(l.reserve(&[id("a")], None, Some("fresh"), 1));
}
#[test]
// covers: RL-07
fn tagged_share_counts_grants_and_holds_but_admin_still_uses_capacity() {
    let mut l = configured(3, 60, 0);
    let tag = ReservationTag::new("campaign:7");
    let slot = grant(l.reserve(&[id("a")], Some(&tag), None, 0));
    let hold = held(l.hold(&[id("a")], Some(&tag), None, 30, 1));
    assert_eq!(l.count_tagged(&id("a"), &tag, 2), 2);
    grant(reserve(&mut l, 2));
    assert_eq!(l.count_tagged(&id("a"), &tag, 2), 2);
    assert_eq!(reserve(&mut l, 2), Decision::RetryAt(31));
    l.release(slot);
    l.release(slot);
    l.release_hold(hold);
    l.release_hold(hold);
    assert_eq!(l.count_tagged(&id("a"), &tag, 2), 0);
    assert_eq!(l.snapshot(2).buckets[0].granted, 1);
}
#[test]
fn closures_spacing_and_rollback_are_monotonic() {
    let mut l = configured(100, 60, 1);
    grant(reserve(&mut l, 10));
    assert_eq!(reserve(&mut l, 9), Decision::RetryAt(11));
    l.record_limit(&id("a"), 30);
    l.record_limit(&id("a"), 20);
    assert_eq!(reserve(&mut l, 11), Decision::RetryAt(30));
    assert_eq!(l.snapshot(29).buckets[0].closed_until, Some(30));
    grant(reserve(&mut l, 30));
    assert_eq!(reserve(&mut l, 0), Decision::RetryAt(31));
}
#[test]
// covers: RL-06
fn hold_commit_charges_actual_commit_time_and_is_idempotent() {
    let mut l = configured(1, 10, 1);
    let h = held(l.hold(&[id("a")], None, Some("write"), 300, 0));
    assert_eq!(reserve(&mut l, 100), Decision::RetryAt(300));
    let slot = l.commit(h, 200).unwrap();
    assert_eq!(l.commit(h, 301), Ok(slot));
    // The late replay above advances GC and expires the committed write.
    assert_eq!(l.snapshot(301).buckets[0].granted, 0);
    let mut l = configured(1, 10, 1);
    let h = held(l.hold(&[id("a")], None, None, 300, 0));
    l.commit(h, 200).unwrap();
    assert_eq!(reserve(&mut l, 200), Decision::RetryAt(210));
    grant(reserve(&mut l, 210));
}
#[test]
// covers: RL-06
fn hold_expiry_equal_now_and_crash_between_ledgers_frees_capacity() {
    let mut user = configured(1, 3600, 1);
    let mut installation = configured(1, 3600, 1);
    let orphan = held(user.hold(&[id("a")], None, Some("request"), 60, 0));
    assert_eq!(reserve(&mut user, 59), Decision::RetryAt(60));
    assert_eq!(user.commit(orphan, 60), Err(LedgerError::Expired));
    let u = held(user.hold(&[id("a")], None, Some("retry"), 60, 60));
    let i = held(installation.hold(&[id("a")], None, Some("retry"), 60, 60));
    let us = user.commit(u, 61).unwrap();
    let ins = installation.commit(i, 61).unwrap();
    assert_eq!(user.commit(u, 62), Ok(us));
    assert_eq!(installation.commit(i, 62), Ok(ins));
    user.release(us);
    installation.release(ins);
    grant(reserve(&mut user, 62));
    grant(reserve(&mut installation, 62));
}
#[test]
fn second_ledger_refusal_releases_first_and_hold_key_replay_never_reacquires() {
    let mut user = configured(1, 60, 0);
    let mut installation = configured(1, 60, 0);
    grant(reserve(&mut installation, 0));
    let first = user.hold(&[id("a")], None, Some("op"), 60, 0);
    assert_eq!(
        installation.hold(&[id("a")], None, Some("op"), 60, 0),
        HoldDecision::RetryAt(60)
    );
    user.release_hold(held(first.clone()));
    assert_eq!(user.hold(&[id("a")], None, Some("op"), 60, 1), first);
    assert_eq!(user.snapshot(1).holds, 0);
    assert_eq!(user.commit(held(first), 1), Err(LedgerError::UnknownHold));
    grant(reserve(&mut user, 1));
}
#[test]
fn ttl_is_capped_and_zero_is_uncommittable() {
    let mut l = configured(10, 60, 0);
    let h = held(l.hold(&[id("a")], None, None, u32::MAX, 0));
    assert_eq!(h.expires_at, 300);
    assert_eq!(l.commit(h, 300), Err(LedgerError::Expired));
    let h = held(l.hold(&[id("a")], None, None, 0, 300));
    assert_eq!(l.commit(h, 300), Err(LedgerError::Expired));
}
#[test]
// covers: RL-10
fn snapshot_really_collects_serialized_state_but_retains_configuration() {
    let mut l = configured(100, 60, 0);
    for now in 0..50 {
        grant(l.reserve(&[id("a")], None, Some(&format!("key:{now}")), now));
    }
    held(l.hold(&[id("a")], None, Some("hold"), 10, 50));
    let before = serde_json::to_vec(&l).unwrap().len();
    let view = l.snapshot(86_500);
    assert_eq!(view.buckets.len(), 1);
    assert_eq!(view.buckets[0].granted, 0);
    assert_eq!(view.holds, 0);
    assert_eq!(view.request_keys, 0);
    assert!(serde_json::to_vec(&l).unwrap().len() < before / 5);
    l.remove_bucket(&id("a"));
    assert!(l.snapshot(86_500).buckets.is_empty());
}
#[test]
fn buckets_prune_independently_and_remove_does_not_resurrect_old_charges() {
    let mut l = configured(1, 10, 0);
    l.upsert_bucket(BucketSpec {
        id: id("b"),
        windows: vec![(1, 100)],
        min_spacing_secs: 0,
    });
    let slot = grant(l.reserve(&[id("a"), id("b")], None, None, 0));
    assert_eq!(l.snapshot(10).buckets[0].granted, 0);
    assert_eq!(l.snapshot(10).buckets[1].granted, 1);
    l.remove_bucket(&id("b"));
    l.upsert_bucket(BucketSpec {
        id: id("b"),
        windows: vec![(1, 100)],
        min_spacing_secs: 0,
    });
    grant(l.reserve(&[id("b")], None, None, 10));
    l.release(slot);
    assert_eq!(l.snapshot(10).buckets[1].granted, 1);
}
#[test]
fn invalid_specs_and_time_overflow_fail_closed() {
    for (cap, window) in [(0, 60), (10, 0)] {
        let mut l = configured(cap, window, 0);
        assert_eq!(reserve(&mut l, 0), Decision::RetryAt(i64::MAX));
    }
    let mut l = configured(1, 60, 0);
    grant(reserve(&mut l, i64::MAX - 1));
    assert_eq!(reserve(&mut l, i64::MAX), Decision::RetryAt(i64::MAX));
    assert_eq!(reserve(&mut l, i64::MIN), Decision::RetryAt(i64::MAX));
    let mut l = configured(1, 60, 0);
    grant(reserve(&mut l, i64::MIN));
    grant(reserve(&mut l, i64::MIN + 60));
}
#[test]
fn sliding_window_matches_independent_reference_over_many_traces() {
    // Deterministic property exploration: changing caps/windows, duplicate
    // timestamps, long jumps and rollback, against an unbounded event model.
    for cap in 1..12 {
        for window in 1..25 {
            let mut w = SlidingWindow::new(cap, window);
            let mut reference = Vec::new();
            let mut now = 0_i64;
            let mut seed = 17_u64;
            for _ in 0..300 {
                seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
                let supplied = now + (seed % 9) as i64 - 2;
                now = now.max(supplied);
                reference.retain(|at: &i64| *at + i64::from(window) > now);
                let expected = if reference.len() >= cap as usize {
                    Err(reference[reference.len() - cap as usize] + i64::from(window))
                } else {
                    reference.push(now);
                    Ok(())
                };
                assert_eq!(w.record(supplied), expected);
                assert_eq!(w.len(), reference.len());
                assert!(w.len() <= cap as usize);
            }
        }
    }
}
#[test]
fn ledger_matches_reference_under_releases_and_closures() {
    let mut l = configured(5, 17, 2);
    let mut charges: BTreeMap<SlotId, i64> = BTreeMap::new();
    let mut closure = 0;
    let mut now = 0;
    for step in 0..3000 {
        now += step % 3;
        if step % 17 == 0 {
            closure = now + 9;
            l.record_limit(&id("a"), closure);
        }
        if step % 7 == 0
            && let Some(slot) = charges.keys().next().copied()
        {
            charges.remove(&slot);
            l.release(slot);
        }
        charges.retain(|_, at| *at + 17 > now);
        let mut retry = (closure > now).then_some(closure);
        let mut times: Vec<_> = charges.values().copied().collect();
        times.sort();
        if times.len() >= 5 {
            retry = Some(retry.unwrap_or(0).max(times[times.len() - 5] + 17));
        }
        if let Some(at) = times.last()
            && at + 2 > now
        {
            retry = Some(retry.unwrap_or(0).max(at + 2));
        }
        match (reserve(&mut l, now), retry) {
            (Decision::Granted { slot }, None) => {
                charges.insert(slot, now);
            }
            (Decision::RetryAt(actual), Some(expected)) => assert_eq!(actual, expected),
            other => panic!("{other:?}"),
        }
    }
}
#[test]
fn content_profiles_and_send_sync_contract() {
    fn send_sync<T: Send + Sync>() {}
    send_sync::<WriteLedger>();
    let specs = content_write_buckets(id("installation"), Some(id("user")));
    assert_eq!(specs.len(), 2);
    for s in specs {
        assert_eq!(s.windows, vec![(80, 60), (500, 3600)]);
        assert_eq!(s.min_spacing_secs, 1);
    }
    assert_eq!(content_write_buckets(id("same"), Some(id("same"))).len(), 1);
}

#[test]
// covers: RL-01 RL-04
// The response classification half of this test (429 `retry-after` and the
// `spammed` sub-code into `ApiError`) stays with the error type's own tests:
// `ApiError::from_response` is private to the GitHub client. What the ledger
// owns is that a limit closes the bucket only when the caller records it.
fn rate_limit_headers_and_spam_classification_only_close_when_caller_records() {
    let mut l = configured(10, 60, 0);
    // A 429 with `retry-after: 30`, recorded by the caller.
    l.record_limit(&id("a"), 100 + 30);
    assert_eq!(reserve(&mut l, 129), Decision::RetryAt(130));
    grant(reserve(&mut l, 130));
    // A `spammed` 422 carries no wait; the 24h closure is P1's choice, never toolkit policy.
    l.record_limit(&id("a"), 130 + 86400);
    assert_eq!(reserve(&mut l, 131), Decision::RetryAt(86530));
    grant(reserve(&mut l, 86530));
}
#[test]
fn journal_roundtrip_of_inflight_hold_and_commit_receipt() {
    let mut l = configured(2, 60, 0);
    let original = l.hold(
        &[id("a")],
        Some(&ReservationTag::new("campaign")),
        Some("hold-op"),
        60,
        100,
    );
    let mut l: WriteLedger = serde_json::from_slice(&serde_json::to_vec(&l).unwrap()).unwrap();
    assert_eq!(l.hold(&[id("a")], None, Some("hold-op"), 1, 110), original);
    let slot = l.commit(held(original), 110).unwrap();
    let mut l: WriteLedger = serde_json::from_slice(&serde_json::to_vec(&l).unwrap()).unwrap();
    let h = held(l.hold(&[], None, Some("hold-op"), 0, 120));
    assert_eq!(l.commit(h, 120), Ok(slot));
    assert_eq!(l.snapshot(120).buckets[0].granted, 1);
    l.release(slot);
    assert_eq!(l.commit(h, 121), Ok(slot));
    assert_eq!(l.snapshot(121).buckets[0].granted, 0);
}
#[test]
fn expired_hold_cannot_be_revived_by_a_rollback_clock() {
    let mut l = configured(1, 60, 0);
    let h = held(l.hold(&[id("a")], None, None, 60, 100));
    l.snapshot(160);
    assert_eq!(l.commit(h, 100), Err(LedgerError::Expired));
    grant(reserve(&mut l, 100));
    assert_eq!(l.snapshot(100).at, 160);
}

#[test]
// covers: RL-05a
fn partial_refund_is_atomic_preserves_content_spacing_and_exact_receipt() {
    let mut l = configured(2, 3600, 1);
    l.upsert_bucket(BucketSpec {
        id: id("invite"),
        windows: vec![(1, 86400)],
        min_spacing_secs: 0,
    });
    let tag = ReservationTag::new("link");
    let slot = grant(l.reserve(&[id("a"), id("invite")], Some(&tag), Some("write"), 100));
    assert_eq!(
        l.refund_buckets(slot, &[id("invite"), id("foreign")]),
        Err(RefundError::InvalidSubset)
    );
    assert_eq!(l.count_tagged(&id("invite"), &tag, 100), 1);
    assert_eq!(l.refund_buckets(slot, &[]), Err(RefundError::InvalidSubset));
    assert_eq!(
        l.refund_buckets(SlotId(u64::MAX), &[id("invite")]),
        Err(RefundError::UnknownSlot)
    );
    l.refund_buckets(slot, &[id("invite"), id("invite")])
        .unwrap();
    l.refund_buckets(slot, &[id("invite")]).unwrap();
    assert_eq!(l.count_tagged(&id("invite"), &tag, 100), 0);
    assert_eq!(
        l.reserve(&[id("a"), id("invite")], Some(&tag), Some("write"), 100),
        Decision::Granted { slot }
    );
    assert_eq!(reserve(&mut l, 100), Decision::RetryAt(101));
    grant(reserve(&mut l, 101));
    assert_eq!(reserve(&mut l, 102), Decision::RetryAt(3700));
    let mut restored: WriteLedger =
        serde_json::from_str(&serde_json::to_string(&l).unwrap()).unwrap();
    restored.refund_buckets(slot, &[id("invite")]).unwrap();
    assert_eq!(reserve(&mut restored, 102), Decision::RetryAt(3700));
}

#[test]
fn partial_refund_keeps_committed_hold_replay_and_commit_timestamp() {
    let mut l = configured(1, 3600, 1);
    l.upsert_bucket(BucketSpec {
        id: id("invite"),
        windows: vec![(1, 86400)],
        min_spacing_secs: 0,
    });
    let hold = held(l.hold(&[id("a"), id("invite")], None, Some("hold"), 60, 100));
    let slot = l.commit(hold, 120).unwrap();
    l.refund_buckets(slot, &[id("invite")]).unwrap();
    assert_eq!(l.commit(hold, 130), Ok(slot));
    assert_eq!(reserve(&mut l, 130), Decision::RetryAt(3720));
    assert_eq!(
        l.snapshot(130)
            .buckets
            .iter()
            .find(|b| b.spec.id == id("invite"))
            .unwrap()
            .granted,
        0
    );
}
