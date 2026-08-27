use std::{
    collections::HashSet,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use sqlx::SqlitePool;
use tokio::time::{Instant, MissedTickBehavior, interval_at, timeout};
use tracing::{debug, warn};

use crate::{
    constants::{
        DEPLOYMENT_LABEL, METRICS_RETENTION, METRICS_SAMPLE_INTERVAL, METRICS_STATS_TIMEOUT,
    },
    db::queries::metrics,
    infra::{
        containers::docker::DockerClient,
        metrics::collector::{Collector, ContainerStats},
    },
};

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since| since.as_secs() as i64)
        .unwrap_or_default()
}

async fn container_stats(
    docker: &DockerClient,
    known: &HashSet<String>,
) -> Result<Vec<ContainerStats>> {
    let containers = docker
        .running_by_label(DEPLOYMENT_LABEL)
        .await
        .context("failed to list the running deployment containers")?;

    let wanted: Vec<_> = containers
        .into_iter()
        .filter(|container| known.contains(&container.value))
        .collect();

    if wanted.is_empty() {
        return Ok(Vec::new());
    }

    let reads = wanted.iter().map(|container| async move {
        (
            container.value.clone(),
            docker.usage(&container.name).await,
            container.name.clone(),
        )
    });

    let results = timeout(METRICS_STATS_TIMEOUT, futures_util::future::join_all(reads))
        .await
        .context("timed out reading container stats from docker")?;

    let mut stats = Vec::new();

    for (deployment_id, usage, name) in results {
        match usage {
            Ok(Some(usage)) => stats.push(ContainerStats {
                deployment_id,
                cpu_percent: usage.cpu_percent,
                memory_used: usage.memory_used as i64,
                memory_limit: usage.memory_limit.map(|limit| limit as i64),
                network_rx_total: usage.network_rx_total,
                network_tx_total: usage.network_tx_total,
            }),
            Ok(None) => {}
            Err(e) => warn!(container = %name, error = ?e, "could not read container stats"),
        }
    }

    Ok(stats)
}

pub async fn run(
    pool: &SqlitePool,
    collector: &mut Collector,
    docker: Option<&DockerClient>,
) -> Result<()> {
    let ts = now();
    let host = collector.host(ts);
    let window_seconds = host.window_seconds;

    metrics::record_host(pool, &host).await?;
    metrics::record_filesystems(pool, &collector.filesystems(ts)).await?;

    if let Some(docker) = docker {
        let known = metrics::known_deployment_ids(pool).await?;

        match container_stats(docker, &known).await {
            Ok(stats) => {
                let samples = collector.deployments(ts, window_seconds, stats);
                metrics::record_deployments(pool, &samples).await?;
            }
            Err(e) => warn!(error = ?e, "could not sample deployment containers"),
        }
    }

    let removed = metrics::prune(pool, ts - METRICS_RETENTION.as_secs() as i64).await?;
    if removed > 0 {
        debug!(removed, "pruned expired metric samples");
    }

    Ok(())
}

pub fn init(pool: SqlitePool, docker: Option<DockerClient>) {
    tokio::spawn(async move {
        let mut collector = Collector::new();
        let mut ticker = interval_at(
            Instant::now() + METRICS_SAMPLE_INTERVAL,
            METRICS_SAMPLE_INTERVAL,
        );
        ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);

        loop {
            ticker.tick().await;

            if let Err(e) = run(&pool, &mut collector, docker.as_ref()).await {
                warn!(error = ?e, "metrics sampling failed");
            }
        }
    });
}
