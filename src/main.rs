use std::path::PathBuf;

use clap::Parser;
use tasmota_web::config::Config;
use tasmota_web::poller::spawn_poller;
use tasmota_web::routes;
use tasmota_web::state::AppState;

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
    let bind = cfg.bind;
    let state = AppState::new(cfg, args.config);
    spawn_poller(state.clone());
    let app = routes::router(state);
    let listener = tokio::net::TcpListener::bind(bind).await?;
    tracing::info!("listening on {}", bind);
    axum::serve(listener, app).await?;
    Ok(())
}
