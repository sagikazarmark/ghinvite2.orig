use clap::Parser;

#[derive(Parser)]
struct Cli {
    #[arg(long, default_value_t = 3001)]
    port: u16,
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, cli.port))
        .await
        .unwrap();
    eprintln!(
        "github-stub listening on {}",
        listener.local_addr().unwrap()
    );
    axum::serve(listener, ghinvite_github::stub::router())
        .await
        .unwrap();
}
