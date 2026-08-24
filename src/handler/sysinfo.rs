use anyhow::Context;
use tokio::task;

use crate::{
    infra::sysinfo,
    infra::sysinfo::SysinfoResponse,
    utils::api_response::{ApiResponse, ApiResult},
};

pub async fn get_sysinfo() -> ApiResult<SysinfoResponse> {
    let info = task::spawn_blocking(sysinfo::collect)
        .await
        .context("failed to collect system information")?;

    Ok(ApiResponse::ok(info, "fetched system information"))
}
