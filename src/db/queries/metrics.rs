use std::collections::HashSet;

use anyhow::{Context, Result};
use serde::Serialize;
use sqlx::SqlitePool;

#[derive(Debug, Clone, Serialize)]
pub struct HostSample {
    pub ts: i64,
    pub window_seconds: i64,
    pub cpu_percent: f64,
    pub memory_used: i64,
    pub memory_total: i64,
    pub swap_used: i64,
    pub swap_total: i64,
    pub load_one: f64,
    pub load_five: f64,
    pub load_fifteen: f64,
    pub network_rx_bytes: i64,
    pub network_tx_bytes: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct FilesystemSample {
    pub ts: i64,
    pub mount_point: String,
    pub total_bytes: i64,
    pub available_bytes: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct DeploymentSample {
    pub ts: i64,
    pub deployment_id: String,
    pub window_seconds: i64,
    pub cpu_percent: f64,
    pub memory_used: i64,
    pub memory_limit: Option<i64>,
    pub network_rx_bytes: i64,
    pub network_tx_bytes: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct NamedDeploymentSample {
    pub name: String,
    #[serde(flatten)]
    pub sample: DeploymentSample,
}

#[derive(Debug, Clone, Serialize)]
pub struct HostPoint {
    pub ts: i64,
    pub cpu_percent: f64,
    pub cpu_percent_max: f64,
    pub memory_used: i64,
    pub memory_total: i64,
    pub swap_used: i64,
    pub swap_total: i64,
    pub load_one: f64,
    pub load_five: f64,
    pub load_fifteen: f64,
    pub network_rx_bps: f64,
    pub network_tx_bps: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct FilesystemPoint {
    pub ts: i64,
    pub mount_point: String,
    pub total_bytes: i64,
    pub available_bytes: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct DeploymentPoint {
    pub ts: i64,
    pub cpu_percent: f64,
    pub cpu_percent_max: f64,
    pub memory_used: i64,
    pub memory_limit: Option<i64>,
    pub network_rx_bps: f64,
    pub network_tx_bps: f64,
}

pub async fn record_host(pool: &SqlitePool, sample: &HostSample) -> Result<()> {
    sqlx::query!(
        r#"insert or replace into host_samples (
            ts, window_seconds, cpu_percent, memory_used, memory_total,
            swap_used, swap_total, load_one, load_five, load_fifteen,
            network_rx_bytes, network_tx_bytes
        ) values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)"#,
        sample.ts,
        sample.window_seconds,
        sample.cpu_percent,
        sample.memory_used,
        sample.memory_total,
        sample.swap_used,
        sample.swap_total,
        sample.load_one,
        sample.load_five,
        sample.load_fifteen,
        sample.network_rx_bytes,
        sample.network_tx_bytes
    )
    .execute(pool)
    .await
    .context("failed to store the host metrics sample")?;

    Ok(())
}

pub async fn record_filesystems(pool: &SqlitePool, samples: &[FilesystemSample]) -> Result<()> {
    if samples.is_empty() {
        return Ok(());
    }

    let mut tx = pool
        .begin()
        .await
        .context("failed to open a transaction for filesystem samples")?;

    for sample in samples {
        sqlx::query!(
            r#"insert or replace into filesystem_samples (ts, mount_point, total_bytes, available_bytes)
            values (?1, ?2, ?3, ?4)"#,
            sample.ts,
            sample.mount_point,
            sample.total_bytes,
            sample.available_bytes
        )
        .execute(&mut *tx)
        .await
        .with_context(|| format!("failed to store the sample for {}", sample.mount_point))?;
    }

    tx.commit()
        .await
        .context("failed to commit the filesystem samples")
}

pub async fn record_deployments(pool: &SqlitePool, samples: &[DeploymentSample]) -> Result<()> {
    if samples.is_empty() {
        return Ok(());
    }

    let mut tx = pool
        .begin()
        .await
        .context("failed to open a transaction for deployment samples")?;

    for sample in samples {
        sqlx::query!(
            r#"insert or replace into deployment_samples (
                ts, deployment_id, window_seconds, cpu_percent, memory_used,
                memory_limit, network_rx_bytes, network_tx_bytes
            ) values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)"#,
            sample.ts,
            sample.deployment_id,
            sample.window_seconds,
            sample.cpu_percent,
            sample.memory_used,
            sample.memory_limit,
            sample.network_rx_bytes,
            sample.network_tx_bytes
        )
        .execute(&mut *tx)
        .await
        .with_context(|| {
            format!(
                "failed to store the sample for deployment {}",
                sample.deployment_id
            )
        })?;
    }

    tx.commit()
        .await
        .context("failed to commit the deployment samples")
}

pub async fn known_deployment_ids(pool: &SqlitePool) -> Result<HashSet<String>> {
    let rows = sqlx::query_scalar!(r#"select id as "id!: String" from deployments"#)
        .fetch_all(pool)
        .await
        .context("failed to list deployment ids")?;

    Ok(rows.into_iter().collect())
}

pub async fn prune(pool: &SqlitePool, before: i64) -> Result<u64> {
    let mut removed = sqlx::query!("delete from host_samples where ts < ?", before)
        .execute(pool)
        .await
        .context("failed to prune host samples")?
        .rows_affected();

    removed += sqlx::query!("delete from filesystem_samples where ts < ?", before)
        .execute(pool)
        .await
        .context("failed to prune filesystem samples")?
        .rows_affected();

    removed += sqlx::query!("delete from deployment_samples where ts < ?", before)
        .execute(pool)
        .await
        .context("failed to prune deployment samples")?
        .rows_affected();

    Ok(removed)
}

pub async fn latest_host(pool: &SqlitePool) -> Result<Option<HostSample>> {
    let sample = sqlx::query_as!(
        HostSample,
        r#"select
            ts as "ts!: i64", window_seconds as "window_seconds!: i64",
            cpu_percent as "cpu_percent!: f64",
            memory_used as "memory_used!: i64", memory_total as "memory_total!: i64",
            swap_used as "swap_used!: i64", swap_total as "swap_total!: i64",
            load_one as "load_one!: f64", load_five as "load_five!: f64",
            load_fifteen as "load_fifteen!: f64",
            network_rx_bytes as "network_rx_bytes!: i64",
            network_tx_bytes as "network_tx_bytes!: i64"
        from host_samples order by ts desc limit 1"#
    )
    .fetch_optional(pool)
    .await
    .context("failed to read the latest host sample")?;

    Ok(sample)
}

pub async fn latest_filesystems(pool: &SqlitePool) -> Result<Vec<FilesystemSample>> {
    let samples = sqlx::query_as!(
        FilesystemSample,
        r#"select
            ts as "ts!: i64", mount_point as "mount_point!: String",
            total_bytes as "total_bytes!: i64", available_bytes as "available_bytes!: i64"
        from filesystem_samples
        where ts = (select max(ts) from filesystem_samples)
        order by mount_point"#
    )
    .fetch_all(pool)
    .await
    .context("failed to read the latest filesystem samples")?;

    Ok(samples)
}

pub async fn latest_deployments(pool: &SqlitePool) -> Result<Vec<NamedDeploymentSample>> {
    let rows = sqlx::query!(
        r#"select
            d.name as "name!: String",
            s.ts as "ts!: i64", s.deployment_id as "deployment_id!: String",
            s.window_seconds as "window_seconds!: i64",
            s.cpu_percent as "cpu_percent!: f64",
            s.memory_used as "memory_used!: i64",
            s.memory_limit as "memory_limit: i64",
            s.network_rx_bytes as "network_rx_bytes!: i64",
            s.network_tx_bytes as "network_tx_bytes!: i64"
        from deployment_samples s
        join deployments d on d.id = s.deployment_id
        where s.ts = (select max(ts) from deployment_samples)
        order by d.name"#
    )
    .fetch_all(pool)
    .await
    .context("failed to read the latest deployment samples")?;

    Ok(rows
        .into_iter()
        .map(|row| NamedDeploymentSample {
            name: row.name,
            sample: DeploymentSample {
                ts: row.ts,
                deployment_id: row.deployment_id,
                window_seconds: row.window_seconds,
                cpu_percent: row.cpu_percent,
                memory_used: row.memory_used,
                memory_limit: row.memory_limit,
                network_rx_bytes: row.network_rx_bytes,
                network_tx_bytes: row.network_tx_bytes,
            },
        })
        .collect())
}

pub async fn host_series(
    pool: &SqlitePool,
    from: i64,
    to: i64,
    step: i64,
) -> Result<Vec<HostPoint>> {
    let points = sqlx::query_as!(
        HostPoint,
        r#"select
            (ts / ?1) * ?1 as "ts!: i64",
            avg(cpu_percent) as "cpu_percent!: f64",
            max(cpu_percent) as "cpu_percent_max!: f64",
            cast(avg(memory_used) as integer) as "memory_used!: i64",
            cast(max(memory_total) as integer) as "memory_total!: i64",
            cast(avg(swap_used) as integer) as "swap_used!: i64",
            cast(max(swap_total) as integer) as "swap_total!: i64",
            avg(load_one) as "load_one!: f64",
            avg(load_five) as "load_five!: f64",
            avg(load_fifteen) as "load_fifteen!: f64",
            sum(network_rx_bytes) * 1.0 / sum(window_seconds) as "network_rx_bps!: f64",
            sum(network_tx_bytes) * 1.0 / sum(window_seconds) as "network_tx_bps!: f64"
        from host_samples
        where ts >= ?2 and ts <= ?3
        group by 1
        order by 1"#,
        step,
        from,
        to
    )
    .fetch_all(pool)
    .await
    .context("failed to read the host metrics series")?;

    Ok(points)
}

pub async fn filesystem_series(
    pool: &SqlitePool,
    from: i64,
    to: i64,
    step: i64,
) -> Result<Vec<FilesystemPoint>> {
    let points = sqlx::query_as!(
        FilesystemPoint,
        r#"select
            (ts / ?1) * ?1 as "ts!: i64",
            mount_point as "mount_point!: String",
            cast(max(total_bytes) as integer) as "total_bytes!: i64",
            cast(avg(available_bytes) as integer) as "available_bytes!: i64"
        from filesystem_samples
        where ts >= ?2 and ts <= ?3
        group by 1, 2
        order by 2, 1"#,
        step,
        from,
        to
    )
    .fetch_all(pool)
    .await
    .context("failed to read the filesystem metrics series")?;

    Ok(points)
}

pub async fn deployment_series(
    pool: &SqlitePool,
    deployment_id: &str,
    from: i64,
    to: i64,
    step: i64,
) -> Result<Vec<DeploymentPoint>> {
    let points = sqlx::query_as!(
        DeploymentPoint,
        r#"select
            (ts / ?1) * ?1 as "ts!: i64",
            avg(cpu_percent) as "cpu_percent!: f64",
            max(cpu_percent) as "cpu_percent_max!: f64",
            cast(avg(memory_used) as integer) as "memory_used!: i64",
            cast(max(memory_limit) as integer) as "memory_limit: i64",
            sum(network_rx_bytes) * 1.0 / sum(window_seconds) as "network_rx_bps!: f64",
            sum(network_tx_bytes) * 1.0 / sum(window_seconds) as "network_tx_bps!: f64"
        from deployment_samples
        where deployment_id = ?4 and ts >= ?2 and ts <= ?3
        group by 1
        order by 1"#,
        step,
        from,
        to,
        deployment_id
    )
    .fetch_all(pool)
    .await
    .context("failed to read the deployment metrics series")?;

    Ok(points)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host(ts: i64, cpu: f64, memory: i64, rx: i64) -> HostSample {
        HostSample {
            ts,
            window_seconds: 15,
            cpu_percent: cpu,
            memory_used: memory,
            memory_total: 8_000,
            swap_used: 0,
            swap_total: 1_000,
            load_one: 1.0,
            load_five: 1.0,
            load_fifteen: 1.0,
            network_rx_bytes: rx,
            network_tx_bytes: rx / 2,
        }
    }

    fn filesystem(ts: i64, mount_point: &str, available: i64) -> FilesystemSample {
        FilesystemSample {
            ts,
            mount_point: mount_point.to_string(),
            total_bytes: 1_000,
            available_bytes: available,
        }
    }

    fn deployment(ts: i64, cpu: f64, memory: i64) -> DeploymentSample {
        DeploymentSample {
            ts,
            deployment_id: "d1".to_string(),
            window_seconds: 15,
            cpu_percent: cpu,
            memory_used: memory,
            memory_limit: Some(4_096),
            network_rx_bytes: 300,
            network_tx_bytes: 150,
        }
    }

    async fn seed_deployment(pool: &SqlitePool) {
        sqlx::query!("insert into deployments (id, name) values ('d1', 'app')")
            .execute(pool)
            .await
            .unwrap();
    }

    #[sqlx::test]
    async fn a_host_sample_round_trips(pool: SqlitePool) {
        record_host(&pool, &host(100, 12.5, 2_048, 900))
            .await
            .unwrap();

        let latest = latest_host(&pool).await.unwrap().unwrap();

        assert_eq!(latest.ts, 100);
        assert_eq!(latest.cpu_percent, 12.5);
        assert_eq!(latest.memory_used, 2_048);
        assert_eq!(latest.network_rx_bytes, 900);
    }

    #[sqlx::test]
    async fn the_latest_host_sample_is_the_newest_one(pool: SqlitePool) {
        record_host(&pool, &host(100, 10.0, 1_000, 0))
            .await
            .unwrap();
        record_host(&pool, &host(200, 20.0, 2_000, 0))
            .await
            .unwrap();

        assert_eq!(latest_host(&pool).await.unwrap().unwrap().ts, 200);
    }

    #[sqlx::test]
    async fn resampling_a_second_replaces_the_earlier_sample(pool: SqlitePool) {
        record_host(&pool, &host(100, 10.0, 1_000, 0))
            .await
            .unwrap();
        record_host(&pool, &host(100, 40.0, 4_000, 0))
            .await
            .unwrap();

        let series = host_series(&pool, 0, 1_000, 15).await.unwrap();

        assert_eq!(series.len(), 1);
        assert_eq!(series[0].cpu_percent, 40.0);
    }

    #[sqlx::test]
    async fn samples_inside_one_step_are_averaged(pool: SqlitePool) {
        record_host(&pool, &host(0, 10.0, 1_000, 0)).await.unwrap();
        record_host(&pool, &host(15, 30.0, 3_000, 0)).await.unwrap();
        record_host(&pool, &host(60, 50.0, 5_000, 0)).await.unwrap();

        let series = host_series(&pool, 0, 100, 60).await.unwrap();

        assert_eq!(series.len(), 2);
        assert_eq!(series[0].ts, 0);
        assert_eq!(series[0].cpu_percent, 20.0);
        assert_eq!(series[0].memory_used, 2_000);
        assert_eq!(series[1].ts, 60);
        assert_eq!(series[1].cpu_percent, 50.0);
    }

    #[sqlx::test]
    async fn a_bucket_keeps_the_peak_alongside_the_average(pool: SqlitePool) {
        record_host(&pool, &host(0, 5.0, 1_000, 0)).await.unwrap();
        record_host(&pool, &host(15, 95.0, 1_000, 0)).await.unwrap();

        let series = host_series(&pool, 0, 100, 60).await.unwrap();

        assert_eq!(series[0].cpu_percent, 50.0);
        assert_eq!(series[0].cpu_percent_max, 95.0);
    }

    #[sqlx::test]
    async fn traffic_becomes_a_rate_over_the_sampled_window(pool: SqlitePool) {
        record_host(&pool, &host(0, 0.0, 0, 1_500)).await.unwrap();
        record_host(&pool, &host(15, 0.0, 0, 1_500)).await.unwrap();

        let series = host_series(&pool, 0, 100, 60).await.unwrap();

        assert_eq!(series[0].network_rx_bps, 100.0);
        assert_eq!(series[0].network_tx_bps, 50.0);
    }

    #[sqlx::test]
    async fn a_series_only_covers_the_requested_range(pool: SqlitePool) {
        record_host(&pool, &host(0, 10.0, 0, 0)).await.unwrap();
        record_host(&pool, &host(600, 20.0, 0, 0)).await.unwrap();
        record_host(&pool, &host(1_200, 30.0, 0, 0)).await.unwrap();

        let series = host_series(&pool, 500, 700, 15).await.unwrap();

        assert_eq!(series.len(), 1);
        assert_eq!(series[0].cpu_percent, 20.0);
    }

    #[sqlx::test]
    async fn filesystems_are_reported_per_mount_point(pool: SqlitePool) {
        record_filesystems(
            &pool,
            &[filesystem(0, "/", 500), filesystem(0, "/var", 200)],
        )
        .await
        .unwrap();
        record_filesystems(&pool, &[filesystem(60, "/", 300)])
            .await
            .unwrap();

        let series = filesystem_series(&pool, 0, 100, 60).await.unwrap();

        assert_eq!(series.len(), 3);
        assert_eq!(series[0].mount_point, "/");
        assert_eq!(series[0].available_bytes, 500);
        assert_eq!(series[1].mount_point, "/");
        assert_eq!(series[1].available_bytes, 300);
        assert_eq!(series[2].mount_point, "/var");

        let latest = latest_filesystems(&pool).await.unwrap();
        assert_eq!(latest.len(), 1);
        assert_eq!(latest[0].ts, 60);
    }

    #[sqlx::test]
    async fn a_deployment_series_covers_only_that_deployment(pool: SqlitePool) {
        seed_deployment(&pool).await;
        sqlx::query!("insert into deployments (id, name) values ('d2', 'other')")
            .execute(&pool)
            .await
            .unwrap();

        let mut other = deployment(0, 99.0, 9_000);
        other.deployment_id = "d2".to_string();

        record_deployments(&pool, &[deployment(0, 10.0, 1_000), other])
            .await
            .unwrap();

        let series = deployment_series(&pool, "d1", 0, 100, 60).await.unwrap();

        assert_eq!(series.len(), 1);
        assert_eq!(series[0].cpu_percent, 10.0);
        assert_eq!(series[0].memory_limit, Some(4_096));
        assert_eq!(series[0].network_rx_bps, 20.0);
    }

    #[sqlx::test]
    async fn the_latest_deployment_samples_carry_the_deployment_name(pool: SqlitePool) {
        seed_deployment(&pool).await;
        record_deployments(&pool, &[deployment(0, 10.0, 1_000)])
            .await
            .unwrap();
        record_deployments(&pool, &[deployment(60, 20.0, 2_000)])
            .await
            .unwrap();

        let latest = latest_deployments(&pool).await.unwrap();

        assert_eq!(latest.len(), 1);
        assert_eq!(latest[0].name, "app");
        assert_eq!(latest[0].sample.ts, 60);
        assert_eq!(latest[0].sample.cpu_percent, 20.0);
    }

    #[sqlx::test]
    async fn deleting_a_deployment_takes_its_samples_with_it(pool: SqlitePool) {
        seed_deployment(&pool).await;
        record_deployments(&pool, &[deployment(0, 10.0, 1_000)])
            .await
            .unwrap();

        sqlx::query!("delete from deployments where id = 'd1'")
            .execute(&pool)
            .await
            .unwrap();

        assert!(
            deployment_series(&pool, "d1", 0, 100, 60)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[sqlx::test]
    async fn pruning_drops_samples_older_than_the_cutoff(pool: SqlitePool) {
        seed_deployment(&pool).await;
        record_host(&pool, &host(100, 10.0, 0, 0)).await.unwrap();
        record_host(&pool, &host(900, 20.0, 0, 0)).await.unwrap();
        record_filesystems(&pool, &[filesystem(100, "/", 500)])
            .await
            .unwrap();
        record_deployments(&pool, &[deployment(100, 10.0, 1_000)])
            .await
            .unwrap();

        let removed = prune(&pool, 500).await.unwrap();

        assert_eq!(removed, 3);
        assert_eq!(latest_host(&pool).await.unwrap().unwrap().ts, 900);
        assert!(latest_filesystems(&pool).await.unwrap().is_empty());
        assert!(latest_deployments(&pool).await.unwrap().is_empty());
    }

    #[sqlx::test]
    async fn known_ids_only_lists_deployments_that_exist(pool: SqlitePool) {
        seed_deployment(&pool).await;

        let ids = known_deployment_ids(&pool).await.unwrap();

        assert!(ids.contains("d1"));
        assert!(!ids.contains("d2"));
    }
}
