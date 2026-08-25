use anyhow::{Context, Result};
use sqlx::SqlitePool;

use crate::{
    constants::CONTAINER_NETWORK_NAME,
    infra::containers::{docker::DockerClient, services},
};

pub async fn run(pool: &SqlitePool, docker: &DockerClient) -> Result<()> {
    docker
        .ensure_network(CONTAINER_NETWORK_NAME)
        .await
        .context("failed to prepare the container network")?;

    services::converge_all(pool, docker).await
}
