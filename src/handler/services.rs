use axum::extract::{Path, State};

use crate::{
    constants::CADDY_READY_TIMEOUT,
    infra::{
        containers::{
            docker::DockerClient,
            services::{self, ServiceId, ServiceStatus},
        },
        proxy::reconciler,
    },
    state::AppState,
    utils::api_response::{ApiError, ApiResponse},
};

fn docker(state: &AppState) -> Result<&DockerClient, ApiError> {
    state.docker.as_ref().ok_or_else(|| {
        ApiError::unavailable(
            "docker is not available on this host, container services are disabled",
        )
    })
}

fn parse_id(id: &str) -> Result<ServiceId, ApiError> {
    ServiceId::parse(id).ok_or_else(|| ApiError::not_found(format!("no such service {id}")))
}

pub async fn get_services(
    State(state): State<AppState>,
) -> Result<ApiResponse<Vec<ServiceStatus>>, ApiError> {
    let docker = docker(&state)?;

    let statuses = services::status_all(&state.db, docker)
        .await
        .map_err(ApiError::internal)?;

    Ok(ApiResponse::ok(statuses, "services fetched"))
}

async fn set_enabled(
    state: AppState,
    id: &str,
    enabled: bool,
) -> Result<ApiResponse<ServiceStatus>, ApiError> {
    let docker = docker(&state)?;
    let id = parse_id(id)?;

    services::set_enabled(&state.db, docker, id, enabled)
        .await
        .map_err(ApiError::internal)?;

    let status = services::status(&state.db, docker, id)
        .await
        .map_err(ApiError::internal)?;

    let mut message = if enabled {
        "service enabled"
    } else {
        "service disabled"
    };

    if enabled && id == ServiceId::ReverseProxy {
        match reconciler::apply_when_ready(
            &state.db,
            &state.master_key,
            &state.caddy,
            CADDY_READY_TIMEOUT,
        )
        .await
        {
            Ok(_) => message = "service enabled and proxy routes applied",
            Err(e) => {
                tracing::warn!(error = ?e, "started the proxy but could not apply its routes");
                message = "service enabled, but the proxy routes could not be applied";
            }
        }
    }

    Ok(ApiResponse::ok(status, message))
}

pub async fn enable_service(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<ApiResponse<ServiceStatus>, ApiError> {
    set_enabled(state, &id, true).await
}

pub async fn disable_service(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<ApiResponse<ServiceStatus>, ApiError> {
    set_enabled(state, &id, false).await
}
