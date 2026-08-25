use std::collections::HashMap;

use anyhow::{Context, Result, bail};
use bollard::{
    Docker,
    errors::Error,
    models::NetworkCreateRequest,
    query_parameters::{
        CreateContainerOptionsBuilder, CreateImageOptionsBuilder, InspectContainerOptionsBuilder,
        ListNetworksOptionsBuilder, LogsOptionsBuilder, RemoveContainerOptionsBuilder,
        StopContainerOptionsBuilder,
    },
};
use futures_util::TryStreamExt;

use crate::{constants::MAX_CONTAINER_LOG_BYTES, infra::containers::spec::ContainerSpec};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContainerStatus {
    pub running: bool,
    pub exit_code: Option<i64>,
}

#[derive(Clone)]
pub struct DockerClient(Docker);

fn is_not_found(error: &Error) -> bool {
    matches!(
        error,
        Error::DockerResponseServerError {
            status_code: 404,
            ..
        }
    )
}

fn is_conflict(error: &Error) -> bool {
    matches!(
        error,
        Error::DockerResponseServerError {
            status_code: 409,
            ..
        }
    )
}

impl DockerClient {
    pub async fn connect() -> Result<Self> {
        let docker =
            Docker::connect_with_local_defaults().context("failed to connect to docker daemon")?;

        let docker = docker
            .negotiate_version()
            .await
            .context("failed to negotiate the docker api version")?;

        Ok(Self(docker))
    }

    pub async fn pull_image(&self, image: &str) -> Result<()> {
        let options = CreateImageOptionsBuilder::default()
            .from_image(image)
            .build();

        let mut stream = self.0.create_image(Some(options), None, None);

        while let Some(info) = stream
            .try_next()
            .await
            .with_context(|| format!("failed to pull image {image}"))?
        {
            if let Some(detail) = info.error_detail {
                bail!(
                    "failed to pull image {image}: {}",
                    detail
                        .message
                        .unwrap_or_else(|| "unknown error".to_string())
                );
            }
        }

        Ok(())
    }

    pub async fn ensure_network(&self, name: &str) -> Result<()> {
        let mut filters = HashMap::new();
        filters.insert("name", vec![name]);
        let options = ListNetworksOptionsBuilder::default()
            .filters(&filters)
            .build();

        let networks = self
            .0
            .list_networks(Some(options))
            .await
            .context("failed to list docker networks")?;

        if networks.iter().any(|n| n.name.as_deref() == Some(name)) {
            return Ok(());
        }

        match self
            .0
            .create_network(NetworkCreateRequest {
                name: name.to_string(),
                driver: Some("bridge".to_string()),
                ..Default::default()
            })
            .await
        {
            Ok(_) => Ok(()),
            Err(e) if is_conflict(&e) => Ok(()),
            Err(e) => Err(e).with_context(|| format!("failed to create docker network {name}")),
        }
    }

    pub async fn inspect(&self, name: &str) -> Result<Option<ContainerStatus>> {
        let options = InspectContainerOptionsBuilder::default().build();

        match self.0.inspect_container(name, Some(options)).await {
            Ok(response) => {
                let state = response.state.unwrap_or_default();
                Ok(Some(ContainerStatus {
                    running: state.running.unwrap_or(false),
                    exit_code: state.exit_code,
                }))
            }
            Err(e) if is_not_found(&e) => Ok(None),
            Err(e) => Err(e).with_context(|| format!("failed to inspect container {name}")),
        }
    }

    pub async fn create_and_start(&self, spec: &ContainerSpec) -> Result<String> {
        self.pull_image(&spec.image).await?;

        let options = CreateContainerOptionsBuilder::default()
            .name(&spec.name)
            .build();

        let response = self
            .0
            .create_container(Some(options), spec.to_create_body())
            .await
            .with_context(|| format!("failed to create container {}", spec.name))?;

        self.start(&spec.name).await?;

        Ok(response.id)
    }

    pub async fn start(&self, name: &str) -> Result<()> {
        self.0
            .start_container(name, None)
            .await
            .with_context(|| format!("failed to start container {name}"))?;
        Ok(())
    }

    pub async fn ensure_running(&self, spec: &ContainerSpec) -> Result<()> {
        match self.inspect(&spec.name).await? {
            Some(status) if status.running => Ok(()),
            Some(_) => self.start(&spec.name).await,
            None => match self.create_and_start(spec).await {
                Ok(_) => Ok(()),
                Err(e) => match e.downcast_ref::<Error>() {
                    Some(inner) if is_conflict(inner) => self.start(&spec.name).await,
                    _ => Err(e),
                },
            },
        }
    }

    pub async fn logs(&self, container: &str, tail: i64) -> Result<String> {
        let options = LogsOptionsBuilder::default()
            .stdout(true)
            .stderr(true)
            .tail(&tail.to_string())
            .build();

        let mut stream = self.0.logs(container, Some(options));
        let mut buffer: Vec<u8> = Vec::new();

        while let Some(chunk) = stream
            .try_next()
            .await
            .with_context(|| format!("failed to read logs for container {container}"))?
        {
            buffer.extend_from_slice(&chunk.into_bytes());

            if buffer.len() > MAX_CONTAINER_LOG_BYTES {
                let cut = buffer.len() - MAX_CONTAINER_LOG_BYTES;
                buffer.drain(..cut);
            }
        }

        Ok(String::from_utf8_lossy(&buffer).into_owned())
    }

    pub async fn stop_and_remove(&self, container: &str) -> Result<()> {
        let options = StopContainerOptionsBuilder::default().build();

        match self.0.stop_container(container, Some(options)).await {
            Ok(()) => {}
            Err(e) if is_not_found(&e) => {}
            Err(e) => {
                return Err(e).with_context(|| format!("failed to stop container {container}"));
            }
        }

        let options = RemoveContainerOptionsBuilder::default().force(true).build();

        match self.0.remove_container(container, Some(options)).await {
            Ok(()) => Ok(()),
            Err(e) if is_not_found(&e) => Ok(()),
            Err(e) => Err(e).with_context(|| format!("failed to remove container {container}")),
        }
    }
}
