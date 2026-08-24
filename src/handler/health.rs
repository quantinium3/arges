use axum::{extract::State, http::StatusCode};
use serde::Serialize;

use crate::{db::queries, state::AppState, utils::api_response::ApiResponse};

#[derive(Serialize)]
pub struct HealthResponse {
    status: &'static str,
}

pub async fn get_health(State(state): State<AppState>) -> ApiResponse<HealthResponse> {
    let is_db_healthy = queries::health::is_healthy(&state.db).await;
    let status = match is_db_healthy {
        true => "OK",
        false => "ERROR",
    };

    match is_db_healthy {
        true => ApiResponse::ok(HealthResponse { status }, "server is healthy"),
        false => ApiResponse::with_status(
            StatusCode::SERVICE_UNAVAILABLE,
            HealthResponse { status },
            "server is not healthy",
        ),
    }
}
