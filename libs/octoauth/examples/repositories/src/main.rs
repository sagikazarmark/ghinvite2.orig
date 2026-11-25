//! `repositories`: serve the example web app.
//!
//! ```sh
//! GITHUB_APP_ID=123 GITHUB_APP_KEY=app.pem \
//! GITHUB_CLIENT_ID=Iv23… GITHUB_CLIENT_SECRET=… \
//!     cargo run -p example-repositories
//! ```
//!
//! `PUBLIC_URL` is where browsers reach it (default `http://localhost:3000`;
//! the App's callback URL is `<PUBLIC_URL>/callback`, its setup URL
//! `<PUBLIC_URL>/setup`). `LISTEN` is the socket address (default
//! `127.0.0.1:3000`). `GITHUB_API_URL` and `GITHUB_WEB_URL` point it at
//! another GitHub (e.g. a running octoemu).

#[cfg(target_arch = "wasm32")]
fn main() {}

#[cfg(not(target_arch = "wasm32"))]
#[tokio::main]
async fn main() -> std::process::ExitCode {
    match run().await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
async fn run() -> Result<(), Box<dyn std::error::Error>> {
    use example_repositories::WebApp;
    use octotoken::{AppTokenProvider, octoauth};

    let var = |name: &str| std::env::var(name).map_err(|_| format!("set {name}"));
    let optional = |name: &str| std::env::var(name).ok();

    let app_id: u64 = var("GITHUB_APP_ID")?
        .parse()
        .map_err(|_| "GITHUB_APP_ID is not a number")?;
    let key_path = var("GITHUB_APP_KEY")?;
    let pem = std::fs::read_to_string(&key_path)
        .map_err(|error| format!("cannot read GITHUB_APP_KEY ({key_path}): {error}"))?;
    let public_url = optional("PUBLIC_URL").unwrap_or_else(|| "http://localhost:3000".into());
    let public_url = public_url.trim_end_matches('/');
    let listen = optional("LISTEN").unwrap_or_else(|| "127.0.0.1:3000".into());

    let mut app = octoauth::App::builder(app_id, octoauth::SigningKey::from_pem(&pem)?);
    let mut sign_in = octologin::SignIn::builder(
        var("GITHUB_CLIENT_ID")?,
        var("GITHUB_CLIENT_SECRET")?,
        format!("{public_url}/callback"),
    );
    if let Some(api_base) = optional("GITHUB_API_URL") {
        app = app.base_url(&api_base);
        sign_in = sign_in.api_base_url(api_base);
    }
    if let Some(web_base) = optional("GITHUB_WEB_URL") {
        sign_in = sign_in.web_base_url(web_base);
    }

    let web = WebApp::new(AppTokenProvider::new(app.build()?), sign_in.build()?)
        .await?
        .secure_cookies(public_url.starts_with("https://"));
    let listener = tokio::net::TcpListener::bind(&listen).await?;
    println!("listening on {public_url} ({listen})");
    axum::serve(listener, web.router()).await?;
    Ok(())
}
