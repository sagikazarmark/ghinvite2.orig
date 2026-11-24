//! The hourly cron's sweep watchdog (#86) over real scoped SQL and a recorded
//! authenticated ingress: the account set comes from active installations,
//! batches stay within the workflow's bound, and only complete checks reach
//! the daily digest.
use chrono::Utc;
use ghinvite_core::{
    Account, AccountType, SelectedRepos,
    account::{App, Installation},
    storage::{AppInstallationStorage, blocked_digest::DailyDigestStorage},
    sweep_health::{OverdueSchedule, WatchdogReport},
};
use ghinvite_web::{
    restate_client::{RestateAuth, RestateClient},
    sweep_watchdog::{WatchdogFailure, check_and_record},
};
use serde_json::{Value, json};
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

const API_KEY: &str = "watchdog-ingress-key";

fn installation(
    app: App,
    installation_id: u64,
    account_id: u64,
    uninstalled: bool,
) -> Installation {
    let mut installation = Installation::repo(Account {
        installation_id,
        account_id,
        account_login: format!("account-{account_id}"),
        account_type: AccountType::Organization,
        installed_at: "2026-05-04T12:00:00Z".parse().unwrap(),
        uninstalled_at: uninstalled.then(Utc::now),
        selected_repos: SelectedRepos::All,
    });
    installation.app = app;
    installation
}

async fn storage() -> ghinvite_storage_sqlx::SqlxStorage {
    let storage = ghinvite_storage_sqlx::SqlxStorage::in_memory()
        .await
        .unwrap();
    // 150 repository accounts span two batches; a reinstalled account appears
    // once; an uninstalled account and the organization app's account do not
    // unless organization sweeps are on.
    for n in 1..=150 {
        storage
            .put_app_installation(&installation(App::Repo, n, 1000 + n, false))
            .await
            .unwrap();
    }
    storage
        .put_app_installation(&installation(App::Repo, 151, 1001, true))
        .await
        .unwrap();
    storage
        .put_app_installation(&installation(App::Repo, 152, 9000, true))
        .await
        .unwrap();
    storage
        .put_app_installation(&installation(App::Org, 153, 1001, false))
        .await
        .unwrap();
    storage
}

fn accounts(request: &Request) -> Vec<(String, u64)> {
    let body: Value = serde_json::from_slice(&request.body).unwrap();
    body["accounts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| {
            (
                a["app"].as_str().unwrap().to_owned(),
                a["account_id"].as_u64().unwrap(),
            )
        })
        .collect()
}

#[tokio::test]
async fn active_accounts_are_checked_in_bounded_authenticated_batches_and_recorded() {
    let storage = storage().await;
    let ingress = MockServer::start().await;
    let due: chrono::DateTime<Utc> = "2026-05-01T00:00:00Z".parse().unwrap();
    Mock::given(method("POST"))
        .and(path("/SweepWatchdog/check"))
        .and(header(
            "authorization",
            format!("Bearer {API_KEY}").as_str(),
        ))
        .respond_with(move |request: &Request| {
            let accounts = accounts(request);
            ResponseTemplate::new(200).set_body_json(WatchdogReport {
                checked: accounts.len() as u64,
                overdue: 1,
                oldest_overdue: Some(OverdueSchedule {
                    account: ghinvite_core::account::AccountInstallationRef {
                        app: App::Repo,
                        account_id: accounts[0].1,
                    },
                    due_at: due + chrono::Duration::hours(accounts[0].1 as i64 - 1000),
                }),
                ..Default::default()
            })
        })
        .expect(2)
        .mount(&ingress)
        .await;
    let restate = RestateClient::with_auth(
        ingress.uri(),
        RestateAuth::from_config(None, Some(API_KEY)).unwrap(),
    )
    .unwrap();
    let now = Utc::now();
    let report = check_and_record(&storage, &restate, false, now)
        .await
        .unwrap();
    let batches = ingress
        .received_requests()
        .await
        .unwrap()
        .iter()
        .map(accounts)
        .collect::<Vec<_>>();
    assert_eq!(
        batches.iter().map(Vec::len).collect::<Vec<_>>(),
        vec![100, 50]
    );
    let all = batches.concat();
    assert!(all.iter().all(|(app, _)| app == "repo"));
    assert_eq!(
        all.iter().map(|(_, id)| *id).collect::<Vec<_>>(),
        (1001..=1150).collect::<Vec<_>>(),
        "each active account once; uninstalled accounts are not watched"
    );
    assert_eq!((report.checked, report.overdue), (150, 2));
    assert_eq!(
        report.oldest_overdue.unwrap().account.account_id,
        1001,
        "the oldest overdue chain across batches"
    );
    let digest = storage
        .sweep_digest(now.date_naive())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (digest.checks, digest.schedules, digest.rearmed),
        (1, 150, 2)
    );
    assert_eq!(
        digest.oldest_overdue.unwrap().1,
        due + chrono::Duration::hours(1)
    );
}

#[tokio::test]
async fn organization_accounts_join_only_with_organization_sweeps() {
    let storage = storage().await;
    let ingress = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/SweepWatchdog/check"))
        .respond_with(ResponseTemplate::new(200).set_body_json(WatchdogReport::default()))
        .mount(&ingress)
        .await;
    let restate = RestateClient::new(ingress.uri()).unwrap();
    check_and_record(&storage, &restate, true, Utc::now())
        .await
        .unwrap();
    let all = ingress
        .received_requests()
        .await
        .unwrap()
        .iter()
        .flat_map(accounts)
        .collect::<Vec<_>>();
    assert_eq!(all.len(), 151);
    assert_eq!(all.last(), Some(&("org".to_owned(), 1001)));
}

#[tokio::test]
async fn a_failed_batch_is_retried_next_hour_and_records_nothing() {
    let storage = storage().await;
    let ingress = MockServer::start().await;
    // The first batch is answered, the second refused.
    Mock::given(method("POST"))
        .and(path("/SweepWatchdog/check"))
        .respond_with(|request: &Request| {
            if accounts(request).len() == 100 {
                ResponseTemplate::new(200).set_body_json(WatchdogReport::default())
            } else {
                ResponseTemplate::new(503)
                    .set_body_json(json!({"message":"private-ingress-marker"}))
            }
        })
        .expect(2)
        .mount(&ingress)
        .await;
    let restate = RestateClient::new(ingress.uri()).unwrap();
    let now = Utc::now();
    assert_eq!(
        check_and_record(&storage, &restate, false, now)
            .await
            .unwrap_err(),
        WatchdogFailure::Ingress,
        "every batch is still tried"
    );
    assert!(
        storage
            .sweep_digest(now.date_naive())
            .await
            .unwrap()
            .is_none()
    );
}
