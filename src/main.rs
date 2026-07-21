use std::io::{self, Write};
use std::net::SocketAddr;
use std::path::PathBuf;

use clap::{Parser, Subcommand};
use plugboard::auth::hash_password;
use plugboard::config::{AuthMode, Config};
use plugboard::poller::spawn_poller;
use plugboard::routes;
use plugboard::state::AppState;
use plugboard::updates::spawn_update_checker;

#[derive(Parser)]
#[command(
    name = "plugboard",
    version,
    about = "Web dashboard for Tasmota and Shelly devices."
)]
struct Args {
    /// Path to the config TOML.
    #[arg(long, default_value = "plugboard.toml")]
    config: PathBuf,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Hash a password for `auth.password_hash` in the config file. Reads the
    /// password from the argument if given, otherwise prompts on stdin (the
    /// terminal echoes the input - pipe it in instead if that is a concern).
    HashPassword {
        /// Password to hash. Omit to read it from stdin.
        password: Option<String>,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    if let Some(Command::HashPassword { password }) = args.command {
        let password = match password {
            Some(p) => p,
            None => read_password_from_stdin()?,
        };
        println!("{}", hash_password(&password));
        return Ok(());
    }

    tracing_subscriber::fmt()
        .with_env_filter("plugboard=info")
        .init();
    let cfg = Config::load(&args.config)?;
    let bind = cfg.bind;
    let secure = cfg.auth.cookie_secure;
    if cfg.auth.mode == AuthMode::Builtin && cfg.auth.configured_credentials().is_none() {
        tracing::warn!(
            "auth.mode is \"builtin\" but auth.username / auth.password_hash are not both set - \
             every login attempt will be rejected until they are configured"
        );
    }
    let state = AppState::new(cfg, args.config);
    spawn_poller(state.clone());
    spawn_update_checker(state.clone());
    let app = routes::router(state, secure);
    let listener = tokio::net::TcpListener::bind(bind).await?;
    tracing::info!("listening on {}", bind);
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await?;
    Ok(())
}

/// Reads a single line from stdin as the password to hash. Not hidden
/// (no added dependency for that); pipe the password in if terminal echo is
/// a concern, e.g. `printf '%s' "$PW" | plugboard hash-password`.
fn read_password_from_stdin() -> anyhow::Result<String> {
    print!("Password: ");
    io::stdout().flush()?;
    let mut line = String::new();
    io::stdin().read_line(&mut line)?;
    Ok(line.trim_end_matches(['\n', '\r']).to_string())
}

/// Resolves once SIGINT (Ctrl+C) or, on unix, SIGTERM is received, so
/// `axum::serve`'s graceful shutdown lets in-flight requests finish instead
/// of dropping connections mid-response.
async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
    tracing::info!("shutdown signal received, draining in-flight requests");
}
