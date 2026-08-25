use anyhow::{Context, Result};

use crate::{
    constants::{CONTAINER_NETWORK_NAME, REGISTRY_CONTAINER_NAME, REGISTRY_IMAGE, REGISTRY_PORT},
    infra::containers::{
        docker::DockerClient,
        spec::{ContainerSpec, RestartPolicy},
    },
};

pub fn registry_spec() -> ContainerSpec {
    ContainerSpec::new(REGISTRY_CONTAINER_NAME, REGISTRY_IMAGE)
        .network(CONTAINER_NETWORK_NAME)
        .port(REGISTRY_PORT, REGISTRY_PORT)
        .restart(RestartPolicy::Always)
}

pub fn registry_prefix() -> String {
    format!("localhost:{REGISTRY_PORT}")
}

pub async fn run(docker: &DockerClient) -> Result<()> {
    docker
        .ensure_network(CONTAINER_NETWORK_NAME)
        .await
        .context("failed to prepare the container network")?;

    docker
        .ensure_running(&registry_spec())
        .await
        .context("failed to start the local image registry")?;

    Ok(())
}
