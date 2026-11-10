use super::*;
use ghinvite_core::storage::{OperationalReadStorage, operational::SignalRow};

async fn rows(f: &Fixture, c: &OrganizationCreateCommand) -> Vec<SignalRow> {
    f.storage
        .delivery_signals(Utc::now())
        .await
        .unwrap()
        .steps
        .into_iter()
        .filter(|row| row.request_id == Some(c.request_id))
        .collect()
}
async fn projected(f: &Fixture, c: &OrganizationCreateCommand, unknown: bool) -> Vec<SignalRow> {
    tokio::time::timeout(std::time::Duration::from_secs(20), async {
        loop {
            let result = rows(f, c).await;
            if result.len() == c.teams.len() + 1
                && result.iter().all(|row| {
                    row.last_observed_at.is_some() && (!unknown || row.unknown_since.is_some())
                })
            {
                return result;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("same-key receipt and telemetry projections converge")
}

#[tokio::test]
async fn first_unknown_clocks_survive_replays_and_projection_outage_then_confirmation() {
    let f = Fixture::new(Scenario {
        writes: VecDeque::from([502]),
        ..Default::default()
    })
    .await;
    f.faults
        .projection_unavailable
        .store(true, Ordering::SeqCst);
    let c = f.command(&[5, 6]).await;
    let unknown = f.create(&c).await;
    let first_completed = Utc::now();
    assert_eq!(
        unknown.create.outcome,
        OrganizationCreateOutcome::OutcomeUnknown
    );
    for _ in 0..3 {
        assert_eq!(f.create(&c).await, unknown);
    }
    f.faults.lose_projection_ack.store(true, Ordering::SeqCst);
    f.faults
        .projection_unavailable
        .store(false, Ordering::SeqCst);
    let initial = projected(&f, &c, true).await;
    assert!(
        initial
            .iter()
            .all(|r| r.unknown_since.unwrap() <= first_completed)
    );
    assert!(
        initial
            .iter()
            .all(|r| r.unknown_since.unwrap() >= c.approved_at)
    );
    let at = initial
        .iter()
        .map(|r| (&r.step, r.unknown_since))
        .collect::<BTreeMap<_, _>>();
    let signals = f
        .storage
        .delivery_signals(first_completed + chrono::Duration::hours(2))
        .await
        .unwrap();
    assert_eq!(signals.counts.unknown.total, 3);
    assert_eq!(signals.counts.unknown.over_threshold, 3);
    assert!(!signals.incomplete());
    {
        let mut gh = f.github.0.lock().unwrap();
        gh.pending = true;
        gh.org = Some(("member", "pending"));
        gh.attached = vec![5, 6];
        gh.created_at = Some(Utc::now());
    }
    let created = f.create(&c).await;
    assert!(matches!(
        created.create.outcome,
        OrganizationCreateOutcome::Created { .. }
    ));
    tokio::time::timeout(std::time::Duration::from_secs(20), async {
        loop {
            let next = rows(&f, &c).await;
            if next.len() == 3 && next.iter().all(|r| r.create_outcome == "created") {
                for row in next {
                    assert_eq!(row.unknown_since, at[&row.step]);
                }
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(f.create(&c).await, created);
    assert_eq!(f.writes("POST").len(), 1);
}

#[tokio::test]
async fn failed_independent_team_reads_do_not_refresh_team_watermark_or_receipt() {
    let f = Fixture::new(Scenario {
        org: Some(("admin", "pending")),
        pending: true,
        ..Default::default()
    })
    .await;
    let c = f.command(&[5]).await;
    let created = f.create(&c).await;
    assert!(matches!(
        created.teams[&5].create.outcome,
        TeamCreateOutcome::Created {
            via: TeamCreation::PendingMembership
        }
    ));
    let first = projected(&f, &c, false).await;
    let team_at = first
        .iter()
        .find(|r| r.step == "team:5")
        .unwrap()
        .last_observed_at;
    f.github.0.lock().unwrap().deny_team_reads = true;
    let observed_at = Utc::now();
    f.call(&c, "observe_listed", observed(&c, observed_at, true))
        .await;
    tokio::time::timeout(std::time::Duration::from_secs(20), async {
        loop {
            let current = rows(&f, &c).await;
            if current
                .iter()
                .any(|r| r.step == "org" && r.last_observed_at == Some(observed_at))
            {
                assert_eq!(
                    current
                        .iter()
                        .find(|r| r.step == "team:5")
                        .unwrap()
                        .last_observed_at,
                    team_at
                );
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let current: OrganizationDeliverySnapshot = serde_json::from_value(
        standard_call(
            &f,
            "OrganizationDelivery",
            &c.delivery_key(),
            "status",
            None,
        )
        .await,
    )
    .unwrap();
    assert_eq!(current.create, created.create);
    assert_eq!(current.teams[&5], created.teams[&5]);
    // Future and pre-watermark input cannot make any evidence fresh.
    let before = rows(&f, &c).await;
    f.call(
        &c,
        "observe_listed",
        observed(&c, observed_at + chrono::Duration::hours(1), false),
    )
    .await;
    f.call(&c, "observe_listed", observed(&c, c.approved_at, false))
        .await;
    assert_eq!(rows(&f, &c).await, before);
    assert_eq!(f.writes("PUT").len(), 1);
}
