use axum::extract::{Path, State};
use serde::Serialize;

use crate::{
    db::queries::packages::{self, DesiredState, PackageStatus, PackageView},
    infra::packages::reconciler,
    state::AppState,
    utils::api_response::{ApiError, ApiResponse},
};

#[derive(Serialize)]
pub struct PackageResponse {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub desired_state: DesiredState,
    pub status: PackageStatus,
    pub installed: bool,
    pub last_error: Option<String>,
}

impl From<PackageView> for PackageResponse {
    fn from(pkg: PackageView) -> Self {
        Self {
            installed: pkg.status == PackageStatus::Installed,
            id: pkg.id,
            name: pkg.name,
            description: pkg.description,
            desired_state: pkg.desired_state,
            status: pkg.status,
            last_error: pkg.last_error,
        }
    }
}

async fn fetch_packages(state: &AppState) -> Result<Vec<PackageResponse>, ApiError> {
    Ok(
        packages::fetch_all_for_manager(&state.db, state.package_manager.id())
            .await
            .map_err(ApiError::internal)?
            .into_iter()
            .map(PackageResponse::from)
            .collect(),
    )
}

pub async fn get_packages(
    State(state): State<AppState>,
) -> Result<ApiResponse<Vec<PackageResponse>>, ApiError> {
    Ok(ApiResponse::ok(
        fetch_packages(&state).await?,
        "packages fetched",
    ))
}

pub async fn resync_packages(
    State(state): State<AppState>,
) -> Result<ApiResponse<Vec<PackageResponse>>, ApiError> {
    reconciler::sync_all(&state.db, &state.package_manager)
        .await
        .map_err(ApiError::internal)?;

    state.reconcile_notify.notify_one();

    Ok(ApiResponse::ok(
        fetch_packages(&state).await?,
        "packages resynced",
    ))
}

pub async fn install_package(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<ApiResponse<()>, ApiError> {
    let found = packages::set_desired_state(
        &state.db,
        &id,
        state.package_manager.id(),
        DesiredState::Installed,
    )
    .await
    .map_err(ApiError::internal)?;

    if !found {
        return Err(ApiError::not_found(format!(
            "package {id} not found for the detected package manager ({})",
            state.package_manager.id()
        )));
    }

    state.reconcile_notify.notify_one();
    Ok(ApiResponse::ok((), "install requested"))
}

pub async fn remove_package(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<ApiResponse<()>, ApiError> {
    let found = packages::set_desired_state(
        &state.db,
        &id,
        state.package_manager.id(),
        DesiredState::Removed,
    )
    .await
    .map_err(ApiError::internal)?;

    if !found {
        return Err(ApiError::not_found(format!(
            "package {id} not found for the detected package manager ({})",
            state.package_manager.id()
        )));
    }

    state.reconcile_notify.notify_one();
    Ok(ApiResponse::ok((), "removal requested"))
}
