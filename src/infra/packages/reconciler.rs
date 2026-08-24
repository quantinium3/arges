use std::sync::Arc;

use anyhow::{Context, Result};
use sqlx::SqlitePool;
use tokio::sync::Notify;

use crate::{
    constants::PACKAGE_RESYNC_INTERVAL,
    db::queries::packages::{
        self, DesiredState, Package, PackageStatus, lookup_name_for_manager, transition,
        transition_failed,
    },
    infra::packages::package_manager::PackageManager,
};

pub async fn init(pool: &SqlitePool, notify: Arc<Notify>, pm: PackageManager) -> Result<()> {
    reconcile(pool, &pm)
        .await
        .context("failed initial package reconcile")?;

    let pool = pool.clone();
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(PACKAGE_RESYNC_INTERVAL);
        loop {
            tokio::select! {
                _ = notify.notified() => {}
                _ = ticker.tick() => {}
            }
            if let Err(e) = reconcile(&pool, &pm).await {
                tracing::error!(error = ?e, "package reconcile failed");
            }
        }
    });

    Ok(())
}

async fn reconcile(pool: &SqlitePool, pm: &PackageManager) -> Result<()> {
    let packages = packages::fetch_all(pool).await?;

    for pkg in packages {
        let Some(manager_name) = lookup_name_for_manager(pool, &pkg.id, pm.id()).await? else {
            continue;
        };

        let mut pkg = match sync(pool, pm, &pkg, &manager_name).await {
            Ok(pkg) => pkg,
            Err(e) => {
                tracing::error!(package = %pkg.id, error = ?e, "failed to sync package reality");
                continue;
            }
        };

        let result = match (pkg.desired_state, pkg.status) {
            (DesiredState::Installed, PackageStatus::Removed | PackageStatus::Failed) => {
                reconcile_one(
                    pool,
                    pm,
                    &mut pkg,
                    &manager_name,
                    PackageStatus::Installing,
                    true,
                )
                .await
            }
            (DesiredState::Removed, PackageStatus::Installed | PackageStatus::Failed) => {
                reconcile_one(
                    pool,
                    pm,
                    &mut pkg,
                    &manager_name,
                    PackageStatus::Removing,
                    false,
                )
                .await
            }
            _ => Ok(()),
        };

        if let Err(e) = result {
            tracing::error!(package = %pkg.id, error = ?e, "failed to reconcile package");
        }
    }
    Ok(())
}

async fn reconcile_one(
    pool: &SqlitePool,
    pm: &PackageManager,
    pkg: &mut Package,
    manager_name: &str,
    in_progress: PackageStatus,
    install: bool,
) -> Result<()> {
    transition(pool, pkg, in_progress, None).await?;
    pkg.status = in_progress;

    let outcome = if install {
        pm.install(manager_name).await
    } else {
        pm.remove(manager_name).await
    };

    match outcome {
        Ok(()) => {
            let done = if install {
                PackageStatus::Installed
            } else {
                PackageStatus::Removed
            };
            transition(pool, pkg, done, None).await
        }
        Err(e) => transition_failed(pool, pkg, &e).await,
    }
}

async fn sync(
    pool: &SqlitePool,
    pm: &PackageManager,
    pkg: &Package,
    manager_name: &str,
) -> Result<Package> {
    if !matches!(
        pkg.status,
        PackageStatus::Installed | PackageStatus::Removed
    ) {
        return Ok(pkg.clone());
    }

    let actual_status = if pm.is_installed(manager_name).await? {
        PackageStatus::Installed
    } else {
        PackageStatus::Removed
    };

    if actual_status == pkg.status {
        return Ok(pkg.clone());
    }

    transition(
        pool,
        pkg,
        actual_status,
        Some("reconciled with live system state"),
    )
    .await?;

    Ok(Package {
        status: actual_status,
        ..pkg.clone()
    })
}
