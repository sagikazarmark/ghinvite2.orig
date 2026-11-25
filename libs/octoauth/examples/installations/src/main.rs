//! `installations`: print every account the App is installed on and the
//! repositories each installation can reach.
//!
//! ```sh
//! GITHUB_APP_ID=123 GITHUB_APP_KEY=app.pem cargo run -p example-installations
//! ```
//!
//! `GITHUB_API_URL` points it at another API base (e.g. a running octoemu).

#[cfg(target_arch = "wasm32")]
fn main() {}

#[cfg(not(target_arch = "wasm32"))]
use example_installations::Lister;
#[cfg(not(target_arch = "wasm32"))]
use octotoken::{AppTokenProvider, octoauth};
#[cfg(not(target_arch = "wasm32"))]
use std::process::ExitCode;

#[cfg(not(target_arch = "wasm32"))]
#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let var = |name: &str| std::env::var(name).map_err(|_| format!("set {name}"));
    let app_id: u64 = var("GITHUB_APP_ID")?
        .parse()
        .map_err(|_| "GITHUB_APP_ID is not a number")?;
    let key_path = var("GITHUB_APP_KEY")?;
    let pem = std::fs::read_to_string(&key_path)
        .map_err(|error| format!("cannot read GITHUB_APP_KEY ({key_path}): {error}"))?;
    let mut app = octoauth::App::builder(app_id, octoauth::SigningKey::from_pem(&pem)?);
    if let Ok(api_base) = std::env::var("GITHUB_API_URL") {
        app = app.base_url(api_base);
    }
    // One provider per App for the life of the process; the octocrab client
    // follows the App's API base.
    let lister = Lister::new(AppTokenProvider::new(app.build()?))?;

    let installations = lister.installations().await?;
    if installations.is_empty() {
        println!("The App has no installations.");
    }
    for installation in installations {
        let selection = installation
            .selection
            .as_deref()
            .map(|selection| format!(", {selection} repositories"))
            .unwrap_or_default();
        println!(
            "{} ({}, installation {}{selection})",
            installation.account, installation.kind, installation.id
        );
        // One installation failing (suspended, say) does not stop the rest.
        match lister.repositories(installation.id).await {
            Ok(repositories) if repositories.is_empty() => println!("  (no repositories)"),
            Ok(repositories) => {
                for repository in repositories {
                    let private = if repository.private { " (private)" } else { "" };
                    println!("  {}{private}", repository.full_name);
                }
            }
            Err(error) => println!("  cannot list repositories: {error}"),
        }
    }
    Ok(())
}
