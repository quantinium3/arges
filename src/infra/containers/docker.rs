use std::{
    collections::HashMap,
    io::ErrorKind,
    net::{SocketAddr, TcpListener, UdpSocket},
};

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

use crate::{
    constants::MAX_CONTAINER_LOG_BYTES,
    infra::containers::spec::{ContainerSpec, Protocol},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContainerStatus {
    pub running: bool,
    pub exit_code: Option<i64>,
    pub published: Vec<String>,
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

fn ensure_host_ports_free(spec: &ContainerSpec) -> Result<()> {
    for mapping in &spec.ports {
        let addr = SocketAddr::new(mapping.host_ip, mapping.host_port);

        let probe = match mapping.protocol {
            Protocol::Tcp => TcpListener::bind(addr).map(|_| ()),
            Protocol::Udp => UdpSocket::bind(addr).map(|_| ()),
        };

        if probe
            .as_ref()
            .is_err_and(|e| e.kind() == ErrorKind::AddrInUse)
        {
            bail!(
                "cannot start container {}: host port {}/{} is already in use by another process",
                spec.name,
                mapping.host_port,
                mapping.protocol.as_str()
            );
        }
    }

    Ok(())
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

                let mut published: Vec<String> = response
                    .network_settings
                    .and_then(|settings| settings.ports)
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|(_, bindings)| bindings.as_ref().is_some_and(|b| !b.is_empty()))
                    .map(|(key, _)| key)
                    .collect();
                published.sort();
                published.dedup();

                Ok(Some(ContainerStatus {
                    running: state.running.unwrap_or(false),
                    exit_code: state.exit_code,
                    published,
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

        if let Err(e) = self.start(&spec.name).await {
            let _ = self.stop_and_remove(&spec.name).await;
            return Err(e);
        }

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
        let expected = spec.published_keys();

        if let Some(status) = self.inspect(&spec.name).await? {
            if status.running && status.published == expected {
                return Ok(());
            }

            self.stop_and_remove(&spec.name)
                .await
                .with_context(|| format!("failed to replace container {}", spec.name))?;
        }

        ensure_host_ports_free(spec)?;
        self.create_and_start(spec).await?;

        Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infra::containers::spec::LOOPBACK;

    #[test]
    fn published_keys_are_sorted_and_deduped() {
        let spec = ContainerSpec::new("app", "nginx")
            .public_port(443, 443)
            .public_udp_port(443, 443)
            .port(80, 8080);

        assert_eq!(spec.published_keys(), vec!["443/tcp", "443/udp", "80/tcp"]);
    }

    #[test]
    fn a_spec_without_ports_expects_nothing_published() {
        assert!(
            ContainerSpec::new("app", "nginx")
                .published_keys()
                .is_empty()
        );
    }

    #[test]
    fn a_free_port_passes_the_preflight() {
        let probe = TcpListener::bind((LOOPBACK, 0)).unwrap();
        let port = probe.local_addr().unwrap().port();
        drop(probe);

        let spec = ContainerSpec::new("app", "nginx").port(80, port);
        assert!(ensure_host_ports_free(&spec).is_ok());
    }

    #[test]
    fn a_taken_tcp_port_is_reported_before_the_container_is_created() {
        let held = TcpListener::bind((LOOPBACK, 0)).unwrap();
        let port = held.local_addr().unwrap().port();

        let spec = ContainerSpec::new("arges-caddy", "caddy").port(80, port);
        let err = ensure_host_ports_free(&spec).unwrap_err().to_string();

        assert!(err.contains("arges-caddy"), "{err}");
        assert!(err.contains(&format!("{port}/tcp")), "{err}");
        assert!(err.contains("already in use"), "{err}");
    }

    #[test]
    fn a_port_we_lack_permission_to_probe_is_not_treated_as_taken() {
        let spec = ContainerSpec::new("app", "caddy").public_port(80, 80);

        let probe = TcpListener::bind((crate::infra::containers::spec::ALL_INTERFACES, 80u16));
        let privileged =
            probe.is_err() && !matches!(probe.as_ref().unwrap_err().kind(), ErrorKind::AddrInUse);

        if privileged {
            assert!(
                ensure_host_ports_free(&spec).is_ok(),
                "a permission error must not be reported as a port conflict"
            );
        }
    }

    #[test]
    fn a_taken_udp_port_is_reported_too() {
        let held = UdpSocket::bind((LOOPBACK, 0)).unwrap();
        let port = held.local_addr().unwrap().port();

        let mut spec = ContainerSpec::new("app", "caddy");
        spec.ports
            .push(crate::infra::containers::spec::PortMapping {
                container_port: 443,
                host_port: port,
                host_ip: LOOPBACK,
                protocol: Protocol::Udp,
            });

        let err = ensure_host_ports_free(&spec).unwrap_err().to_string();
        assert!(err.contains(&format!("{port}/udp")), "{err}");
    }
}
