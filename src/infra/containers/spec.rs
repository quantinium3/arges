use std::net::{IpAddr, Ipv4Addr};

use bollard::models::{
    ContainerCreateBody, HostConfig, PortBinding, RestartPolicy as BollardRestartPolicy,
    RestartPolicyNameEnum,
};
use std::collections::HashMap;

pub const LOOPBACK: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);
pub const ALL_INTERFACES: IpAddr = IpAddr::V4(Ipv4Addr::UNSPECIFIED);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestartPolicy {
    No,
    OnFailure,
    Always,
    UnlessStopped,
}

impl From<RestartPolicy> for BollardRestartPolicy {
    fn from(policy: RestartPolicy) -> Self {
        let name = match policy {
            RestartPolicy::No => RestartPolicyNameEnum::NO,
            RestartPolicy::OnFailure => RestartPolicyNameEnum::ON_FAILURE,
            RestartPolicy::Always => RestartPolicyNameEnum::ALWAYS,
            RestartPolicy::UnlessStopped => RestartPolicyNameEnum::UNLESS_STOPPED,
        };

        Self {
            name: Some(name),
            maximum_retry_count: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    Tcp,
    Udp,
}

impl Protocol {
    pub fn as_str(self) -> &'static str {
        match self {
            Protocol::Tcp => "tcp",
            Protocol::Udp => "udp",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PortMapping {
    pub container_port: u16,
    pub host_port: u16,
    pub host_ip: IpAddr,
    pub protocol: Protocol,
}

impl PortMapping {
    pub fn key(&self) -> String {
        format!("{}/{}", self.container_port, self.protocol.as_str())
    }

    fn binding(&self) -> PortBinding {
        PortBinding {
            host_ip: Some(self.host_ip.to_string()),
            host_port: Some(self.host_port.to_string()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VolumeMount {
    pub source: String,
    pub target: String,
    pub read_only: bool,
}

impl VolumeMount {
    pub fn to_bind(&self) -> String {
        if self.read_only {
            format!("{}:{}:ro", self.source, self.target)
        } else {
            format!("{}:{}", self.source, self.target)
        }
    }
}

#[derive(Debug, Clone)]
pub struct ContainerSpec {
    pub name: String,
    pub image: String,
    pub network: Option<String>,
    pub ports: Vec<PortMapping>,
    pub volumes: Vec<VolumeMount>,
    pub env: Vec<String>,
    pub labels: HashMap<String, String>,
    pub restart: RestartPolicy,
    pub memory_limit_mb: Option<i64>,
    pub cpu_shares: Option<i64>,
}

impl ContainerSpec {
    pub fn new(name: impl Into<String>, image: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            image: image.into(),
            network: None,
            ports: Vec::new(),
            volumes: Vec::new(),
            env: Vec::new(),
            labels: HashMap::new(),
            restart: RestartPolicy::No,
            memory_limit_mb: None,
            cpu_shares: None,
        }
    }

    pub fn network(mut self, network: impl Into<String>) -> Self {
        self.network = Some(network.into());
        self
    }

    pub fn port(mut self, container_port: u16, host_port: u16) -> Self {
        self.ports.push(PortMapping {
            container_port,
            host_port,
            host_ip: LOOPBACK,
            protocol: Protocol::Tcp,
        });
        self
    }

    pub fn public_port(mut self, container_port: u16, host_port: u16) -> Self {
        self.ports.push(PortMapping {
            container_port,
            host_port,
            host_ip: ALL_INTERFACES,
            protocol: Protocol::Tcp,
        });
        self
    }

    pub fn public_udp_port(mut self, container_port: u16, host_port: u16) -> Self {
        self.ports.push(PortMapping {
            container_port,
            host_port,
            host_ip: ALL_INTERFACES,
            protocol: Protocol::Udp,
        });
        self
    }

    pub fn volume(mut self, source: impl Into<String>, target: impl Into<String>) -> Self {
        self.volumes.push(VolumeMount {
            source: source.into(),
            target: target.into(),
            read_only: false,
        });
        self
    }

    pub fn env(mut self, env: Vec<String>) -> Self {
        self.env = env;
        self
    }

    pub fn restart(mut self, restart: RestartPolicy) -> Self {
        self.restart = restart;
        self
    }

    pub fn label(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.labels.insert(key.into(), value.into());
        self
    }

    pub fn limits(mut self, memory_limit_mb: Option<i64>, cpu_shares: Option<i64>) -> Self {
        self.memory_limit_mb = memory_limit_mb;
        self.cpu_shares = cpu_shares;
        self
    }

    pub fn volume_ro(mut self, source: impl Into<String>, target: impl Into<String>) -> Self {
        self.volumes.push(VolumeMount {
            source: source.into(),
            target: target.into(),
            read_only: true,
        });
        self
    }

    pub fn published_keys(&self) -> Vec<String> {
        let mut keys: Vec<String> = self.ports.iter().map(PortMapping::key).collect();
        keys.sort();
        keys.dedup();
        keys
    }

    pub fn bind_specs(&self) -> Vec<String> {
        let mut binds: Vec<String> = self.volumes.iter().map(VolumeMount::to_bind).collect();
        binds.sort();
        binds
    }

    pub fn to_create_body(&self) -> ContainerCreateBody {
        let mut port_bindings = HashMap::new();
        let mut exposed_ports = Vec::new();

        for mapping in &self.ports {
            port_bindings
                .entry(mapping.key())
                .or_insert_with(|| Some(Vec::new()))
                .get_or_insert_with(Vec::new)
                .push(mapping.binding());
            exposed_ports.push(mapping.key());
        }

        ContainerCreateBody {
            image: Some(self.image.clone()),
            env: Some(self.env.clone()),
            labels: (!self.labels.is_empty()).then(|| self.labels.clone()),
            exposed_ports: (!exposed_ports.is_empty()).then_some(exposed_ports),
            host_config: Some(HostConfig {
                network_mode: self.network.clone(),
                port_bindings: (!port_bindings.is_empty()).then_some(port_bindings),
                binds: (!self.volumes.is_empty())
                    .then(|| self.volumes.iter().map(VolumeMount::to_bind).collect()),
                restart_policy: Some(self.restart.into()),
                memory: self.memory_limit_mb.map(|mb| mb * 1024 * 1024),
                cpu_shares: self.cpu_shares,
                ..Default::default()
            }),
            ..Default::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bindings(body: &ContainerCreateBody, key: &str) -> Vec<PortBinding> {
        body.host_config
            .as_ref()
            .unwrap()
            .port_bindings
            .as_ref()
            .unwrap()
            .get(key)
            .unwrap()
            .clone()
            .unwrap()
    }

    #[test]
    fn a_published_port_defaults_to_loopback() {
        let body = ContainerSpec::new("app", "nginx")
            .port(80, 8080)
            .to_create_body();

        let binding = &bindings(&body, "80/tcp")[0];
        assert_eq!(binding.host_ip.as_deref(), Some("127.0.0.1"));
        assert_eq!(binding.host_port.as_deref(), Some("8080"));
    }

    #[test]
    fn exposing_publicly_is_explicit() {
        let body = ContainerSpec::new("app", "nginx")
            .public_port(80, 8080)
            .to_create_body();

        assert_eq!(
            bindings(&body, "80/tcp")[0].host_ip.as_deref(),
            Some("0.0.0.0")
        );
    }

    #[test]
    fn every_published_port_is_also_exposed() {
        let body = ContainerSpec::new("app", "nginx")
            .port(80, 8080)
            .port(443, 8443)
            .to_create_body();

        let mut exposed = body.exposed_ports.clone().unwrap();
        exposed.sort();
        assert_eq!(exposed, vec!["443/tcp".to_string(), "80/tcp".to_string()]);
    }

    #[test]
    fn a_spec_without_ports_sets_neither_map() {
        let body = ContainerSpec::new("app", "nginx").to_create_body();

        assert!(body.exposed_ports.is_none());
        assert!(body.host_config.as_ref().unwrap().port_bindings.is_none());
    }

    #[test]
    fn network_and_restart_reach_the_host_config() {
        let body = ContainerSpec::new("app", "nginx")
            .network("arges")
            .restart(RestartPolicy::Always)
            .to_create_body();

        let host = body.host_config.as_ref().unwrap();
        assert_eq!(host.network_mode.as_deref(), Some("arges"));
        assert_eq!(
            host.restart_policy.as_ref().unwrap().name,
            Some(RestartPolicyNameEnum::ALWAYS)
        );
    }

    #[test]
    fn the_registry_is_only_reachable_from_the_host() {
        let body = crate::infra::containers::services::registry_spec().to_create_body();

        let binding = &bindings(&body, "5000/tcp")[0];
        assert_eq!(binding.host_ip.as_deref(), Some("127.0.0.1"));
    }
}
