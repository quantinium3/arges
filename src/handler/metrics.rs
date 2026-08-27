use std::{
    collections::BTreeMap,
    time::{SystemTime, UNIX_EPOCH},
};

use axum::extract::{Path, Query, State};
use serde::{Deserialize, Serialize};

use crate::{
    constants::{
        METRICS_DEFAULT_RANGE, METRICS_MAX_POINTS, METRICS_RETENTION, METRICS_SAMPLE_INTERVAL,
    },
    db::queries::{
        deployments,
        metrics::{
            self, DeploymentPoint, FilesystemSample, HostPoint, HostSample, NamedDeploymentSample,
        },
    },
    state::AppState,
    utils::api_response::{ApiError, ApiResponse, ApiResult},
};

#[derive(Deserialize)]
pub struct RangeQuery {
    pub from: Option<i64>,
    pub to: Option<i64>,
    pub step: Option<i64>,
}

#[derive(Serialize)]
pub struct MetricsSnapshot {
    host: Option<HostSample>,
    filesystems: Vec<FilesystemSample>,
    deployments: Vec<NamedDeploymentSample>,
    sample_interval: i64,
    retention: i64,
}

#[derive(Serialize)]
pub struct FilesystemSeries {
    mount_point: String,
    points: Vec<FilesystemPointValue>,
}

#[derive(Serialize)]
pub struct FilesystemPointValue {
    ts: i64,
    total_bytes: i64,
    available_bytes: i64,
}

#[derive(Serialize)]
pub struct HostHistory {
    from: i64,
    to: i64,
    step: i64,
    host: Vec<HostPoint>,
    filesystems: Vec<FilesystemSeries>,
}

#[derive(Serialize)]
pub struct DeploymentHistory {
    from: i64,
    to: i64,
    step: i64,
    deployment_id: String,
    name: String,
    points: Vec<DeploymentPoint>,
}

struct Window {
    from: i64,
    to: i64,
    step: i64,
}

fn ceil_div(value: i64, divisor: i64) -> i64 {
    (value + divisor - 1) / divisor
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since| since.as_secs() as i64)
        .unwrap_or_default()
}

fn resolve(query: &RangeQuery) -> Result<Window, ApiError> {
    let interval = METRICS_SAMPLE_INTERVAL.as_secs() as i64;
    let retention = METRICS_RETENTION.as_secs() as i64;
    let now = now();

    let to = query.to.unwrap_or(now);
    let from = query
        .from
        .unwrap_or(to - METRICS_DEFAULT_RANGE.as_secs() as i64);

    if from >= to {
        return Err(ApiError::bad_request("from must be earlier than to"));
    }

    if let Some(step) = query.step
        && step <= 0
    {
        return Err(ApiError::bad_request(
            "step must be a positive number of seconds",
        ));
    }

    let from = from.max(to - retention);
    let span = to - from;

    let step = query
        .step
        .unwrap_or(interval)
        .max(ceil_div(span, METRICS_MAX_POINTS))
        .max(interval);
    let step = ceil_div(step, interval) * interval;

    Ok(Window { from, to, step })
}

pub async fn get_metrics(State(state): State<AppState>) -> ApiResult<MetricsSnapshot> {
    let host = metrics::latest_host(&state.db)
        .await
        .map_err(ApiError::internal)?;
    let filesystems = metrics::latest_filesystems(&state.db)
        .await
        .map_err(ApiError::internal)?;
    let deployments = metrics::latest_deployments(&state.db)
        .await
        .map_err(ApiError::internal)?;

    Ok(ApiResponse::ok(
        MetricsSnapshot {
            host,
            filesystems,
            deployments,
            sample_interval: METRICS_SAMPLE_INTERVAL.as_secs() as i64,
            retention: METRICS_RETENTION.as_secs() as i64,
        },
        "fetched the latest metrics",
    ))
}

pub async fn get_metrics_history(
    State(state): State<AppState>,
    Query(query): Query<RangeQuery>,
) -> ApiResult<HostHistory> {
    let window = resolve(&query)?;

    let host = metrics::host_series(&state.db, window.from, window.to, window.step)
        .await
        .map_err(ApiError::internal)?;

    let points = metrics::filesystem_series(&state.db, window.from, window.to, window.step)
        .await
        .map_err(ApiError::internal)?;

    let mut grouped: BTreeMap<String, Vec<FilesystemPointValue>> = BTreeMap::new();

    for point in points {
        grouped
            .entry(point.mount_point)
            .or_default()
            .push(FilesystemPointValue {
                ts: point.ts,
                total_bytes: point.total_bytes,
                available_bytes: point.available_bytes,
            });
    }

    Ok(ApiResponse::ok(
        HostHistory {
            from: window.from,
            to: window.to,
            step: window.step,
            host,
            filesystems: grouped
                .into_iter()
                .map(|(mount_point, points)| FilesystemSeries {
                    mount_point,
                    points,
                })
                .collect(),
        },
        "fetched the host metrics history",
    ))
}

pub async fn get_deployment_metrics(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<RangeQuery>,
) -> ApiResult<DeploymentHistory> {
    let window = resolve(&query)?;

    let deployment = deployments::fetch(&state.db, &id)
        .await
        .map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::not_found(format!("deployment {id} not found")))?;

    let points = metrics::deployment_series(
        &state.db,
        &deployment.id,
        window.from,
        window.to,
        window.step,
    )
    .await
    .map_err(ApiError::internal)?;

    Ok(ApiResponse::ok(
        DeploymentHistory {
            from: window.from,
            to: window.to,
            step: window.step,
            deployment_id: deployment.id,
            name: deployment.name,
            points,
        },
        "fetched the deployment metrics history",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn query(from: Option<i64>, to: Option<i64>, step: Option<i64>) -> RangeQuery {
        RangeQuery { from, to, step }
    }

    #[test]
    fn a_range_without_a_step_falls_back_to_the_sample_interval() {
        let window = resolve(&query(Some(1_000), Some(2_000), None)).unwrap();

        assert_eq!(window.from, 1_000);
        assert_eq!(window.to, 2_000);
        assert_eq!(window.step, METRICS_SAMPLE_INTERVAL.as_secs() as i64);
    }

    #[test]
    fn a_step_below_the_sample_interval_is_raised_to_it() {
        let window = resolve(&query(Some(1_000), Some(2_000), Some(1))).unwrap();

        assert_eq!(window.step, METRICS_SAMPLE_INTERVAL.as_secs() as i64);
    }

    #[test]
    fn a_step_is_aligned_to_whole_sample_intervals() {
        let window = resolve(&query(Some(0), Some(3_600), Some(20))).unwrap();

        assert_eq!(window.step % METRICS_SAMPLE_INTERVAL.as_secs() as i64, 0);
        assert!(window.step >= 20);
    }

    #[test]
    fn a_long_range_is_bucketed_into_a_graphable_number_of_points() {
        let window = resolve(&query(Some(0), Some(7 * 24 * 60 * 60), None)).unwrap();

        let points = (window.to - window.from) / window.step;
        assert!(points <= METRICS_MAX_POINTS, "got {points} points");
    }

    #[test]
    fn a_range_longer_than_the_retention_window_is_clamped() {
        let retention = METRICS_RETENTION.as_secs() as i64;
        let window = resolve(&query(Some(0), Some(retention * 4), None)).unwrap();

        assert_eq!(window.to - window.from, retention);
    }

    #[test]
    fn a_backwards_range_is_rejected() {
        assert!(resolve(&query(Some(2_000), Some(1_000), None)).is_err());
        assert!(resolve(&query(Some(1_000), Some(1_000), None)).is_err());
    }

    #[test]
    fn a_zero_or_negative_step_is_rejected() {
        assert!(resolve(&query(Some(0), Some(1_000), Some(0))).is_err());
        assert!(resolve(&query(Some(0), Some(1_000), Some(-30))).is_err());
    }
}
