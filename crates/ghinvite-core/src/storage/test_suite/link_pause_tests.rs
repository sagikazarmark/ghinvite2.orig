use super::*;
use crate::link_lifecycle::{self, LifecycleAction, LifecycleRefusal, PauseReason};

fn decide(
    link: &crate::invitation_link::LinkSnapshot,
    action: LifecycleAction,
    actor: u64,
    now: DateTime<Utc>,
) -> link_lifecycle::LifecycleOutcome {
    link_lifecycle::decide(link, action, actor, now).unwrap()
}

pub async fn scenario_link_pause<S: Storage + ProjectionStorage>(s: S) {
    let now = dt("2026-05-04T13:00:00Z");
    s.put_app_installation(&crate::account::Installation::repo(
        (sample_account(1, 100, "acme")).clone(),
    ))
    .await
    .unwrap();
    s.upsert_user(&sample_user(7, "owner")).await.unwrap();
    let mut original = sample_link(100, 1, 7);
    original.invitation_lifetime_seconds = 3600;
    original.abuse_threshold_per_hour = Some(2);
    let first = fixture::envelope(&original, 1);
    s.apply_transition(&first).await.unwrap();
    assert_eq!(
        decide(&first.link, LifecycleAction::Resume, 7, now),
        Err(LifecycleRefusal::NotPaused)
    );
    for revision in [0, crate::storage::integer::MAX_EXACT, u64::MAX] {
        let mut invalid = first.link.clone();
        invalid.revision = revision;
        assert_eq!(
            link_lifecycle::decide(&invalid, LifecycleAction::Pause, 7, now),
            Err(link_lifecycle::InvalidLifecycleState)
        );
    }
    let paused = decide(&first.link, LifecycleAction::Pause, 7, now).unwrap();
    assert_eq!(paused.creation, first.link.creation);
    assert_eq!(paused.uses, first.link.uses);
    assert_eq!(paused.inactive(now), Some(crate::Inactive::Paused));
    assert_eq!(
        decide(&paused, LifecycleAction::Pause, 7, now),
        Err(LifecycleRefusal::AlreadyPaused)
    );
    let mut envelope = first.clone();
    envelope.link = paused.clone();
    envelope.transition_id = "pause/2".into();
    s.apply_transition(&envelope).await.unwrap();
    let row = s
        .get_invitation_link_by_id(original.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.pause, paused.pause);
    assert_eq!(row.invitation_lifetime_seconds, 3600);
    assert_eq!(row.abuse_threshold_per_hour, Some(2));
    for (lifetime, threshold) in [(7200, Some(2)), (3600, Some(3))] {
        let mut changed = envelope.clone();
        changed.link.revision += 1;
        changed.link.creation.invitation_lifetime_seconds = lifetime;
        changed.link.creation.abuse_threshold_per_hour = threshold;
        assert!(
            s.apply_transition(&changed).await.is_err(),
            "creation guardrails are immutable"
        );
    }
    assert!(!row.is_active(now));
    assert_eq!(row.state(), crate::invitation_link::LinkState::Paused);
    s.apply_transition(&first).await.unwrap();
    assert_eq!(
        s.get_invitation_link_by_id(original.id)
            .await
            .unwrap()
            .unwrap()
            .pause,
        paused.pause
    );
    let mut conflict = envelope.clone();
    conflict.link.pause.as_mut().unwrap().by = Some(8);
    assert!(s.apply_transition(&conflict).await.is_err());
    let mut exhausted = paused.clone();
    exhausted.uses = 1000;
    assert_eq!(
        decide(&exhausted, LifecycleAction::Resume, 7, now),
        Err(LifecycleRefusal::ExpiredOrExhausted)
    );
    let mut expired = paused.clone();
    expired.creation.expires_at = Some(now);
    assert_eq!(
        decide(&expired, LifecycleAction::Resume, 7, now),
        Err(LifecycleRefusal::ExpiredOrExhausted)
    );
    let resumed = decide(&paused, LifecycleAction::Resume, 7, now).unwrap();
    envelope.link = resumed.clone();
    envelope.transition_id = "resume/3".into();
    s.apply_transition(&envelope).await.unwrap();
    let row = s
        .get_invitation_link_by_id(original.id)
        .await
        .unwrap()
        .unwrap();
    assert!(row.pause.is_none());
    assert!(row.is_active(now));
    // The typed storage representation also preserves a distinct abuse window.
    let mut automatic = decide(&resumed, LifecycleAction::Pause, 7, now).unwrap();
    let pause = automatic.pause.as_mut().unwrap();
    pause.by = None;
    pause.reason = PauseReason::AbuseSuspected {
        window_start: now - chrono::Duration::hours(1),
    };
    envelope.link = automatic.clone();
    envelope.transition_id = "auto-pause/4".into();
    s.apply_transition(&envelope).await.unwrap();
    assert_eq!(
        s.get_invitation_link_by_id(original.id)
            .await
            .unwrap()
            .unwrap()
            .pause,
        automatic.pause
    );
    automatic.revoked_at = Some(now);
    automatic.revoked_by = Some(7);
    automatic.revision += 1;
    assert_eq!(
        decide(&automatic, LifecycleAction::Resume, 7, now),
        Err(LifecycleRefusal::Revoked)
    );
    envelope.link = automatic;
    envelope.transition_id = "revoke/5".into();
    s.apply_transition(&envelope).await.unwrap();
    let row = s
        .get_invitation_link_by_id(original.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.state(), crate::invitation_link::LinkState::Revoked);
    assert!(!row.is_active(now));
}
