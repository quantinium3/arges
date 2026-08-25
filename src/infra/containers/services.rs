use anyhow::{Context, Result};
use serde::Serialize;
use sqlx::SqlitePool;

use crate::{
    constants::{
        CADDY_ADMIN_PORT, CADDY_CONFIG_VOLUME, CADDY_CONTAINER_NAME, CADDY_DATA_VOLUME,
        CADDY_HTTP_PORT, CADDY_HTTPS_PORT, CADDY_IMAGE, CONTAINER_NETWORK_NAME,
        REGISTRY_CONTAINER_NAME, REGISTRY_IMAGE, REGISTRY_PORT,
    },
    db::queries::settings,
    infra::containers::{
        docker::DockerClient,
        spec::{ContainerSpec, RestartPolicy},
    },
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ServiceId {
    Registry,
    ReverseProxy,
}

pub const ALL_SERVICES: [ServiceId; 2] = [ServiceId::Registry, ServiceId::ReverseProxy];

impl ServiceId {
    pub fn as_str(self) -> &'static str {
        match self {
            ServiceId::Registry => "registry",
            ServiceId::ReverseProxy => "reverse-proxy",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        ALL_SERVICES.into_iter().find(|s| s.as_str() == value)
    }

    pub fn container_name(self) -> &'static str {
        match self {
            ServiceId::Registry => REGISTRY_CONTAINER_NAME,
            ServiceId::ReverseProxy => CADDY_CONTAINER_NAME,
        }
    }

    pub fn enabled_by_default(self) -> bool {
        match self {
            ServiceId::Registry => true,
            ServiceId::ReverseProxy => true,
        }
    }

    fn setting_key(self) -> String {
        format!("service.{}.enabled", self.as_str())
    }

    pub fn spec(self) -> ContainerSpec {
        match self {
            ServiceId::Registry => registry_spec(),
            ServiceId::ReverseProxy => caddy_spec(),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct ServiceStatus {
    pub id: ServiceId,
    pub container: &'static str,
    pub enabled: bool,
    pub running: bool,
}

pub fn registry_spec() -> ContainerSpec {
    ContainerSpec::new(REGISTRY_CONTAINER_NAME, REGISTRY_IMAGE)
        .network(CONTAINER_NETWORK_NAME)
        .port(REGISTRY_PORT, REGISTRY_PORT)
        .restart(RestartPolicy::Always)
}

pub fn registry_prefix() -> String {
    format!("localhost:{REGISTRY_PORT}")
}

pub fn caddy_spec() -> ContainerSpec {
    ContainerSpec::new(CADDY_CONTAINER_NAME, CADDY_IMAGE)
        .network(CONTAINER_NETWORK_NAME)
        .public_port(CADDY_HTTP_PORT, CADDY_HTTP_PORT)
        .public_port(CADDY_HTTPS_PORT, CADDY_HTTPS_PORT)
        .public_udp_port(CADDY_HTTPS_PORT, CADDY_HTTPS_PORT)
        .port(CADDY_ADMIN_PORT, CADDY_ADMIN_PORT)
        .env(vec![format!("CADDY_ADMIN=0.0.0.0:{CADDY_ADMIN_PORT}")])
        .volume(CADDY_DATA_VOLUME, "/data")
        .volume(CADDY_CONFIG_VOLUME, "/config")
        .restart(RestartPolicy::Always)
}

pub async fn is_enabled(pool: &SqlitePool, id: ServiceId) -> Result<bool> {
    settings::get_bool(pool, &id.setting_key(), id.enabled_by_default()).await
}

pub async fn set_enabled(
    pool: &SqlitePool,
    docker: &DockerClient,
    id: ServiceId,
    enabled: bool,
) -> Result<()> {
    settings::set_bool(pool, &id.setting_key(), enabled).await?;
    converge(pool, docker, id).await
}

pub async fn converge(pool: &SqlitePool, docker: &DockerClient, id: ServiceId) -> Result<()> {
    let spec = id.spec();

    if is_enabled(pool, id).await? {
        docker
            .ensure_running(&spec)
            .await
            .with_context(|| format!("failed to start the {} service", id.as_str()))
    } else {
        docker
            .stop_and_remove(&spec.name)
            .await
            .with_context(|| format!("failed to stop the {} service", id.as_str()))
    }
}

pub async fn converge_all(pool: &SqlitePool, docker: &DockerClient) -> Result<()> {
    for id in ALL_SERVICES {
        converge(pool, docker, id).await?;
    }
    Ok(())
}

pub async fn status(
    pool: &SqlitePool,
    docker: &DockerClient,
    id: ServiceId,
) -> Result<ServiceStatus> {
    let running = docker
        .inspect(id.container_name())
        .await?
        .is_some_and(|status| status.running);

    Ok(ServiceStatus {
        id,
        container: id.container_name(),
        enabled: is_enabled(pool, id).await?,
        running,
    })
}

pub async fn status_all(pool: &SqlitePool, docker: &DockerClient) -> Result<Vec<ServiceStatus>> {
    let mut out = Vec::with_capacity(ALL_SERVICES.len());
    for id in ALL_SERVICES {
        out.push(status(pool, docker, id).await?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_service_id_round_trips() {
        for id in ALL_SERVICES {
            assert_eq!(ServiceId::parse(id.as_str()), Some(id));
        }
        assert_eq!(ServiceId::parse("nope"), None);
    }

    #[test]
    fn caddy_publishes_http_and_https_but_keeps_admin_local() {
        let spec = caddy_spec();

        let public: Vec<_> = spec
            .ports
            .iter()
            .filter(|p| p.host_ip == crate::infra::containers::spec::ALL_INTERFACES)
            .map(|p| p.container_port)
            .collect();
        assert_eq!(public, vec![80, 443, 443]);

        let admin: Vec<_> = spec
            .ports
            .iter()
            .filter(|p| p.container_port == CADDY_ADMIN_PORT)
            .collect();
        assert_eq!(admin.len(), 1);
        assert_eq!(admin[0].host_ip, crate::infra::containers::spec::LOOPBACK);
    }

    #[test]
    fn caddy_serves_https_over_quic_as_well() {
        let keys: Vec<String> = caddy_spec().ports.iter().map(|p| p.key()).collect();

        assert!(keys.contains(&"443/tcp".to_string()));
        assert!(keys.contains(&"443/udp".to_string()));
    }

    #[test]
    fn caddy_persists_certificates_across_recreates() {
        let spec = caddy_spec();

        let targets: Vec<&str> = spec.volumes.iter().map(|v| v.target.as_str()).collect();
        assert!(targets.contains(&"/data"), "caddy must keep /data");
        assert!(targets.contains(&"/config"));
    }

    #[test]
    fn caddy_exposes_its_admin_api_inside_the_container() {
        assert!(
            caddy_spec()
                .env
                .iter()
                .any(|e| e == "CADDY_ADMIN=0.0.0.0:2019")
        );
    }

    #[sqlx::test]
    async fn services_default_to_enabled(pool: sqlx::SqlitePool) {
        for id in ALL_SERVICES {
            assert!(is_enabled(&pool, id).await.unwrap(), "{}", id.as_str());
        }
    }

    #[sqlx::test]
    async fn the_toggle_persists_and_is_per_service(pool: sqlx::SqlitePool) {
        settings::set_bool(&pool, "service.reverse-proxy.enabled", false)
            .await
            .unwrap();

        assert!(!is_enabled(&pool, ServiceId::ReverseProxy).await.unwrap());
        assert!(is_enabled(&pool, ServiceId::Registry).await.unwrap());
    }
}
