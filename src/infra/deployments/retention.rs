use std::{collections::HashSet, sync::Arc};

use anyhow::{Context, Result};
use sqlx::SqlitePool;
use tracing::{info, warn};

use crate::{
    constants::{REGISTRY_CONTAINER_NAME, RETENTION_INTERVAL},
    db::queries::{audit, deployments},
    infra::containers::{docker::DockerClient, registry::RegistryClient},
};

const SUBJECT: &str = "deployment";

pub struct Pruned {
    pub releases: usize,
    pub collected: bool,
}

async fn prune_deployment(
    pool: &SqlitePool,
    registry: &RegistryClient,
    deployment_id: &str,
    name: &str,
) -> Result<usize> {
    let stale = deployments::prunable_releases(pool, deployment_id).await?;

    let stale_ids: HashSet<&str> = stale.iter().map(|r| r.id.as_str()).collect();
    let kept_digests: HashSet<String> = deployments::releases(pool, deployment_id)
        .await?
        .iter()
        .filter(|r| !stale_ids.contains(r.id.as_str()))
        .filter_map(|r| r.digest.clone())
        .collect();

    let mut removed = 0;

    for release in stale {
        let digest = match &release.digest {
            Some(digest) => digest.clone(),
            None => match registry.manifest_digest(name, &release.tag).await? {
                Some(digest) => digest,
                None => {
                    deployments::delete_release(pool, &release.id).await?;
                    removed += 1;
                    continue;
                }
            },
        };

        if kept_digests.contains(&digest) {
            deployments::delete_release(pool, &release.id).await?;
            removed += 1;
            info!(
                deployment = %name,
                tag = %release.tag,
                "dropped the release record but kept the image, a retained release shares its digest"
            );
            continue;
        }

        registry
            .delete_manifest(name, &digest)
            .await
            .with_context(|| format!("failed to drop {name}:{}", release.tag))?;

        deployments::delete_release(pool, &release.id).await?;
        removed += 1;

        info!(deployment = %name, tag = %release.tag, "pruned release");
        audit::record(
            pool,
            SUBJECT,
            Some(deployment_id),
            "release_pruned",
            Some(&release.tag),
        )
        .await?;
    }

    Ok(removed)
}

pub async fn collect_garbage(docker: &DockerClient) -> Result<()> {
    docker
        .exec(
            REGISTRY_CONTAINER_NAME,
            &[
                "registry",
                "garbage-collect",
                "--delete-untagged",
                "/etc/docker/registry/config.yml",
            ],
        )
        .await
        .context("failed to run the registry garbage collector")?;

    Ok(())
}

pub async fn run(
    pool: &SqlitePool,
    registry: &RegistryClient,
    docker: &DockerClient,
) -> Result<Pruned> {
    let mut releases = 0;

    for deployment in deployments::list(pool).await? {
        match prune_deployment(pool, registry, &deployment.id, &deployment.name).await {
            Ok(count) => releases += count,
            Err(e) => warn!(deployment = %deployment.name, error = ?e, "retention failed"),
        }
    }

    let collected = if releases > 0 {
        if let Err(e) = collect_garbage(docker).await {
            warn!(error = ?e, "registry garbage collection failed");
            false
        } else {
            info!(releases, "pruned releases and collected registry garbage");
            true
        }
    } else {
        false
    };

    Ok(Pruned {
        releases,
        collected,
    })
}

pub fn init(
    pool: SqlitePool,
    registry: RegistryClient,
    docker: DockerClient,
    notify: Arc<tokio::sync::Notify>,
) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(RETENTION_INTERVAL);

        loop {
            tokio::select! {
                _ = notify.notified() => {}
                _ = ticker.tick() => {}
            }

            if let Err(e) = run(&pool, &registry, &docker).await {
                warn!(error = ?e, "retention run failed");
            }
        }
    });
}
