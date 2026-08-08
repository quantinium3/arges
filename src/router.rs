use axum::{Router, routing::get};

use crate::state::AppState;

pub fn routes(state: AppState) -> Router {
    let api_router = Router::new();

    Router::new()
        .route("/", get(|| async { "Welcome to Arges" }))
        .nest("/api", api_router)
        .with_state(state)
}
