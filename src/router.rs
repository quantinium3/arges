use axum::{Router, routing::get};
use tower_http::trace::{DefaultMakeSpan, DefaultOnResponse, TraceLayer};
use tracing::Level;

use crate::state::AppState;

pub fn routes(state: AppState) -> Router {
    let api_router = Router::new();

    Router::new()
        .route("/", get(|| async { "Welcome to Arges" }))
        .nest("/api", api_router)
        .with_state(state)
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(DefaultMakeSpan::new().level(Level::INFO))
                .on_response(DefaultOnResponse::new().level(Level::INFO)),
        )
}
