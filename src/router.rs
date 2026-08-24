use std::any::Any;

use axum::{Router, response::IntoResponse, response::Response, routing::get};
use tower_http::{
    catch_panic::CatchPanicLayer,
    trace::{DefaultMakeSpan, DefaultOnResponse, TraceLayer},
};
use tracing::Level;

use crate::{
    handler::sysinfo::get_sysinfo,
    utils::api_response::{ApiError, ApiResponse, ApiResult},
};

pub fn routes() -> Router {
    Router::new()
        .route("/", get(root))
        .route("/api/sysinfo", get(get_sysinfo))
        .fallback(not_found)
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(DefaultMakeSpan::new().level(Level::INFO))
                .on_response(DefaultOnResponse::new().level(Level::INFO)),
        )
        .layer(CatchPanicLayer::custom(on_panic))
}

async fn root() -> ApiResult<()> {
    Ok(ApiResponse::ok((), "Welcome to Arges"))
}

async fn not_found() -> ApiError {
    ApiError::not_found("no such endpoint")
}

fn on_panic(panic: Box<dyn Any + Send + 'static>) -> Response {
    let details = panic
        .downcast_ref::<&str>()
        .map(|s| (*s).to_string())
        .or_else(|| panic.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "unknown panic".to_string());

    ApiError::internal(anyhow::anyhow!("handler panicked: {details}")).into_response()
}
