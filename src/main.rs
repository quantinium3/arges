use anyhow::{Context, Result};
use std::{future, sync::Arc};
use tokio::{net::TcpListener, signal::unix, sync::Notify};
use tracing::{error, info};
use tracing_subscriber::EnvFilter;

use crate::{config::Config, state::AppState};

mod config;
mod router;
mod state;

#[tokio::main]
async fn main() -> Result<()> {
    let env_filter = EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new("arges=info,tower_http=debug"))
        .context("failed to configure tracing subscriber")?;

    tracing_subscriber::fmt()
        .json()
        .with_env_filter(env_filter)
        .init();

    let config = Arc::new(Config::new()?);
    let addr = config.socket_addr();
    let state = AppState::new(config);

    info!("Starting server on {}", addr);

    let listener = TcpListener::bind(addr)
        .await
        .with_context(|| format!("failed to bind tcp listener on {addr}"))?;

    axum::serve(listener, router::routes(state))
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("server failed while handling requests")?;

    Ok(())
}

async fn shutdown_signal() {
    let terminate = async {
        match unix::signal(unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(e) => {
                error!(%e, "failed to install SIGTERM handler");
                future::pending::<()>().await;
            }
        }
    };

    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    tokio::select! {
        _ = ctrl_c => {
            info!("received Ctrl+C shutdown signal");
        }
        _ = terminate => {
            info!("received SIGTERM shutdown signal");
        }
    }
}
