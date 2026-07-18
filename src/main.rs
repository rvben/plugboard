use std::path::PathBuf;

use axum::{Router, routing::get};
use clap::Parser;
use tasmota_web::config::Config;

#[derive(Parser)]
#[command(
    name = "tasmota-web",
    version,
    about = "Web dashboard for Tasmota devices."
)]
struct Args {
    /// Path to the config TOML.
    #[arg(long, default_value = "tasmota-web.toml")]
    config: PathBuf,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter("tasmota_web=info")
        .init();
    let args = Args::parse();
    let cfg = Config::load(&args.config)?;
    let app = Router::new().route("/", get(|| async { "tasmota-web" }));
    let listener = tokio::net::TcpListener::bind(cfg.bind).await?;
    tracing::info!("listening on {}", cfg.bind);
    axum::serve(listener, app).await?;
    Ok(())
}
