//! Real retained installation authority for protocol tests with separate projectors.
use serde_json::json;
use std::sync::Arc;

pub struct InstallationContext {
    state: ghinvite_workflows::AppState,
    server: tokio::task::JoinHandle<()>,
}
impl Drop for InstallationContext {
    fn drop(&mut self) {
        self.server.abort();
    }
}
impl InstallationContext {
    pub async fn start(
        storage: Arc<ghinvite_storage_sqlx::SqlxStorage>,
        client: reqwest::Client,
    ) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener, ghinvite_github::stub::router())
                .await
                .unwrap();
        });
        let github_transport = ghinvite_e2e::github_transport(client);
        let state = ghinvite_workflows::AppState::new(storage).with_github_apps(Arc::new(
            ghinvite_workflows::RepositoryGithub::new(
                github_transport,
                &base,
                ghinvite_e2e::repository_apps(&base),
            )
            .unwrap(),
        ));
        Self { state, server }
    }
    pub fn bind(&self, builder: restate_sdk::endpoint::Builder) -> restate_sdk::endpoint::Builder {
        ghinvite_workflows::delivery::bind(builder, self.state.clone())
            .bind(ghinvite_workflows::availability::AccountInstallation {
                state: self.state.clone(),
            })
            .bind(ghinvite_workflows::availability::InstallationProjection {
                state: self.state.clone(),
            })
    }
    pub async fn seed(&self, client: &reqwest::Client, ingress: &str) {
        let setup = ghinvite_e2e::setup_command(
            json!({"installation_id":1,"account_id":100,"account_login":"acme","account_type":"Organization","selected_repos":"all","installed_at":"2026-01-01T00:00:00Z"}),
        );
        for (handler, input) in [
            ("onboard", setup.clone()),
            (
                "onboard_webhook",
                ghinvite_e2e::signed_owner_command(&setup, 7),
            ),
        ] {
            let response = client
                .post(format!("{ingress}/AccountInstallation/repo:100/{handler}"))
                .json(&input)
                .send()
                .await
                .unwrap();
            assert!(
                response.status().is_success(),
                "{handler}: {}",
                response.text().await.unwrap()
            );
        }
    }
}
