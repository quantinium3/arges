use serde::Serialize;

use crate::utils::api_response::{ApiResponse, ApiResult};

#[derive(Serialize)]
pub struct VersionResponse {
    version: &'static str,
}

pub async fn get_version() -> ApiResult<VersionResponse> {
    let response = VersionResponse {
        version: env!("CARGO_PKG_VERSION"),
    };

    Ok(ApiResponse::ok(response, "fetched server version"))
}
