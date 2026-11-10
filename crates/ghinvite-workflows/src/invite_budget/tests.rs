use super::*;
use ghinvite_core::{
    InvitationLinkId,
    budget::{BucketId, BudgetKind, LinkShare, RetryReason},
};
fn time(at: i64) -> DateTime<Utc> {
    DateTime::from_timestamp(at, 0).unwrap()
}
fn owner() -> AccountInstallationRef {
    AccountInstallationRef {
        app: App::Org,
        account_id: 1,
    }
}
fn input(key: &str, link: Option<InvitationLinkId>) -> Reserve {
    Reserve {
        request_key: key.into(),
        kind: BudgetKind::OrgInvite,
        link_id: link,
    }
}
fn prepare(s: &mut State, at: i64) {
    s.prepare(owner(), time(at));
}
fn granted(r: Reservation) -> SlotId {
    match r {
        Reservation::Granted { slot_id } => slot_id,
        v => panic!("{v:?}"),
    }
}

fn observed_limit(id: u64, reason: ghinvite_core::budget::LimitReason, at: i64) -> RecordLimit {
    let mut limit = RecordLimit::observed(BucketId::Org, None, reason, time(at));
    limit.observation.as_mut().unwrap().id = Some(format!("{id:064x}"));
    limit
}

#[test]
fn limit_counts_preserve_observation_time_and_deduplicate_recovery() {
    use ghinvite_core::budget::LimitReason::{RateLimited, Spammed};
    let mut state = State::default();
    prepare(&mut state, 0);
    prepare(&mut state, 100);
    let spam = observed_limit(1, Spammed, 100);
    state.record_limit(owner(), spam.clone()).unwrap();
    prepare(&mut state, 130);
    state.record_limit(owner(), spam.clone()).unwrap();
    state
        .record_limit(owner(), observed_limit(2, RateLimited, 130))
        .unwrap();
    let snapshot = state.snapshot(owner());
    assert_eq!(snapshot.limit_counts.spammed, 1);
    assert_eq!(snapshot.limit_counts.rate_limited, 1);
    assert_eq!(snapshot.limit_counts.coverage_started_at, Some(time(0)));
    assert!(snapshot.limit_counts.incomplete);
    let mut changed = spam.clone();
    changed.observation.as_mut().unwrap().reason = RateLimited;
    assert_eq!(
        state.record_limit(owner(), changed).unwrap_err().code(),
        409
    );
    assert_eq!(state.snapshot(owner()), snapshot);
    prepare(&mut state, 86_500);
    state.record_limit(owner(), spam).unwrap();
    assert_eq!(
        state.snapshot(owner()).limit_counts.spammed,
        0,
        "expired evidence never becomes a new observation"
    );
    assert_eq!(
        state.snapshot(owner()).buckets[0].closed_until,
        Some(time(86_530)),
        "only the distinct rate-limit observation extended the closure"
    );
    prepare(&mut state, 86_530);
    assert!(!state.snapshot(owner()).limit_counts.incomplete);
    granted(
        state
            .reserve(owner(), input("after-observations", None))
            .unwrap(),
    );
}

#[test]
fn unbound_legacy_future_and_overflow_observations_never_claim_complete_counts() {
    use ghinvite_core::budget::LimitReason::Spammed;
    let mut state = State::default();
    prepare(&mut state, 0);
    prepare(&mut state, 86_500);
    assert!(!state.snapshot(owner()).limit_counts.incomplete);
    let unbound = RecordLimit::observed(BucketId::Org, None, Spammed, time(86_500));
    assert_eq!(
        state.record_limit(owner(), unbound).unwrap_err().code(),
        400
    );
    state
        .record_limit(
            owner(),
            RecordLimit {
                bucket: BucketId::Org,
                until: None,
                observation: None,
            },
        )
        .unwrap();
    assert_eq!(state.snapshot(owner()).limit_counts.spammed, 0);
    assert!(state.snapshot(owner()).limit_counts.incomplete);
    state
        .record_limit(owner(), observed_limit(1, Spammed, 86_501))
        .unwrap();
    assert_eq!(state.snapshot(owner()).limit_counts.future_observations, 1);
    assert_eq!(state.snapshot(owner()).limit_counts.spammed, 0);

    let mut bounded = State::default();
    prepare(&mut bounded, 0);
    prepare(&mut bounded, 86_500);
    for id in 0..=super::model::MAX_LIMIT_OBSERVATIONS {
        bounded
            .record_limit(owner(), observed_limit(id as u64, Spammed, 86_500))
            .unwrap();
    }
    let counts = bounded.snapshot(owner()).limit_counts;
    assert_eq!(
        counts.spammed as usize,
        super::model::MAX_LIMIT_OBSERVATIONS
    );
    assert!(
        counts.incomplete,
        "overflow cannot present a complete lower count"
    );
    assert!(matches!(
        bounded
            .reserve(owner(), input("still-closed", None))
            .unwrap(),
        Reservation::RetryAt {
            why: RetryReason::Closed,
            ..
        }
    ));
}
#[test]
fn default_share_refuses_twenty_sixth_while_admin_uses_remainder() {
    let mut s = State::default();
    let link = InvitationLinkId::new();
    for i in 0..25 {
        prepare(&mut s, i);
        granted(
            s.reserve(owner(), input(&format!("d{i}"), Some(link)))
                .unwrap(),
        );
    }
    prepare(&mut s, 25);
    assert_eq!(
        s.reserve(owner(), input("share", Some(link))).unwrap(),
        Reservation::RetryAt {
            at: time(86400),
            why: RetryReason::LinkShare
        }
    );
    granted(s.reserve(owner(), input("admin", None)).unwrap());
    assert_eq!(s.snapshot(owner()).buckets[0].used_24h, 26);
}
#[test]
fn cap_raise_keeps_counts_and_lower_cannot_erase_existing_reservations() {
    let mut s = State::default();
    for i in 0..50 {
        prepare(&mut s, i);
        granted(s.reserve(owner(), input(&format!("d{i}"), None)).unwrap());
    }
    prepare(&mut s, 50);
    assert!(matches!(
        s.reserve(owner(), input("full", None)).unwrap(),
        Reservation::RetryAt {
            why: RetryReason::Bucket,
            ..
        }
    ));
    s.configure(
        owner(),
        Configure {
            request_key: "established".into(),
            confirmed_established: true,
            link_shares: vec![],
        },
    )
    .unwrap();
    granted(s.reserve(owner(), input("raised", None)).unwrap());
    assert_eq!(s.snapshot(owner()).buckets[0].cap, 500);
    assert_eq!(s.snapshot(owner()).buckets[0].used_24h, 51);
    prepare(&mut s, 51);
    s.configure(
        owner(),
        Configure {
            request_key: "lowered".into(),
            confirmed_established: false,
            link_shares: vec![],
        },
    )
    .unwrap();
    assert!(matches!(
        s.reserve(owner(), input("lowered-write", None)).unwrap(),
        Reservation::RetryAt { .. }
    ));
    assert_eq!(s.snapshot(owner()).buckets[0].used_24h, 51);
}
#[test]
fn exact_receipt_replay_after_release_and_twenty_four_hour_rotation() {
    let mut s = State::default();
    prepare(&mut s, 100);
    let original = s.reserve(owner(), input("op", None)).unwrap();
    let first = granted(original.clone());
    s.release(first);
    prepare(&mut s, 101);
    assert_eq!(s.reserve(owner(), input("op", None)).unwrap(), original);
    assert_eq!(s.snapshot(owner()).buckets[0].used_24h, 0);
    let mut changed = input("op", Some(InvitationLinkId::new()));
    assert!(s.reserve(owner(), changed.clone()).is_err());
    prepare(&mut s, 86500);
    let replacement = granted(s.reserve(owner(), input("op", None)).unwrap());
    assert_ne!(replacement, first);
    s.release(first);
    assert_eq!(s.snapshot(owner()).buckets[0].used_24h, 1);
    changed.request_key = "another".into();
    assert!(s.reserve(owner(), changed).is_ok());
}
#[test]
fn retained_spacing_refusal_needs_new_attempt_and_clock_rollback_fails_closed() {
    let mut s = State::default();
    prepare(&mut s, 100);
    granted(s.reserve(owner(), input("first", None)).unwrap());
    let retry = s.reserve(owner(), input("second", None)).unwrap();
    assert_eq!(
        retry,
        Reservation::RetryAt {
            at: time(101),
            why: RetryReason::Spacing
        }
    );
    prepare(&mut s, 101);
    assert_eq!(s.reserve(owner(), input("second", None)).unwrap(), retry);
    granted(s.reserve(owner(), input("fresh-attempt", None)).unwrap());
    prepare(&mut s, 90);
    assert_eq!(s.snapshot(owner()).observed_at, time(101));
    assert!(matches!(
        s.reserve(owner(), input("rollback", None)).unwrap(),
        Reservation::RetryAt {
            why: RetryReason::Spacing,
            ..
        }
    ));
}
#[test]
fn closure_spam_fallback_and_explicit_wait_do_not_shorten_each_other() {
    let mut s = State::default();
    prepare(&mut s, 100);
    s.record_limit(
        owner(),
        RecordLimit {
            observation: None,
            bucket: BucketId::Org,
            until: None,
        },
    )
    .unwrap();
    s.record_limit(
        owner(),
        RecordLimit {
            observation: None,
            bucket: BucketId::Org,
            until: Some(time(130)),
        },
    )
    .unwrap();
    assert_eq!(
        s.reserve(owner(), input("blocked", None)).unwrap(),
        Reservation::RetryAt {
            at: time(86500),
            why: RetryReason::Closed
        }
    );
    prepare(&mut s, 86500);
    granted(s.reserve(owner(), input("open", None)).unwrap());
    assert_eq!(s.snapshot(owner()).content_closed_until, None);
    s.record_limit(
        owner(),
        RecordLimit {
            observation: None,
            bucket: BucketId::Content,
            until: Some(time(86530)),
        },
    )
    .unwrap();
    prepare(&mut s, 86529);
    assert_eq!(s.snapshot(owner()).content_closed_until, Some(time(86530)));
    assert!(
        matches!(s.reserve(owner(),input("wait",None)).unwrap(),Reservation::RetryAt{at,..} if at==time(86530))
    );
    prepare(&mut s, 86530);
    assert_eq!(s.snapshot(owner()).content_closed_until, None);
    granted(s.reserve(owner(), input("after", None)).unwrap());
}
#[test]
fn configured_share_rounds_down_with_minimum_one_and_input_binding() {
    let mut s = State::default();
    let link = InvitationLinkId::new();
    prepare(&mut s, 0);
    let policy = Configure {
        request_key: "policy".into(),
        confirmed_established: false,
        link_shares: vec![LinkShare {
            link_id: link,
            percent: 1,
        }],
    };
    s.configure(owner(), policy.clone()).unwrap();
    granted(s.reserve(owner(), input("first", Some(link))).unwrap());
    prepare(&mut s, 1);
    assert!(matches!(
        s.reserve(owner(), input("second", Some(link))).unwrap(),
        Reservation::RetryAt {
            why: RetryReason::LinkShare,
            ..
        }
    ));
    s.configure(owner(), policy.clone()).unwrap();
    let mut conflict = policy.clone();
    conflict.link_shares[0].percent = 100;
    assert!(s.configure(owner(), conflict).is_err());
    let mut invalid = policy;
    invalid.request_key = "invalid".into();
    invalid.link_shares[0].percent = 0;
    assert!(s.configure(owner(), invalid).is_err());
}
#[test]
fn repo_app_isolated_buckets_share_content_spacing_and_other_writes_skip_invite_cap() {
    let owner = AccountInstallationRef {
        app: App::Repo,
        account_id: 1,
    };
    let mut s = State::default();
    s.prepare(owner, time(0));
    assert!(s.reserve(owner, input("wrong-app", None)).is_err());
    let r = |key: &str, id| Reserve {
        request_key: key.into(),
        kind: BudgetKind::RepoInvite { repo_id: id },
        link_id: None,
    };
    granted(s.reserve(owner, r("a", 10)).unwrap());
    assert!(matches!(
        s.reserve(owner, r("b", 11)).unwrap(),
        Reservation::RetryAt {
            why: RetryReason::Spacing,
            ..
        }
    ));
    s.prepare(owner, time(1));
    granted(s.reserve(owner, r("b2", 11)).unwrap());
    s.prepare(owner, time(2));
    granted(
        s.reserve(
            owner,
            Reserve {
                request_key: "patch".into(),
                kind: BudgetKind::OtherWrite,
                link_id: None,
            },
        )
        .unwrap(),
    );
    let view = s.snapshot(owner);
    assert_eq!(view.content_used_hour, 3);
    assert_eq!(view.buckets.iter().map(|b| b.used_24h).sum::<u32>(), 2);
    s.prepare(owner, time(86402));
    assert!(s.snapshot(owner).buckets.is_empty());
}
#[test]
fn journal_roundtrip_keeps_slot_and_decision_identity() {
    let mut s = State::default();
    prepare(&mut s, 10);
    let result = s.reserve(owner(), input("same", None)).unwrap();
    s.advance().unwrap();
    let mut copy: State = serde_json::from_slice(&serde_json::to_vec(&s).unwrap()).unwrap();
    prepare(&mut copy, 11);
    assert_eq!(copy.reserve(owner(), input("same", None)).unwrap(), result);
    copy.release(granted(result));
    assert_eq!(copy.snapshot(owner()).content_used_hour, 0);
}

#[test]
fn content_hour_cap_includes_other_writes_and_uses_exact_window_boundaries() {
    let mut s = State::default();
    for i in 0..500 {
        prepare(&mut s, i);
        granted(
            s.reserve(
                owner(),
                Reserve {
                    request_key: format!("write-{i}"),
                    kind: BudgetKind::OtherWrite,
                    link_id: None,
                },
            )
            .unwrap(),
        );
    }
    let view = s.snapshot(owner());
    assert_eq!(view.content_used_hour, 500);
    assert_eq!(view.content_used_minute, 60);
    assert_eq!(view.buckets[0].used_24h, 0);
    prepare(&mut s, 500);
    assert_eq!(
        s.reserve(
            owner(),
            Reserve {
                request_key: "full-hour".into(),
                kind: BudgetKind::OtherWrite,
                link_id: None
            }
        )
        .unwrap(),
        Reservation::RetryAt {
            at: time(3600),
            why: RetryReason::Bucket
        }
    );
    prepare(&mut s, 3600);
    granted(
        s.reserve(
            owner(),
            Reserve {
                request_key: "new-window".into(),
                kind: BudgetKind::OtherWrite,
                link_id: None,
            },
        )
        .unwrap(),
    );
    assert_eq!(s.snapshot(owner()).content_used_hour, 500);
}

fn repo_owner() -> AccountInstallationRef {
    AccountInstallationRef {
        app: App::Repo,
        account_id: 9,
    }
}
fn repo_input(key: &str) -> Reserve {
    Reserve {
        request_key: key.into(),
        kind: BudgetKind::RepoInvite { repo_id: 10 },
        link_id: None,
    }
}
#[test]
fn put204_refund_binds_original_input_preserves_content_and_fences_all_release() {
    let mut s = State::default();
    let owner = repo_owner();
    s.prepare(owner, time(100));
    let original = repo_input("put");
    let slot = granted(s.reserve(owner, original.clone()).unwrap());
    let refund = RefundInvitationAfterPut204 {
        slot_id: slot,
        reservation: original.clone(),
    };
    for change in 0..4 {
        let mut wrong = refund.clone();
        match change {
            0 => wrong.slot_id = SlotId(slot.0 + 1),
            1 => wrong.reservation.link_id = Some(InvitationLinkId::new()),
            2 => wrong.reservation.kind = BudgetKind::RepoInvite { repo_id: 20 },
            _ => wrong.reservation.kind = BudgetKind::OtherWrite,
        }
        assert!(s.refund_invitation_after_put204(wrong).is_err());
        assert_eq!(s.snapshot(owner).buckets[0].used_24h, 1);
    }
    let refused = repo_input("spacing");
    assert!(matches!(
        s.reserve(owner, refused.clone()).unwrap(),
        Reservation::RetryAt { .. }
    ));
    assert_eq!(
        s.refund_invitation_after_put204(RefundInvitationAfterPut204 {
            slot_id: slot,
            reservation: refused
        })
        .unwrap(),
        InvitationRefund::NotApplicable
    );
    assert_eq!(
        s.refund_invitation_after_put204(refund.clone()).unwrap(),
        InvitationRefund::Refunded
    );
    s.release(slot);
    let mut s: State = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
    assert_eq!(
        s.refund_invitation_after_put204(refund.clone()).unwrap(),
        InvitationRefund::Refunded
    );
    s.release(slot);
    let snapshot = s.snapshot(owner);
    assert_eq!(snapshot.buckets[0].used_24h, 0);
    assert_eq!(snapshot.content_used_hour, 1);
    assert_eq!(snapshot.next_content_write_at, Some(time(101)));
    assert_eq!(
        s.reserve(owner, original.clone()).unwrap(),
        Reservation::Granted { slot_id: slot }
    );
    assert!(matches!(
        s.reserve(owner, repo_input("still-spaced")).unwrap(),
        Reservation::RetryAt {
            why: RetryReason::Spacing,
            ..
        }
    ));
    s.prepare(owner, time(101));
    let other = Reserve {
        kind: BudgetKind::OtherWrite,
        ..repo_input("other")
    };
    let other_slot = granted(s.reserve(owner, other.clone()).unwrap());
    assert_eq!(
        s.refund_invitation_after_put204(RefundInvitationAfterPut204 {
            slot_id: other_slot,
            reservation: other
        })
        .unwrap(),
        InvitationRefund::NotApplicable
    );
    s.prepare(owner, time(102));
    let unused = repo_input("unused");
    let unused_slot = granted(s.reserve(owner, unused.clone()).unwrap());
    s.release(unused_slot);
    assert_eq!(
        s.refund_invitation_after_put204(RefundInvitationAfterPut204 {
            slot_id: unused_slot,
            reservation: unused
        })
        .unwrap(),
        InvitationRefund::NotApplicable
    );
    let missing = repo_input("missing");
    assert_eq!(
        s.refund_invitation_after_put204(RefundInvitationAfterPut204 {
            slot_id: SlotId(999),
            reservation: missing
        })
        .unwrap(),
        InvitationRefund::NotApplicable
    );
    s.prepare(owner, time(86500));
    assert_eq!(
        s.refund_invitation_after_put204(refund.clone()).unwrap(),
        InvitationRefund::NotApplicable
    );
    let replacement = granted(s.reserve(owner, original).unwrap());
    assert_ne!(replacement, slot);
    assert!(s.refund_invitation_after_put204(refund).is_err());
    s.release(slot);
    assert_eq!(s.snapshot(owner).content_used_hour, 1);
}
#[test]
fn refunded_puts_still_exhaust_content_hour_but_free_daily_and_link_share() {
    let owner = repo_owner();
    let mut s = State::default();
    let link = InvitationLinkId::new();
    for i in 0..500 {
        s.prepare(owner, time(i * 2));
        let mut reservation = repo_input(&format!("put-{i}"));
        reservation.link_id = Some(link);
        let slot_id = granted(s.reserve(owner, reservation.clone()).unwrap());
        assert_eq!(
            s.refund_invitation_after_put204(RefundInvitationAfterPut204 {
                slot_id,
                reservation
            })
            .unwrap(),
            InvitationRefund::Refunded
        );
        s.release(slot_id);
    }
    s.prepare(owner, time(1000));
    let snapshot = s.snapshot(owner);
    assert_eq!(snapshot.content_used_hour, 500);
    assert!(snapshot.buckets.iter().all(|b| b.used_24h == 0));
    assert!(matches!(
        s.reserve(owner, repo_input("full")).unwrap(),
        Reservation::RetryAt {
            why: RetryReason::Bucket,
            ..
        }
    ));
}

#[test]
fn authenticated_cap_policy_preserves_shares_usage_and_rejects_equal_revision_conflicts() {
    use ghinvite_core::budget::{OrgCapPolicy, SetOrgCap};
    let mut s = State::default();
    let link = InvitationLinkId::new();
    prepare(&mut s, 100);
    s.configure(
        owner(),
        Configure {
            request_key: "initial-share".into(),
            confirmed_established: false,
            link_shares: vec![LinkShare {
                link_id: link,
                percent: 2,
            }],
        },
    )
    .unwrap();
    granted(s.reserve(owner(), input("first", Some(link))).unwrap());
    prepare(&mut s, 101);
    assert!(matches!(
        s.reserve(owner(), input("denied", Some(link))).unwrap(),
        Reservation::RetryAt {
            why: RetryReason::LinkShare,
            ..
        }
    ));
    let raised = OrgCapPolicy {
        account_id: 1,
        revision: 1,
        command: SetOrgCap {
            operation_id: ghinvite_core::RequestId::new()
                .to_string()
                .try_into()
                .unwrap(),
            actor_user_id: 8,
            confirmed_established: true,
        },
        changed_at: time(101),
    };
    s.set_org_cap(owner(), raised.clone()).unwrap();
    granted(s.reserve(owner(), input("second", Some(link))).unwrap());
    assert_eq!(s.snapshot(owner()).buckets[0].used_24h, 2);
    let mut conflict = raised.clone();
    conflict.command.confirmed_established = false;
    assert!(s.set_org_cap(owner(), conflict).is_err());
    let mut lowered = raised.clone();
    lowered.revision = 2;
    lowered.command.confirmed_established = false;
    s.set_org_cap(owner(), lowered).unwrap();
    s.set_org_cap(owner(), raised).unwrap();
    prepare(&mut s, 102);
    assert_eq!(s.snapshot(owner()).buckets[0].cap, 50);
    assert!(matches!(
        s.reserve(owner(), input("still-shared", Some(link)))
            .unwrap(),
        Reservation::RetryAt {
            why: RetryReason::LinkShare,
            ..
        }
    ));
    granted(s.reserve(owner(), input("admin-unshared", None)).unwrap());
}

// GM-00 golden shapes (PD-9). `State` is the persisted Restate value at key
// `budget`; it embeds the `WriteLedger` and `ledger::SlotId`.
fn golden_link(value: &str) -> InvitationLinkId {
    value.parse().unwrap()
}
#[test]
fn golden_invite_budget_state_org() {
    use crate::test_support::golden;
    use ghinvite_core::budget::{LimitReason, OrgCapPolicy, SetOrgCap};
    let a = golden_link("01J0000000000000000000000A");
    let b = golden_link("01J0000000000000000000000B");
    let mut s = State::default();
    prepare(&mut s, 1_000);
    s.configure(
        owner(),
        Configure {
            request_key: "policy-1".into(),
            confirmed_established: false,
            link_shares: vec![LinkShare {
                link_id: a,
                percent: 40,
            }],
        },
    )
    .unwrap();
    s.set_link_share(LinkShare {
        link_id: b,
        percent: 25,
    })
    .unwrap();
    s.set_org_cap(
        owner(),
        OrgCapPolicy {
            account_id: 1,
            revision: 1,
            command: SetOrgCap {
                operation_id: "01J0000000000000000000000D".to_owned().try_into().unwrap(),
                actor_user_id: 8,
                confirmed_established: true,
            },
            changed_at: time(1_000),
        },
    )
    .unwrap();
    granted(s.reserve(owner(), input("r1", Some(a))).unwrap());
    prepare(&mut s, 1_001);
    let other = Reserve {
        request_key: "r2".into(),
        kind: BudgetKind::OtherWrite,
        link_id: None,
    };
    let released = granted(s.reserve(owner(), other).unwrap());
    prepare(&mut s, 1_002);
    granted(s.reserve(owner(), input("r3", Some(b))).unwrap());
    s.release(released);
    s.record_limit(owner(), observed_limit(7, LimitReason::Spammed, 1_002))
        .unwrap();
    prepare(&mut s, 1_003);
    assert!(matches!(
        s.reserve(owner(), input("r4", None)).unwrap(),
        Reservation::RetryAt {
            why: RetryReason::Closed,
            ..
        }
    ));
    s.record_limit(
        owner(),
        RecordLimit {
            bucket: BucketId::Content,
            until: Some(time(1_100)),
            observation: None,
        },
    )
    .unwrap();
    s.advance().unwrap();
    let decoded = golden::json("invite_budget_state_org.json", &s);
    assert_eq!(decoded.snapshot(owner()), s.snapshot(owner()));
}
#[test]
fn golden_invite_budget_state_repo_refund() {
    use crate::test_support::golden;
    let owner = repo_owner();
    let mut s = State::default();
    s.prepare(owner, time(2_000));
    let original = repo_input("put");
    let slot = granted(s.reserve(owner, original.clone()).unwrap());
    assert_eq!(
        s.refund_invitation_after_put204(RefundInvitationAfterPut204 {
            slot_id: slot,
            reservation: original,
        })
        .unwrap(),
        InvitationRefund::Refunded
    );
    s.prepare(owner, time(2_001));
    let second = Reserve {
        request_key: "second".into(),
        kind: BudgetKind::RepoInvite { repo_id: 11 },
        link_id: Some(golden_link("01J0000000000000000000000C")),
    };
    granted(s.reserve(owner, second).unwrap());
    s.advance().unwrap();
    let decoded = golden::json("invite_budget_state_repo.json", &s);
    assert_eq!(decoded.snapshot(owner), s.snapshot(owner));
}
/// The ledger on its own, built through its public API with every
/// retained collection populated: buckets and closures, granted, refunded and
/// committed slots, open holds, and reserve/hold/commit receipts.
#[test]
fn golden_write_ledger() {
    use super::ledger::{self, BucketSpec, Decision, HoldDecision, ReservationTag, WriteLedger};
    use crate::test_support::golden;
    let content = ledger::BucketId::new("content");
    let org = ledger::BucketId::new("org");
    let repo = ledger::BucketId::new("repo:10");
    let tag = ReservationTag::new("01J0000000000000000000000A");
    let mut ledger = WriteLedger::new();
    for spec in ledger::content_write_buckets(content.clone(), None) {
        ledger.upsert_bucket(spec);
    }
    for (id, cap) in [(&org, 500), (&repo, 50)] {
        ledger.upsert_bucket(BucketSpec {
            id: id.clone(),
            windows: vec![(cap, 86_400)],
            min_spacing_secs: 0,
        });
    }
    let Decision::Granted { .. } = ledger.reserve(
        &[content.clone(), org.clone()],
        Some(&tag),
        Some("reserve-1"),
        1_000,
    ) else {
        panic!("first reservation")
    };
    assert!(matches!(
        ledger.reserve(
            std::slice::from_ref(&content),
            None,
            Some("reserve-2"),
            1_000
        ),
        Decision::RetryAt(_)
    ));
    assert!(matches!(
        ledger.reserve(
            &[ledger::BucketId::new("unknown")],
            None,
            Some("reserve-3"),
            1_000
        ),
        Decision::UnknownBucket(_)
    ));
    let Decision::Granted { slot: refunded } = ledger.reserve(
        &[content.clone(), repo.clone()],
        None,
        Some("reserve-4"),
        1_001,
    ) else {
        panic!("refunded reservation")
    };
    ledger
        .refund_buckets(refunded, std::slice::from_ref(&repo))
        .unwrap();
    let HoldDecision::Held { hold, .. } = ledger.hold(
        &[content.clone(), org.clone()],
        Some(&tag),
        Some("hold-1"),
        120,
        1_002,
    ) else {
        panic!("first hold")
    };
    ledger.commit(hold, 1_003).unwrap();
    assert!(matches!(
        ledger.hold(std::slice::from_ref(&org), None, Some("hold-2"), 600, 1_003),
        HoldDecision::Held { .. }
    ));
    assert!(matches!(
        ledger.hold(
            std::slice::from_ref(&content),
            None,
            Some("hold-3"),
            60,
            1_003
        ),
        HoldDecision::RetryAt(_)
    ));
    let Decision::Granted { slot: unkeyed } =
        ledger.reserve(std::slice::from_ref(&repo), None, None, 1_004)
    else {
        panic!("unkeyed reservation")
    };
    let Decision::Granted { slot: released } =
        ledger.reserve(std::slice::from_ref(&repo), None, None, 1_004)
    else {
        panic!("released reservation")
    };
    assert_ne!(unkeyed, released);
    ledger.release(released);
    ledger.record_limit(&org, 5_000);
    let decoded = golden::json("write_ledger.json", &ledger);
    assert_eq!(decoded.snapshot(1_005), ledger.snapshot(1_005));
}
