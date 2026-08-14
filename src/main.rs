use anyhow::{Context, Result, bail};
use std::{fs, future, io::ErrorKind, os::unix::fs::PermissionsExt, path::Path, sync::Arc};
use tokio::{net::UnixListener, signal::unix};
use tracing::{error, info};
use tracing_subscriber::EnvFilter;

use crate::{config::Config, state::AppState};

mod config;
mod router;
mod state;

const SOCKET_MODE: u32 = 0o660;

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
    let socket_path = config.socket_path.clone();
    let state = AppState::new(config);

    let listener = bind_socket(&socket_path)?;

    info!("Starting server on {}", socket_path.display());

    let result = axum::serve(listener, router::routes(state))
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("server failed while handling requests");

    if let Err(e) = fs::remove_file(&socket_path)
        && e.kind() != ErrorKind::NotFound
    {
        error!(%e, "failed to remove the socket on shutdown");
    }

    result
}

fn bind_socket(path: &Path) -> Result<UnixListener> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create socket directory {}", parent.display()))?;
    }

    if fs::symlink_metadata(path).is_ok() {
        if std::os::unix::net::UnixStream::connect(path).is_ok() {
            bail!(
                "another arges instance is already listening on {}",
                path.display()
            );
        }

        info!("removing the stale socket at {}", path.display());
        fs::remove_file(path)
            .with_context(|| format!("failed to remove the stale socket at {}", path.display()))?;
    }

    let listener = UnixListener::bind(path)
        .with_context(|| format!("failed to bind unix listener on {}", path.display()))?;

    fs::set_permissions(path, fs::Permissions::from_mode(SOCKET_MODE))
        .with_context(|| format!("failed to set permissions on {}", path.display()))?;

    Ok(listener)
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
