use anyhow::{Context, Result, bail};
use sqlx::SqlitePool;
use std::{
    fs::{self, DirBuilder, File, OpenOptions},
    future,
    io::ErrorKind,
    os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::{net::UnixListener, signal::unix, sync::Notify};
use tracing::{error, info, warn};

use crate::{
    config::Config,
    infra::{
        containers::{
            bootstrap,
            docker::DockerClient,
            registry::RegistryClient,
            services::{self, ServiceId},
        },
        deployments::{reconciler as deployment_reconciler, retention},
        metrics::sampler as metrics_sampler,
        packages::{catalog, package_manager::PackageManager, reconciler},
        parameters::secrets::MasterKey,
        proxy::{admin::CaddyAdmin, reconciler as proxy_reconciler},
    },
    state::AppState,
};

mod config;
mod constants;
mod db;
mod handler;
mod infra;
mod logging;
mod router;
mod state;
mod utils;

const SOCKET_MODE: u32 = 0o660;
const SOCKET_DIR_MODE: u32 = 0o750;
const LOCK_MODE: u32 = 0o640;
type SocketIdentity = (u64, u64);

#[tokio::main]
async fn main() -> Result<()> {
    let agent_log = logging::init()?;

    let config = Config::new()?;
    let socket_path = config.socket_path;

    prepare_socket_dir(&socket_path)?;

    let _lock = acquire_lock(&socket_path)?;

    let listener = bind_socket(&socket_path)?;
    let socket_identity = socket_identity(&socket_path)?;

    let master_key = Arc::new(
        MasterKey::load(&config.master_key_path)
            .await
            .context("failed to load the master key")?,
    );

    let db_url = format!("sqlite://{}", config.db_path.display());
    let pool = db::pool::connect(&db_url).await?;
    db::migration::migrate(&pool).await?;

    let package_manager = PackageManager::detect()
        .await
        .context("failed to detect host package manager")?;

    catalog::seed(&pool, &package_manager)
        .await
        .context("failed to seed package catalog")?;

    let docker = start_containers(&pool).await;

    let caddy = CaddyAdmin::new(constants::CADDY_ADMIN_URL);
    let registry = RegistryClient::new(constants::REGISTRY_URL);
    apply_proxy_config(&pool, &master_key, &caddy).await;

    let deploy_notify = Arc::new(Notify::new());
    let retention_notify = Arc::new(Notify::new());
    if let Some(docker) = &docker {
        retention::init(
            pool.clone(),
            registry.clone(),
            docker.clone(),
            retention_notify.clone(),
        );
        deployment_reconciler::init(
            pool.clone(),
            master_key.clone(),
            docker.clone(),
            caddy.clone(),
            deploy_notify.clone(),
        );
    }

    metrics_sampler::init(pool.clone(), docker.clone());

    let reconcile_notify = Arc::new(Notify::new());
    reconciler::init(&pool, reconcile_notify.clone(), package_manager).await?;

    let state = AppState {
        db: pool,
        package_manager,
        reconcile_notify,
        deploy_notify,
        retention_notify,
        agent_log,
        journal: Arc::new(logging::journal::JournalReader::new(config.journal_unit)),
        master_key,
        docker,
        caddy,
        registry,
    };

    info!(socket = %socket_path.display(), "arges listening");

    let res = axum::serve(listener, router::routes(state))
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("server failed while handling requests");

    remove_socket(&socket_path, socket_identity);

    res
}

async fn start_containers(pool: &SqlitePool) -> Option<DockerClient> {
    let docker = match DockerClient::connect().await {
        Ok(docker) => docker,
        Err(e) => {
            warn!(error = ?e, "docker is unavailable, container features are disabled");
            return None;
        }
    };

    if let Err(e) = bootstrap::run(pool, &docker).await {
        warn!(error = ?e, "container bootstrap failed, some services may not be running");
    }

    Some(docker)
}

async fn apply_proxy_config(pool: &SqlitePool, master_key: &MasterKey, caddy: &CaddyAdmin) {
    let enabled = services::is_enabled(pool, ServiceId::ReverseProxy)
        .await
        .unwrap_or(false);

    if !enabled {
        return;
    }

    if let Err(e) =
        proxy_reconciler::apply_when_ready(pool, master_key, caddy, constants::CADDY_READY_TIMEOUT)
            .await
    {
        warn!(error = ?e, "could not apply the proxy config at startup");
    }
}

fn lock_path(socket_path: &Path) -> PathBuf {
    let mut name = socket_path.as_os_str().to_os_string();
    name.push(".lock");
    PathBuf::from(name)
}

fn acquire_lock(socket_path: &Path) -> Result<File> {
    let path = lock_path(socket_path);
    let file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .mode(LOCK_MODE)
        .open(&path)
        .with_context(|| format!("failed to open the lock file at {}", path.display()))?;

    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(fs::TryLockError::WouldBlock) => bail!(
            "another arges instance holds the lock at {}",
            path.display()
        ),
        Err(fs::TryLockError::Error(e)) => {
            Err(e).with_context(|| format!("failed to lock {}", path.display()))
        }
    }
}

fn bind_socket(path: &Path) -> Result<UnixListener> {
    clear_socket_path(path)?;

    let listener = UnixListener::bind(path)
        .with_context(|| format!("failed to bind unix listener on {}", path.display()))?;

    fs::set_permissions(path, fs::Permissions::from_mode(SOCKET_MODE))
        .with_context(|| format!("failed to set permissions on {}", path.display()))?;

    Ok(listener)
}

fn prepare_socket_dir(path: &Path) -> Result<()> {
    let Some(parent) = path.parent() else {
        return Ok(());
    };

    DirBuilder::new()
        .recursive(true)
        .mode(SOCKET_DIR_MODE)
        .create(parent)
        .with_context(|| format!("failed to create socket directory {}", parent.display()))?;

    let mode = fs::metadata(parent)
        .with_context(|| format!("failed to stat socket directory {}", parent.display()))?
        .permissions()
        .mode();
    if mode & 0o007 != 0 {
        warn!(
            directory = %parent.display(),
            mode = format!("{:04o}", mode & 0o7777),
            "socket directory is accessible to all users"
        );
    }

    Ok(())
}

fn clear_socket_path(path: &Path) -> Result<()> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(()),
        Err(e) => {
            return Err(e).with_context(|| format!("failed to stat {}", path.display()));
        }
    };

    if !metadata.file_type().is_socket() {
        bail!(
            "{} exists and is not a socket, refusing to remove it",
            path.display()
        );
    }

    match std::os::unix::net::UnixStream::connect(path) {
        Ok(_) => bail!(
            "another arges instance is already listening on {}",
            path.display()
        ),
        Err(e) if e.kind() == ErrorKind::ConnectionRefused => {}
        Err(e) => {
            return Err(e).with_context(|| {
                format!(
                    "failed to probe the existing socket at {}, refusing to remove it",
                    path.display()
                )
            });
        }
    }

    info!(socket = %path.display(), "removing stale socket");
    fs::remove_file(path)
        .with_context(|| format!("failed to remove the stale socket at {}", path.display()))
}

fn socket_identity(path: &Path) -> Result<SocketIdentity> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("failed to stat the socket at {}", path.display()))?;
    Ok((metadata.dev(), metadata.ino()))
}

fn remove_socket(path: &Path, expected: SocketIdentity) {
    match socket_identity(path) {
        Ok(current) if current == expected => {
            if let Err(e) = fs::remove_file(path)
                && e.kind() != ErrorKind::NotFound
            {
                error!(%e, socket = %path.display(), "failed to remove the socket on shutdown");
            }
        }
        Ok(_) => warn!(
            socket = %path.display(),
            "socket was replaced while running, leaving it in place"
        ),
        Err(_) => {}
    }
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
