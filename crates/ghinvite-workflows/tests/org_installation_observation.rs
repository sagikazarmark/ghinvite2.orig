//! Actual Restate + HTTP proof for app-specific organization scope observation.
//!
//! GitHub is the octoemu emulator: the repository App installed on acme for
//! `acme/api` (1) and the organization App (2). A suspension and an
//! uninstall are GitHub's own (admin API); an installation or organization
//! reported under another account and an outage are fault rules. The call
//! log checks every request: only the expected routes, each with its App's
//! credential and exact token profile.
#![cfg(feature = "integration")]
use ghinvite_e2e::{Call, Harness};
use octoemu::calls::{Call as GithubCall, CredentialKind, Identity};
use octoemu::faults::{Fault, FaultAction};
use serde_json::{Value, json};
use std::sync::Arc;

/// The installation whose token the call carried, if it did.
fn installation_of(call: &GithubCall) -> Option<u64> {
    match call.identity {
        Some(Identity::Installation {
            installation_id, ..
        }) => Some(installation_id),
        _ => None,
    }
}

async fn invoke(
    h: &Harness,
    service: &str,
    key: &str,
    handler: &str,
    input: Option<&Value>,
) -> Value {
    let result = h
        .restate
        .invoke(&Call::object(service, key, handler), input, None)
        .await;
    assert_eq!(result.status, 200, "{handler}: {}", result.body);
    result.body
}
#[tokio::test]
async fn organization_scope_has_its_own_read_profile_and_fail_closed_observations() {
    let mut world = ghinvite_e2e::github::seed();
    for installation in &mut world.installations {
        if installation.id == 1 {
            *installation = installation.clone().selected(["acme/api"]);
        }
    }
    let h = Harness::start_with_emulator(ghinvite_e2e::github_emulator_with(world).await).await;
    let emulator = h.emulator();
    let github = h.github();
    let storage = Arc::new(
        ghinvite_storage_sqlx::SqlxStorage::in_memory()
            .await
            .unwrap(),
    );
    let github_transport = ghinvite_e2e::github_transport(h.client.clone());
    let state = ghinvite_workflows::AppState::new(storage).with_github_apps(Arc::new(
        ghinvite_workflows::RepositoryGithub::new(
            github_transport,
            &h.github_url,
            ghinvite_e2e::installed_apps(&h.github_url, true),
        )
        .unwrap(),
    ));
    h.restate
        .deploy(
            restate_sdk::endpoint::Endpoint::builder()
                .bind(ghinvite_workflows::sweep_schedule::SweepSchedule::default())
                .bind(ghinvite_workflows::installation::Installation {
                    state: state.clone(),
                })
                .bind(ghinvite_workflows::availability::AccountInstallation {
                    state: state.clone(),
                })
                .bind(ghinvite_workflows::availability::InstallationProjection { state })
                .build(),
        )
        .await;
    for (app, id) in [("repo", 1), ("org", 2)] {
        let input = json!({"operation_id":ghinvite_core::RequestId::new(),"verified":{"app":app,"account":{"installation_id":id,"account_id":100,"account_login":"acme","account_type":"Organization","selected_repos":[],"installed_at":"2026-01-01T00:00:00Z","uninstalled_at":null},"suspended_at":null,"permissions":{}}});
        invoke(
            &h,
            "Installation",
            &format!("{app}:{id}"),
            "onboard",
            Some(&input),
        )
        .await;
        invoke(
            &h,
            "AccountInstallation",
            &format!("{app}:100"),
            "refresh",
            Some(&json!(id)),
        )
        .await;
    }
    let org = invoke(&h, "AccountInstallation", "org:100", "status", None).await;
    let repo = invoke(&h, "AccountInstallation", "repo:100", "status", None).await;
    assert_eq!(
        org["observation"],
        json!({"kind":"available","repo_ids":[]})
    );
    assert_eq!(
        repo["observation"],
        json!({"kind":"available","repo_ids":[10]})
    );
    assert_eq!(org["installation"]["app"], "org");
    assert_eq!(repo["installation"]["app"], "repo");
    let minted = |id: u64| {
        emulator.calls().iter().any(|call| {
            call.method == "POST"
                && call.path == format!("/app/installations/{id}/access_tokens")
                && call.status == 201
        })
    };
    assert!(minted(1));
    assert!(minted(2));
    // Each mode is a GitHub-side change, undone after its refresh.
    let installation_2 = |account: u64| json!({"id":2,"app_id":456,"account":{"id":account,"login":"acme","type":"Organization"},"created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-01T00:00:00Z","suspended_at":null,"repository_selection":"all","permissions":{"members":"write","metadata":"read"}});
    for (mode, expected) in [
        ("suspended", "unavailable"),
        ("account_mismatch", "unavailable"),
        ("org_mismatch", "unavailable"),
        ("upstream_failure", "unknown"),
    ] {
        let rule = match mode {
            "suspended" => {
                assert!(emulator.suspend_installation(2));
                None
            }
            // GitHub reports the installation under another account.
            "account_mismatch" => Some(github.rule(
                mode,
                Fault::on(
                    "GET",
                    "/app/installations/2",
                    FaultAction::respond_json(200, installation_2(999)),
                ),
            )),
            // GitHub reports the organization under another ID.
            "org_mismatch" => Some(github.rule(
                mode,
                Fault::on(
                    "GET",
                    "/orgs/acme",
                    FaultAction::respond_json(
                        200,
                        json!({"id":999,"login":"acme","created_at":"2020-01-01T00:00:00Z"}),
                    ),
                ),
            )),
            _ => Some(github.rule(
                mode,
                Fault::on(
                    "GET",
                    "/orgs/acme",
                    FaultAction::respond_json(503, json!({"message":"Service Unavailable"})),
                ),
            )),
        };
        invoke(
            &h,
            "AccountInstallation",
            "org:100",
            "refresh",
            Some(&json!(2)),
        )
        .await;
        let status = invoke(&h, "AccountInstallation", "org:100", "status", None).await;
        assert_eq!(status["observation"]["kind"], expected, "{mode}");
        assert_eq!(
            invoke(&h, "AccountInstallation", "repo:100", "status", None).await["observation"],
            repo["observation"]
        );
        match rule {
            Some(rule) => assert!(emulator.remove_fault(rule)),
            None => assert!(emulator.unsuspend_installation(2)),
        }
    }
    invoke(
        &h,
        "AccountInstallation",
        "org:100",
        "refresh",
        Some(&json!(2)),
    )
    .await;
    assert_eq!(
        invoke(&h, "AccountInstallation", "org:100", "status", None).await["observation"]["kind"],
        "available"
    );
    // The organization App is uninstalled on GitHub: its installation is
    // missing (404).
    assert!(emulator.uninstall(2));
    invoke(
        &h,
        "AccountInstallation",
        "org:100",
        "refresh",
        Some(&json!(2)),
    )
    .await;
    assert_eq!(
        invoke(&h, "AccountInstallation", "org:100", "status", None).await["observation"]["kind"],
        "unavailable",
        "missing"
    );
    assert_eq!(
        invoke(&h, "AccountInstallation", "repo:100", "status", None).await["observation"],
        repo["observation"]
    );
    invoke(
        &h,
        "AccountInstallation",
        "org:100",
        "uninstall",
        Some(&json!({"app":"org","installation_id":2,"uninstalled_at":chrono::Utc::now()})),
    )
    .await;
    let before = emulator.calls().len();
    invoke(&h, "AccountInstallation", "org:100", "recheck", None).await;
    assert_eq!(emulator.calls().len(), before);
    assert_eq!(
        invoke(&h, "AccountInstallation", "org:100", "status", None).await["observation"]["kind"],
        "unavailable"
    );
    // Every request was one of these, with its App's credential: the
    // installation reads with the App JWT, the organization read with the
    // organization App's token, the repository listing with the repository
    // App's, and each token minted with its read profile.
    for call in emulator.calls() {
        let path = call.path.split('?').next().unwrap_or_default().to_owned();
        match (call.method.as_str(), path.as_str()) {
            ("GET", "/app/installations/1" | "/app/installations/2") => {
                assert_eq!(call.credential, CredentialKind::Jwt, "{path}");
            }
            ("POST", "/app/installations/1/access_tokens") => assert_eq!(
                call.body_fields.as_ref().and_then(|f| f.get("permissions")),
                Some(&json!({"administration":"read","metadata":"read"}))
            ),
            ("POST", "/app/installations/2/access_tokens") => assert_eq!(
                call.body_fields.as_ref().and_then(|f| f.get("permissions")),
                Some(&json!({"members":"read","metadata":"read"}))
            ),
            ("GET", "/orgs/acme") => assert_eq!(installation_of(&call), Some(2)),
            ("GET", "/installation/repositories") => assert_eq!(installation_of(&call), Some(1)),
            _ => panic!("unexpected {} {path}", call.method),
        }
    }
    h.finish().await;
}
