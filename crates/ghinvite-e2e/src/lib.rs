//! Native test support for P1: a real Restate server and an emulated GitHub
//! HTTP API (octoemu with its temporary stub-compatibility world).
//!
//! No production crate depends on this library. CI must supply a server binary
//! or both reuse URLs; a missing server always fails these application tests.

use std::time::Duration;

pub mod journal;

pub use restate_e2e_harness::{Call, Reply, Restate, RunPath, Table};
use restate_e2e_harness::{ReusePolicy, ServerSpec, launcher_or_skip};

const SERVER: ServerSpec = ServerSpec {
    name: "p1-authoritative-admission",
    features: &[],
    env: &["RESTATE_WORKER__CLEANUP_INTERVAL=1s"],
};

/// The installed app fixtures as `octoauth::App`s for `GithubApps`: `repo`
/// (App 123), and with `org` also App 456, built through
/// `ghinvite_config::apps::parse_installed_github_apps` with the static RSA
/// test key (public test material; production configuration never depends
/// on this crate). They create tokens at `api_base`: the emulator's
/// ([`github_emulator`], [`Harness::start`]) or a scenario's own fake.
pub fn installed_apps(api_base: &str, org: bool) -> Vec<ghinvite_github::InstalledApp> {
    use base64::Engine as _;
    use ghinvite_config::apps::{AppStage, parse_installed_github_apps};
    let keys = format!(
        "primary:{}",
        base64::engine::general_purpose::STANDARD
            .encode(include_str!("../../../test-fixtures/github-app-rsa.pem"))
    );
    let stage = if org {
        AppStage::RepoAndOrg
    } else {
        AppStage::Repo
    };
    parse_installed_github_apps(stage, api_base, |field| match field {
        "GH_REPO_APP_ID" => Some("123".into()),
        "GH_REPO_CLIENT_ID" => Some("Iv-p1-runtime-fixture".into()),
        "GH_ORG_APP_ID" => Some("456".into()),
        "GH_ORG_CLIENT_ID" => Some("Iv-p1-runtime-fixture-org".into()),
        "GH_REPO_PRIVATE_KEYS" | "GH_ORG_PRIVATE_KEYS" => Some(keys.clone()),
        _ => None,
    })
    .expect("installed app fixtures")
}

/// [`installed_apps`] for the `repo` app only.
pub fn repository_apps(api_base: &str) -> Vec<ghinvite_github::InstalledApp> {
    installed_apps(api_base, false)
}

/// `ghinvite_github`'s reqwest transport over `client`, for `GithubApps`.
pub fn github_transport(
    client: reqwest::Client,
) -> std::sync::Arc<dyn ghinvite_github::HttpTransportDyn> {
    std::sync::Arc::new(ghinvite_github::transport::ReqwestTransport::with_client(
        client,
    ))
}

/// An embedded GitHub emulator with the stub-compatibility world
/// (`octoemu::compat::seed`): `acme` (100) with `acme/api` (10) and
/// `acme/web` (11), `creator` (7, an owner), `alice` (8), the repository App
/// (123) installed as installation 1 and the organization App (456) as 2,
/// both verifying [`installed_apps`]' key. Its `api_base` is what
/// [`installed_apps`] takes; the stub's control routes are under
/// [`compat_url`] (temporary, GE-09).
pub async fn github_emulator() -> octoemu::Emulator {
    github_emulator_with(octoemu::compat::seed()).await
}

/// [`github_emulator`] with the repository App installed as `installation_id`.
pub async fn github_emulator_for(installation_id: u64) -> octoemu::Emulator {
    github_emulator_with(octoemu::compat::seed_with_installation(installation_id)).await
}

/// An embedded GitHub emulator with `seed`.
pub async fn github_emulator_with(seed: octoemu::seed::Seed) -> octoemu::Emulator {
    octoemu::Emulator::start(seed)
        .await
        .expect("GitHub emulator starts")
}

/// The emulator's stub-compatibility route `route` (`identity`, `outcomes`,
/// `installation-identity`, `installation-repositories`,
/// `invitation-expired`, `calls`, `reset`) for an emulator at `base`.
pub fn compat_url(base: &str, route: &str) -> String {
    format!("{base}/__octoemu/compat/{route}")
}

/// Owns both the GitHub fixture and the actual Restate process/endpoint lifecycle.
pub struct Harness {
    pub restate: Restate,
    pub client: reqwest::Client,
    pub github_url: String,
    github: Github,
}

/// What serves GitHub for a [`Harness`].
enum Github {
    Emulator(octoemu::Emulator),
    /// Stops the router's server when dropped.
    Router {
        _server: AbortOnDrop,
    },
}

struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

impl Harness {
    /// A shared-or-spawned Restate server and [`github_emulator`].
    pub async fn start() -> Self {
        Self::start_with_emulator(github_emulator().await).await
    }

    /// [`Harness::start`] with a prepared emulator (another seed, an
    /// installation ID, fault rules).
    pub async fn start_with_emulator(emulator: octoemu::Emulator) -> Self {
        let launcher = launcher_or_skip(ReusePolicy::Allowed)
            .expect("P1 workflow verification requires RESTATE_SERVER_BIN or both Restate URLs");
        let url = emulator.api_base().to_owned();
        Self::launch(launcher, Github::Emulator(emulator), url).await
    }

    /// Use a scenario-specific HTTP fixture while retaining the real Restate
    /// lifecycle and the fail-closed requirement for a server binary.
    pub async fn start_with_github_router(router: axum::Router) -> Self {
        let launcher = launcher_or_skip(ReusePolicy::Allowed)
            .expect("P1 workflow verification requires RESTATE_SERVER_BIN or both Restate URLs");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let server = AbortOnDrop(tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        }));
        Self::launch(launcher, Github::Router { _server: server }, url).await
    }

    /// [`Harness::start`] on a server this test spawns and owns, ignoring
    /// `RESTATE_ADMIN_URL` / `RESTATE_INGRESS_URL`.
    ///
    /// For suites whose tests each deploy their own endpoint (own fault
    /// handles, own in-memory storage) under the same service names. A shared
    /// server routes every new invocation of a service to the latest
    /// registered deployment, so concurrent tests drive each other's
    /// endpoints, and retained object state (e.g. an already onboarded
    /// account key) suppresses the effects a later test's fresh storage needs.
    pub async fn start_owned() -> Self {
        let launcher = launcher_or_skip(ReusePolicy::Never).expect(
            "this suite needs a Restate server per test: set RESTATE_SERVER_BIN \
             (RESTATE_ADMIN_URL / RESTATE_INGRESS_URL are ignored)",
        );
        let emulator = github_emulator().await;
        let url = emulator.api_base().to_owned();
        Self::launch(launcher, Github::Emulator(emulator), url).await
    }

    async fn launch(
        launcher: restate_e2e_harness::Launcher,
        github: Github,
        github_url: String,
    ) -> Self {
        let restate = launcher.launch(&SERVER).await;
        let client = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(25))
            .build()
            .unwrap();
        Self {
            restate,
            client,
            github_url,
            github,
        }
    }

    /// The emulator serving GitHub; panics for a scenario's own router.
    pub fn emulator(&self) -> &octoemu::Emulator {
        match &self.github {
            Github::Emulator(emulator) => emulator,
            Github::Router { .. } => panic!("this harness serves a scenario's own GitHub router"),
        }
    }

    /// [`compat_url`] for this harness's emulator.
    pub fn compat(&self, route: &str) -> String {
        compat_url(&self.github_url, route)
    }

    /// Finish endpoints before dropping the GitHub server and detect task panics.
    pub async fn finish(self) {
        self.restate.finish().await;
        drop(self.github);
    }
}

/// Explicit setup fixture builder. The old actor field is deliberately discarded:
/// a setup callback can prove installation facts, never its installing owner.
pub fn setup_command(mut account: serde_json::Value) -> serde_json::Value {
    let account = account.as_object_mut().expect("fixture account");
    account.remove("actor_user_id");
    account.remove("app");
    account.insert("uninstalled_at".into(), serde_json::Value::Null);
    serde_json::json!({"operation_id":ulid::Ulid::new().to_string(),"verified":{
        "app":"repo","account":account,"suspended_at":null,
        "permissions":{"administration":"write","metadata":"read"}
    }})
}

/// Authenticated-webhook boundary fixture, separate from installer-free setup.
/// The fixed payload digest is suitable for one initial owner event per account.
pub fn signed_owner_command(setup: &serde_json::Value, sender: u64) -> serde_json::Value {
    serde_json::json!({"delivery_id":"a".repeat(64),"action":"created",
        "verified":setup["verified"],"sender_id":sender,"sender_login":format!("user-{sender}"),
        "updated_at":"2026-01-01T01:00:00Z"})
}
