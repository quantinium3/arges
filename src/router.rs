use std::any::Any;

use axum::{
    Router,
    response::IntoResponse,
    response::Response,
    routing::{get, post},
};
use tower_http::{
    catch_panic::CatchPanicLayer,
    trace::{DefaultMakeSpan, DefaultOnResponse, TraceLayer},
};
use tracing::Level;

use crate::{
    handler::{health, packages, parameters, proxy, services, sysinfo, version},
    state::AppState,
    utils::api_response::{ApiError, ApiResponse, ApiResult},
};

pub fn routes(state: AppState) -> Router {
    let package_router = Router::new()
        .route("/", get(packages::get_packages))
        .route("/resync", post(packages::resync_packages))
        .route("/{id}/install", post(packages::install_package))
        .route("/{id}/remove", post(packages::remove_package));
    let parameter_router = Router::new()
        .route("/", get(parameters::list_parameters))
        .route(
            "/{*key}",
            get(parameters::get_parameter)
                .put(parameters::put_parameter)
                .delete(parameters::delete_parameter),
        );
    let service_router = Router::new()
        .route("/", get(services::get_services))
        .route("/{id}/enable", post(services::enable_service))
        .route("/{id}/disable", post(services::disable_service));
    let proxy_router = Router::new()
        .route(
            "/",
            get(proxy::list_proxy_hosts).post(proxy::create_proxy_host),
        )
        .route("/apply", post(proxy::apply_proxy_config))
        .route("/status", get(proxy::get_proxy_status))
        .route(
            "/{id}",
            get(proxy::get_proxy_host)
                .put(proxy::update_proxy_host)
                .delete(proxy::delete_proxy_host),
        )
        .route("/{id}/enable", post(proxy::enable_proxy_host))
        .route("/{id}/disable", post(proxy::disable_proxy_host));
    let api_router = Router::new()
        .nest("/proxy", proxy_router)
        .nest("/service", service_router)
        .nest("/package", package_router)
        .nest("/parameter", parameter_router)
        .route("/health", get(health::get_health))
        .route("/sysinfo", get(sysinfo::get_sysinfo))
        .route("/version", get(version::get_version));

    Router::new()
        .route("/", get(root))
        .nest("/api", api_router)
        .fallback(not_found)
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(DefaultMakeSpan::new().level(Level::INFO))
                .on_response(DefaultOnResponse::new().level(Level::INFO)),
        )
        .layer(CatchPanicLayer::custom(on_panic))
        .with_state(state)
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
