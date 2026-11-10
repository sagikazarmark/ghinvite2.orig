use super::*;
fn at(seconds: i64) -> DateTime<Utc> {
    DateTime::from_timestamp(1_800_000_000 + seconds, 0).unwrap()
}
fn identity() -> Identity {
    Identity {
        batch: None,
        link_id: Some(InvitationLinkId::new()),
        request_id: RequestId::new(),
        kind: DeliveryKind::Repository,
        target_id: 10,
        delivery_generation: 1,
    }
}
fn state() -> State {
    State::new(identity(), at(0), 3600).unwrap()
}
fn facts(phase: Phase) -> Facts {
    Facts {
        phase,
        delivery_revision: 2,
        last_observed_at: None,
        budget: BudgetDisposition::Unused {
            slot_id: "slot1".into(),
        },
    }
}
fn window(epoch: u64) -> HoldWindow {
    HoldWindow {
        epoch,
        window_start: at(-100),
        paused_at: at(100),
    }
}
fn envelope(s: &State, revision: u64, epoch: u64, control: LinkControl) -> ControlEnvelope {
    ControlEnvelope {
        identity: s.identity.clone(),
        operation_id: RequestId::new().to_string().try_into().unwrap(),
        revision,
        hold_epoch: epoch,
        at: at(100 + revision as i64),
        control,
    }
}
fn apply(s: &State, f: &Facts, e: ControlEnvelope) -> Transition {
    reduce(s, f, Event::Apply(e), at(200)).unwrap()
}
fn revoke(
    s: &State,
    revision: u64,
    cancel: bool,
    abort_hold: Option<HoldWindow>,
) -> ControlEnvelope {
    envelope(
        s,
        revision,
        abort_hold.as_ref().map_or(0, |w| w.epoch),
        LinkControl::Revoked {
            cancel_pending_invitations: cancel,
            decline_pending_requests: false,
            abort_hold,
        },
    )
}
#[test]
fn manual_pause_and_resume_never_hold_existing_work() {
    let s = state();
    let f = facts(Phase::NoWriteProven);
    let paused = apply(&s, &f, envelope(&s, 1, 0, LinkControl::ManualPause));
    assert_eq!(paused.state.disposition(), &Disposition::Ready);
    assert!(paused.effects.is_empty());
    let resumed = apply(&paused.state, &f, envelope(&s, 2, 0, LinkControl::Active));
    assert!(resumed.effects.is_empty());
    assert_eq!(resumed.state.disposition(), &Disposition::Ready);
}
#[test]
fn held_release_invalidates_old_wakes_and_replays_exactly() {
    let s = state();
    let f = facts(Phase::NoWriteProven);
    let hold = envelope(&s, 1, 1, LinkControl::AutoPause { window: window(1) });
    let held = apply(&s, &f, hold.clone());
    assert_eq!(held.effects, vec![Effect::RecordHeld { epoch: 1 }]);
    assert!(!held.state.accepts_wake(held.state.wake_generation()));
    assert!(apply(&held.state, &f, hold.clone()).effects.is_empty());
    let release = envelope(&s, 2, 1, LinkControl::Active);
    let released = apply(&held.state, &f, release.clone());
    assert_eq!(released.effects, vec![Effect::Wake { generation: 2 }]);
    assert!(released.state.accepts_wake(2));
    assert!(!released.state.accepts_wake(1));
    assert_eq!(apply(&released.state, &f, hold).state, released.state);
    assert!(apply(&released.state, &f, release).effects.is_empty());
    let reused = envelope(&s, 3, 1, LinkControl::AutoPause { window: window(1) });
    assert_eq!(
        reduce(&released.state, &f, Event::Apply(reused), at(200)),
        Err(Error::RevisionConflict)
    );
    let newer = apply(
        &released.state,
        &f,
        envelope(&s, 3, 3, LinkControl::AutoPause { window: window(3) }),
    );
    assert_eq!(newer.state.disposition(), &Disposition::Held { epoch: 3 });
    assert!(!newer.state.accepts_wake(2));
}
#[test]
fn delayed_revoke_contains_unseen_hold_and_is_irreversible() {
    let s = state();
    let f = facts(Phase::NoWriteProven);
    let revoked = apply(&s, &f, revoke(&s, 2, false, Some(window(1))));
    assert_eq!(revoked.state.disposition(), &Disposition::Aborted);
    assert_eq!(
        revoked.effects,
        vec![
            Effect::RecordAborted,
            Effect::ReleaseUnusedSlot {
                slot_id: "slot1".into()
            }
        ]
    );
    let hold = envelope(&s, 1, 1, LinkControl::AutoPause { window: window(1) });
    assert_eq!(apply(&revoked.state, &f, hold).state, revoked.state);
    assert_eq!(
        reduce(
            &revoked.state,
            &f,
            Event::Apply(envelope(&s, 3, 1, LinkControl::Active)),
            at(200)
        ),
        Err(Error::Revoked)
    );
    assert!(
        reduce(&revoked.state, &f, Event::Advance, at(300))
            .unwrap()
            .effects
            .is_empty()
    );
    assert_eq!(
        reduce(
            &revoked.state,
            &facts(Phase::MayHaveWritten),
            Event::Advance,
            at(300)
        ),
        Err(Error::InconsistentFacts)
    );
    let outside = State::new(s.identity.clone(), at(-101), 3600).unwrap();
    assert_eq!(
        apply(&outside, &f, revoke(&outside, 2, false, Some(window(1))))
            .state
            .disposition(),
        &Disposition::Ready
    );
    let future = State::new(s.identity.clone(), at(101), 3600).unwrap();
    assert_eq!(
        apply(&future, &f, revoke(&future, 2, false, Some(window(1))))
            .state
            .disposition(),
        &Disposition::Ready
    );
}
#[test]
fn unknown_is_never_held_aborted_or_released_and_resolution_keeps_intent() {
    let s = state();
    let f = Facts {
        budget: BudgetDisposition::ChargedOrUncertain,
        ..facts(Phase::MayHaveWritten)
    };
    let held = apply(
        &s,
        &f,
        envelope(&s, 1, 1, LinkControl::AutoPause { window: window(1) }),
    );
    assert_eq!(held.state.disposition(), &Disposition::RecoverUnknown);
    assert_eq!(held.effects, vec![Effect::RecoverReadOnly]);
    let rev = revoke(&s, 2, true, Some(window(1)));
    let revoked = apply(&held.state, &f, rev.clone());
    assert_eq!(revoked.effects, vec![Effect::RecoverReadOnly]);
    assert!(apply(&revoked.state, &f, rev).effects.is_empty());
    for _ in 0..3 {
        let absent = reduce(&revoked.state, &f, Event::Advance, at(300)).unwrap();
        assert_eq!(absent.state.disposition(), &Disposition::RecoverUnknown);
        assert!(absent.effects.is_empty());
    }
    let resolved = facts(Phase::OwnedInvitation {
        upstream_id: 20,
        created_at: at(10),
    });
    let cancelled = reduce(&revoked.state, &resolved, Event::Advance, at(300)).unwrap();
    assert_eq!(
        cancelled.effects,
        vec![Effect::RequestCancellation {
            upstream_id: 20,
            reason: CancelReason::LinkRevoked
        }]
    );
    assert!(
        reduce(&cancelled.state, &resolved, Event::Advance, at(300))
            .unwrap()
            .effects
            .is_empty()
    );
    // A positively proven no-write result can finally abort; list absence cannot.
    let stopped = reduce(
        &revoked.state,
        &facts(Phase::NoWriteProven),
        Event::Advance,
        at(300),
    )
    .unwrap();
    assert_eq!(stopped.state.disposition(), &Disposition::Aborted);
}
#[test]
fn default_revoke_leaves_unheld_and_inflight_work_running() {
    for phase in [
        Phase::NoWriteProven,
        Phase::MayHaveWritten,
        Phase::OwnedInvitation {
            upstream_id: 20,
            created_at: at(10),
        },
        Phase::ForeignInvitation { upstream_id: 20 },
        Phase::Final,
    ] {
        let s = state();
        let f = facts(phase.clone());
        let result = apply(&s, &f, revoke(&s, 2, false, None));
        assert!(!result.effects.iter().any(|e| matches!(
            e,
            Effect::RecordAborted
                | Effect::RequestCancellation { .. }
                | Effect::StopTrackingForeign
                | Effect::ReleaseUnusedSlot { .. }
        )));
    }
}
#[test]
fn owned_foreign_and_terminal_receipts_have_distinct_dispositions() {
    for (phase, expected) in [
        (
            Phase::OwnedInvitation {
                upstream_id: 20,
                created_at: at(10),
            },
            vec![Effect::RequestCancellation {
                upstream_id: 20,
                reason: CancelReason::LinkRevoked,
            }],
        ),
        (
            Phase::ForeignInvitation { upstream_id: 20 },
            vec![Effect::StopTrackingForeign],
        ),
        (Phase::Final, vec![]),
    ] {
        let s = state();
        let f = facts(phase);
        let original = f.clone();
        let result = apply(&s, &f, revoke(&s, 1, true, None));
        assert_eq!(result.effects, expected);
        assert_eq!(
            f, original,
            "facts and historical receipts are never mutated"
        );
        assert!(
            reduce(&result.state, &f, Event::Advance, at(300))
                .unwrap()
                .effects
                .is_empty()
        );
    }
}
#[test]
fn identity_revision_and_equal_revision_payload_conflicts_fail_closed() {
    let s = state();
    let f = facts(Phase::NoWriteProven);
    let e = envelope(&s, 1, 0, LinkControl::ManualPause);
    let first = apply(&s, &f, e.clone());
    let mut changed = e.clone();
    changed.control = LinkControl::Active;
    assert_eq!(
        reduce(&first.state, &f, Event::Apply(changed), at(200)),
        Err(Error::RevisionConflict)
    );
    for field in 0..5 {
        let mut wrong = e.clone();
        match field {
            0 => wrong.identity.link_id = Some(InvitationLinkId::new()),
            1 => wrong.identity.request_id = RequestId::new(),
            2 => wrong.identity.kind = DeliveryKind::Organization,
            3 => wrong.identity.target_id += 1,
            _ => wrong.identity.delivery_generation += 1,
        }
        assert_eq!(
            reduce(&s, &f, Event::Apply(wrong), at(200)),
            Err(Error::IdentityConflict)
        );
    }
    let mut zero = e.clone();
    zero.revision = 0;
    assert_eq!(
        reduce(&s, &f, Event::Apply(zero), at(200)),
        Err(Error::Invalid)
    );
    let mut invalid = e;
    invalid.revision = 2;
    invalid.hold_epoch = 3;
    assert_eq!(
        reduce(&s, &f, Event::Apply(invalid), at(200)),
        Err(Error::Invalid)
    );
}
fn expiry_ready() -> (State, Facts) {
    let s = state();
    let f = facts(Phase::OwnedInvitation {
        upstream_id: 20,
        created_at: at(10),
    });
    let result = reduce(&s, &f, Event::Advance, at(100)).unwrap();
    assert_eq!(
        result.effects,
        vec![Effect::ScheduleOwnExpiry {
            generation: 1,
            upstream_id: 20,
            at: at(3610)
        }]
    );
    assert!(
        reduce(&result.state, &f, Event::Advance, at(200))
            .unwrap()
            .effects
            .is_empty()
    );
    (result.state, f)
}
fn expiry_observation(
    generation: u64,
    expected_revision: u64,
    observed_at: DateTime<Utc>,
    evidence: ExpiryEvidence,
) -> Event {
    Event::OwnExpiryObserved {
        generation,
        upstream_id: 20,
        expected_revision,
        observed_at,
        evidence,
    }
}
#[test]
fn own_expiry_requires_due_current_timer_and_fresh_complete_evidence() {
    let (s, f) = expiry_ready();
    for (generation, id, now) in [(0, 20, at(4000)), (1, 21, at(4000)), (1, 20, at(3609))] {
        assert!(
            reduce(
                &s,
                &f,
                Event::TickOwnExpiry {
                    generation,
                    upstream_id: id
                },
                now
            )
            .unwrap()
            .effects
            .is_empty()
        );
    }
    let tick = reduce(
        &s,
        &f,
        Event::TickOwnExpiry {
            generation: 1,
            upstream_id: 20,
        },
        at(3610),
    )
    .unwrap();
    assert_eq!(
        tick.effects,
        vec![Effect::ObserveOwnExpiry {
            generation: 1,
            upstream_id: 20
        }]
    );
    assert!(
        reduce(
            &tick.state,
            &f,
            Event::TickOwnExpiry {
                generation: 1,
                upstream_id: 20
            },
            at(3700)
        )
        .unwrap()
        .effects
        .is_empty()
    );
    for event in [
        expiry_observation(1, 1, at(3620), ExpiryEvidence::StillPending),
        expiry_observation(1, 2, at(3609), ExpiryEvidence::StillPending),
        expiry_observation(1, 2, at(4001), ExpiryEvidence::StillPending),
    ] {
        let stale = reduce(&tick.state, &f, event, at(4000)).unwrap();
        assert_eq!(
            stale.effects,
            vec![Effect::ScheduleOwnExpiry {
                generation: 2,
                upstream_id: 20,
                at: at(4060)
            }]
        );
        assert_eq!(stale.state.own_expiry().unwrap().due_at, at(3610));
        assert!(!stale.state.own_expiry_due(2, 20, at(4059)));
        assert!(
            reduce(
                &stale.state,
                &f,
                Event::TickOwnExpiry {
                    generation: 2,
                    upstream_id: 20
                },
                at(4059)
            )
            .unwrap()
            .effects
            .is_empty()
        );
        assert!(stale.state.own_expiry_due(2, 20, at(4060)));
    }
    let stale = Facts {
        last_observed_at: Some(at(3800)),
        ..f.clone()
    };
    assert_eq!(
        reduce(
            &tick.state,
            &stale,
            expiry_observation(1, 2, at(3700), ExpiryEvidence::StillPending),
            at(4000)
        )
        .unwrap()
        .effects,
        vec![Effect::ScheduleOwnExpiry {
            generation: 2,
            upstream_id: 20,
            at: at(4060)
        }]
    );
    let pending = reduce(
        &tick.state,
        &f,
        expiry_observation(1, 2, at(3620), ExpiryEvidence::StillPending),
        at(3620),
    )
    .unwrap();
    assert_eq!(
        pending.effects,
        vec![Effect::RequestCancellation {
            upstream_id: 20,
            reason: CancelReason::OwnExpiry
        }]
    );
    assert!(
        reduce(
            &pending.state,
            &f,
            expiry_observation(1, 2, at(3620), ExpiryEvidence::StillPending),
            at(3620)
        )
        .unwrap()
        .effects
        .is_empty()
    );
    let accepted = reduce(
        &tick.state,
        &f,
        expiry_observation(1, 2, at(3620), ExpiryEvidence::Accepted),
        at(3620),
    )
    .unwrap();
    assert_eq!(accepted.effects, vec![Effect::RecordAccepted]);
}
#[test]
fn missing_evidence_retries_without_inventing_cancellation_or_extending_expiry() {
    let (s, f) = expiry_ready();
    let tick = reduce(
        &s,
        &f,
        Event::TickOwnExpiry {
            generation: 1,
            upstream_id: 20,
        },
        at(3610),
    )
    .unwrap();
    for evidence in [ExpiryEvidence::Absent, ExpiryEvidence::Unavailable] {
        let retry = reduce(
            &tick.state,
            &f,
            expiry_observation(1, 2, at(3620), evidence),
            at(3620),
        )
        .unwrap();
        assert_eq!(retry.state.own_expiry().unwrap().due_at, at(3610));
        assert!(retry.effects.contains(&Effect::ScheduleOwnExpiry {
            generation: 2,
            upstream_id: 20,
            at: at(3680)
        }));
        assert!(!retry.effects.iter().any(|e| matches!(
            e,
            Effect::RequestCancellation { .. } | Effect::RecordAccepted
        )));
        assert!(
            reduce(
                &retry.state,
                &f,
                Event::TickOwnExpiry {
                    generation: 1,
                    upstream_id: 20
                },
                at(4000)
            )
            .unwrap()
            .effects
            .is_empty()
        );
    }
}
#[test]
fn foreign_and_default_seven_day_invites_have_no_own_expiry_timer() {
    for (seconds, phase) in [
        (3600, Phase::ForeignInvitation { upstream_id: 20 }),
        (
            604800,
            Phase::OwnedInvitation {
                upstream_id: 20,
                created_at: at(10),
            },
        ),
    ] {
        let s = State::new(identity(), at(0), seconds).unwrap();
        let f = facts(phase);
        for event in [
            Event::Advance,
            Event::TickOwnExpiry {
                generation: 1,
                upstream_id: 20,
            },
            expiry_observation(1, 2, at(4000), ExpiryEvidence::StillPending),
        ] {
            let result = reduce(&s, &f, event, at(4000)).unwrap();
            assert!(result.state.own_expiry().is_none());
            assert!(result.effects.is_empty());
        }
    }
}
#[test]
fn envelope_permutations_converge_without_reviving_a_revoked_window() {
    let s = state();
    let f = facts(Phase::NoWriteProven);
    let controls = [
        envelope(&s, 1, 1, LinkControl::AutoPause { window: window(1) }),
        envelope(&s, 2, 1, LinkControl::Active),
        envelope(&s, 3, 3, LinkControl::AutoPause { window: window(3) }),
        revoke(&s, 4, false, Some(window(3))),
    ];
    // Enumerate all24 permutations, including revoke before unseen hold/release.
    for a in 0..4 {
        for b in 0..4 {
            for c in 0..4 {
                for d in 0..4 {
                    if [a, b, c, d]
                        .iter()
                        .enumerate()
                        .any(|(i, v)| [a, b, c, d][..i].contains(v))
                    {
                        continue;
                    }
                    let mut current = s.clone();
                    for i in [a, b, c, d] {
                        current = apply(&current, &f, controls[i].clone()).state;
                    }
                    assert_eq!(current.disposition(), &Disposition::Aborted);
                    assert_eq!(current.accepted.as_ref().unwrap(), &controls[3]);
                }
            }
        }
    }
}

#[test]
fn portable_numeric_bounds_and_creation_time_are_not_silently_changed() {
    let s = state();
    for revision in [0, crate::storage::integer::MAX_EXACT + 1] {
        let f = Facts {
            delivery_revision: revision,
            ..facts(Phase::NoWriteProven)
        };
        assert_eq!(reduce(&s, &f, Event::Advance, at(200)), Err(Error::Invalid));
    }
    for upstream_id in [0, crate::storage::integer::MAX_EXACT + 1] {
        for phase in [
            Phase::OwnedInvitation {
                upstream_id,
                created_at: at(10),
            },
            Phase::ForeignInvitation { upstream_id },
        ] {
            assert_eq!(
                reduce(&s, &facts(phase), Event::Advance, at(200)),
                Err(Error::Invalid)
            );
        }
    }
    let (s, mut f) = expiry_ready();
    f.phase = Phase::OwnedInvitation {
        upstream_id: 20,
        created_at: at(11),
    };
    assert_eq!(
        reduce(&s, &f, Event::Advance, at(200)),
        Err(Error::InconsistentFacts)
    );
}

#[test]
fn retained_json_roundtrip_preserves_controls_and_effect_deduplication() {
    let s = state();
    let f = facts(Phase::NoWriteProven);
    let command = envelope(&s, 1, 1, LinkControl::AutoPause { window: window(1) });
    let held = apply(&s, &f, command.clone());
    let retained: State =
        serde_json::from_slice(&serde_json::to_vec(&held.state).unwrap()).unwrap();
    assert_eq!(retained, held.state);
    assert!(apply(&retained, &f, command).effects.is_empty());
    let revoked = apply(&retained, &f, revoke(&retained, 2, false, Some(window(1))));
    assert_eq!(revoked.state.disposition(), &Disposition::Aborted);

    let (s, f) = expiry_ready();
    let tick = reduce(
        &s,
        &f,
        Event::TickOwnExpiry {
            generation: 1,
            upstream_id: 20,
        },
        at(3610),
    )
    .unwrap();
    let retained: State =
        serde_json::from_slice(&serde_json::to_vec(&tick.state).unwrap()).unwrap();
    let pending = reduce(
        &retained,
        &f,
        expiry_observation(1, 2, at(3620), ExpiryEvidence::StillPending),
        at(3620),
    )
    .unwrap();
    assert_eq!(
        pending.effects,
        vec![Effect::RequestCancellation {
            upstream_id: 20,
            reason: CancelReason::OwnExpiry
        }]
    );
    let retained: State =
        serde_json::from_slice(&serde_json::to_vec(&pending.state).unwrap()).unwrap();
    assert!(
        reduce(
            &retained,
            &f,
            expiry_observation(1, 2, at(3620), ExpiryEvidence::StillPending),
            at(3620)
        )
        .unwrap()
        .effects
        .is_empty()
    );
}

#[test]
fn revoke_cannot_resurrect_released_hold_or_change_an_observed_window() {
    let s = state();
    let f = facts(Phase::NoWriteProven);
    let held = apply(
        &s,
        &f,
        envelope(&s, 1, 1, LinkControl::AutoPause { window: window(1) }),
    );
    let released = apply(
        &held.state,
        &f,
        envelope(&held.state, 2, 1, LinkControl::Active),
    );
    assert_eq!(
        reduce(
            &released.state,
            &f,
            Event::Apply(revoke(&released.state, 3, false, Some(window(1)))),
            at(200)
        ),
        Err(Error::RevisionConflict)
    );
    let mut altered = window(1);
    altered.window_start = at(-99);
    assert_eq!(
        reduce(
            &held.state,
            &f,
            Event::Apply(revoke(&held.state, 2, false, Some(altered))),
            at(200)
        ),
        Err(Error::RevisionConflict)
    );
    let unseen = apply(
        &released.state,
        &f,
        revoke(&released.state, 4, false, Some(window(3))),
    );
    assert_eq!(unseen.state.disposition(), &Disposition::Aborted);
}

#[test]
fn expiry_evidence_must_postdate_the_current_live_read_request() {
    let (s, f) = expiry_ready();
    let tick = reduce(
        &s,
        &f,
        Event::TickOwnExpiry {
            generation: 1,
            upstream_id: 20,
        },
        at(5000),
    )
    .unwrap();
    let stale = reduce(
        &tick.state,
        &f,
        expiry_observation(1, 2, at(4000), ExpiryEvidence::StillPending),
        at(5001),
    )
    .unwrap();
    assert_eq!(
        stale.effects,
        vec![Effect::ScheduleOwnExpiry {
            generation: 2,
            upstream_id: 20,
            at: at(5061)
        }]
    );
    assert_eq!(stale.state.own_expiry().unwrap().due_at, at(3610));
}
