use std::{
    collections::HashMap,
    io::ErrorKind,
    net::{SocketAddr, TcpListener, UdpSocket},
};

use anyhow::{Context, Result, bail};
use bollard::{
    Docker,
    errors::Error,
    exec::{CreateExecOptions, StartExecOptions, StartExecResults},
    models::ContainerStatsResponse,
    models::HealthStatusEnum,
    models::NetworkCreateRequest,
    query_parameters::{
        CreateContainerOptionsBuilder, CreateImageOptionsBuilder, InspectContainerOptionsBuilder,
        ListContainersOptionsBuilder, ListNetworksOptionsBuilder, LogsOptionsBuilder,
        RemoveContainerOptionsBuilder, StatsOptionsBuilder, StopContainerOptionsBuilder,
    },
};
use futures_util::TryStreamExt;

use crate::infra::containers::spec::{ContainerSpec, Protocol};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageHealth {
    None,
    Starting,
    Healthy,
    Unhealthy,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContainerStatus {
    pub running: bool,
    pub exit_code: Option<i64>,
    pub published: Vec<String>,
    pub binds: Vec<String>,
    pub env: Vec<String>,
    pub ip: Option<String>,
    pub health: ImageHealth,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ContainerUsage {
    pub cpu_percent: f64,
    pub memory_used: u64,
    pub memory_limit: Option<u64>,
    pub network_rx_total: u64,
    pub network_tx_total: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LabelledContainer {
    pub name: String,
    pub value: String,
}

#[derive(Clone)]
pub struct DockerClient(Docker);

fn cpu_percent(stats: &ContainerStatsResponse) -> f64 {
    let (Some(current), Some(previous)) = (&stats.cpu_stats, &stats.precpu_stats) else {
        return 0.0;
    };

    let used = current
        .cpu_usage
        .as_ref()
        .and_then(|usage| usage.total_usage)
        .unwrap_or(0);
    let used_before = previous
        .cpu_usage
        .as_ref()
        .and_then(|usage| usage.total_usage)
        .unwrap_or(0);

    let system = current.system_cpu_usage.unwrap_or(0);
    let system_before = previous.system_cpu_usage.unwrap_or(0);

    if system_before == 0 {
        return 0.0;
    }

    let used_delta = used.saturating_sub(used_before) as f64;
    let system_delta = system.saturating_sub(system_before) as f64;

    if used_delta <= 0.0 || system_delta <= 0.0 {
        return 0.0;
    }

    let cpus = current
        .online_cpus
        .filter(|count| *count > 0)
        .map(u64::from)
        .or_else(|| {
            current
                .cpu_usage
                .as_ref()
                .and_then(|usage| usage.percpu_usage.as_ref())
                .map(|per_cpu| per_cpu.len() as u64)
        })
        .unwrap_or(1)
        .max(1) as f64;

    (used_delta / system_delta) * cpus * 100.0
}

fn memory_used(stats: &ContainerStatsResponse) -> u64 {
    let Some(memory) = &stats.memory_stats else {
        return 0;
    };

    let usage = memory.usage.unwrap_or(0);
    let cache = memory
        .stats
        .as_ref()
        .and_then(|stats| stats.get("inactive_file").copied())
        .unwrap_or(0);

    usage.saturating_sub(cache)
}

fn network_totals(stats: &ContainerStatsResponse) -> (u64, u64) {
    stats
        .networks
        .as_ref()
        .map(|networks| {
            networks.values().fold((0, 0), |(rx, tx), interface| {
                (
                    rx + interface.rx_bytes.unwrap_or(0),
                    tx + interface.tx_bytes.unwrap_or(0),
                )
            })
        })
        .unwrap_or((0, 0))
}

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

fn tagged(image: &str) -> String {
    if image.contains('@') {
        return image.to_string();
    }

    let name = image.rsplit('/').next().unwrap_or(image);

    if name.contains(':') {
        image.to_string()
    } else {
        format!("{image}:latest")
    }
}

fn spec_matches(spec: &ContainerSpec, status: &ContainerStatus) -> bool {
    if status.published != spec.published_keys() {
        return false;
    }

    if status.binds != spec.bind_specs() {
        return false;
    }

    spec.env.iter().all(|wanted| status.env.contains(wanted))
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
        let reference = tagged(image);
        let options = CreateImageOptionsBuilder::default()
            .from_image(&reference)
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

                let network_settings = response.network_settings.unwrap_or_default();

                let mut published: Vec<String> = network_settings
                    .ports
                    .clone()
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|(_, bindings)| bindings.as_ref().is_some_and(|b| !b.is_empty()))
                    .map(|(key, _)| key)
                    .collect();
                published.sort();
                published.dedup();

                let host_config = response.host_config.unwrap_or_default();
                let mut binds = host_config.binds.unwrap_or_default();
                binds.sort();

                let env = response.config.unwrap_or_default().env.unwrap_or_default();

                let health = match state.health.as_ref().and_then(|h| h.status) {
                    Some(HealthStatusEnum::STARTING) => ImageHealth::Starting,
                    Some(HealthStatusEnum::HEALTHY) => ImageHealth::Healthy,
                    Some(HealthStatusEnum::UNHEALTHY) => ImageHealth::Unhealthy,
                    _ => ImageHealth::None,
                };

                let ip = network_settings
                    .networks
                    .unwrap_or_default()
                    .into_values()
                    .find_map(|network| network.ip_address.filter(|ip| !ip.is_empty()));

                Ok(Some(ContainerStatus {
                    running: state.running.unwrap_or(false),
                    exit_code: state.exit_code,
                    published,
                    binds,
                    env,
                    ip,
                    health,
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

    pub async fn list_by_label(&self, label: &str, value: &str) -> Result<Vec<String>> {
        let mut filters = HashMap::new();
        let selector = format!("{label}={value}");
        filters.insert("label", vec![selector.as_str()]);

        let options = ListContainersOptionsBuilder::default()
            .all(true)
            .filters(&filters)
            .build();

        let containers = self
            .0
            .list_containers(Some(options))
            .await
            .with_context(|| format!("failed to list containers labelled {label}={value}"))?;

        Ok(containers
            .into_iter()
            .filter_map(|c| c.names)
            .flatten()
            .map(|name| name.trim_start_matches('/').to_string())
            .collect())
    }

    pub async fn running_by_label(&self, label: &str) -> Result<Vec<LabelledContainer>> {
        let mut filters = HashMap::new();
        filters.insert("label", vec![label]);
        filters.insert("status", vec!["running"]);

        let options = ListContainersOptionsBuilder::default()
            .all(false)
            .filters(&filters)
            .build();

        let containers = self
            .0
            .list_containers(Some(options))
            .await
            .with_context(|| format!("failed to list running containers labelled {label}"))?;

        Ok(containers
            .into_iter()
            .filter_map(|container| {
                let name = container
                    .names
                    .as_ref()
                    .and_then(|names| names.first())
                    .map(|name| name.trim_start_matches('/').to_string())?;
                let value = container.labels.as_ref()?.get(label)?.clone();

                Some(LabelledContainer { name, value })
            })
            .collect())
    }

    pub async fn usage(&self, container: &str) -> Result<Option<ContainerUsage>> {
        let options = StatsOptionsBuilder::default()
            .stream(false)
            .one_shot(false)
            .build();

        let stats = match futures_util::StreamExt::next(&mut Box::pin(
            self.0.stats(container, Some(options)),
        ))
        .await
        {
            Some(Ok(stats)) => stats,
            Some(Err(e)) if is_not_found(&e) => return Ok(None),
            Some(Err(e)) => {
                return Err(e).with_context(|| format!("failed to read stats for {container}"));
            }
            None => return Ok(None),
        };

        let (network_rx_total, network_tx_total) = network_totals(&stats);

        Ok(Some(ContainerUsage {
            cpu_percent: cpu_percent(&stats).max(0.0),
            memory_used: memory_used(&stats),
            memory_limit: stats
                .memory_stats
                .as_ref()
                .and_then(|memory| memory.limit)
                .filter(|limit| *limit > 0),
            network_rx_total,
            network_tx_total,
        }))
    }

    pub async fn exec(&self, container: &str, command: &[&str]) -> Result<String> {
        let config = CreateExecOptions {
            cmd: Some(command.iter().map(|c| c.to_string()).collect()),
            attach_stdout: Some(true),
            attach_stderr: Some(true),
            ..Default::default()
        };

        let created = self
            .0
            .create_exec(container, config)
            .await
            .with_context(|| format!("failed to prepare a command in {container}"))?;

        let started = self
            .0
            .start_exec(&created.id, None::<StartExecOptions>)
            .await
            .with_context(|| format!("failed to run a command in {container}"))?;

        let mut output = String::new();

        if let StartExecResults::Attached {
            output: mut stream, ..
        } = started
        {
            while let Some(chunk) = stream.try_next().await? {
                output.push_str(&String::from_utf8_lossy(&chunk.into_bytes()));
            }
        }

        let inspected = self
            .0
            .inspect_exec(&created.id)
            .await
            .with_context(|| format!("failed to inspect a command in {container}"))?;

        match inspected.exit_code {
            Some(0) | None => Ok(output),
            Some(code) => bail!(
                "command in {container} exited with {code}: {}",
                output.trim()
            ),
        }
    }

    pub async fn create_raw(
        &self,
        name: &str,
        body: bollard::models::ContainerCreateBody,
    ) -> Result<()> {
        let options = CreateContainerOptionsBuilder::default().name(name).build();

        self.0
            .create_container(Some(options), body)
            .await
            .with_context(|| format!("failed to create container {name}"))?;

        self.start(name).await
    }

    pub async fn start(&self, name: &str) -> Result<()> {
        self.0
            .start_container(name, None)
            .await
            .with_context(|| format!("failed to start container {name}"))?;
        Ok(())
    }

    pub async fn ensure_running(&self, spec: &ContainerSpec) -> Result<()> {
        if let Some(status) = self.inspect(&spec.name).await? {
            if status.running && spec_matches(spec, &status) {
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

    pub fn follow_logs(
        &self,
        container: &str,
        tail: i64,
    ) -> impl futures_util::Stream<Item = Result<String>> + use<> {
        let options = LogsOptionsBuilder::default()
            .stdout(true)
            .stderr(true)
            .follow(true)
            .timestamps(false)
            .tail(&tail.to_string())
            .build();

        let stream = self.0.logs(container, Some(options));

        futures_util::StreamExt::map(stream, |chunk| {
            chunk
                .map(|output| String::from_utf8_lossy(&output.into_bytes()).into_owned())
                .context("the container log stream ended unexpectedly")
        })
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
mod stats_tests {
    use super::*;
    use bollard::models::{
        ContainerCpuStats, ContainerCpuUsage, ContainerMemoryStats, ContainerNetworkStats,
    };
    use std::collections::HashMap;

    fn cpu(total: u64, system: u64, cpus: u32) -> ContainerCpuStats {
        ContainerCpuStats {
            cpu_usage: Some(ContainerCpuUsage {
                total_usage: Some(total),
                ..Default::default()
            }),
            system_cpu_usage: Some(system),
            online_cpus: Some(cpus),
            ..Default::default()
        }
    }

    fn stats(current: ContainerCpuStats, previous: ContainerCpuStats) -> ContainerStatsResponse {
        ContainerStatsResponse {
            cpu_stats: Some(current),
            precpu_stats: Some(previous),
            ..Default::default()
        }
    }

    #[test]
    fn a_container_saturating_one_of_four_cores_reports_a_full_core() {
        let sample = stats(cpu(100, 1_400, 4), cpu(0, 1_000, 4));

        assert_eq!(cpu_percent(&sample), 100.0);
    }

    #[test]
    fn a_container_saturating_every_core_reports_all_of_them() {
        let sample = stats(cpu(400, 1_400, 4), cpu(0, 1_000, 4));

        assert_eq!(cpu_percent(&sample), 400.0);
    }

    #[test]
    fn an_idle_container_reports_nothing() {
        let sample = stats(cpu(100, 1_400, 4), cpu(100, 1_000, 4));

        assert_eq!(cpu_percent(&sample), 0.0);
    }

    #[test]
    fn a_first_sample_without_a_previous_one_reports_nothing() {
        let sample = stats(cpu(100, 1_400, 4), cpu(0, 0, 0));

        assert_eq!(cpu_percent(&ContainerStatsResponse::default()), 0.0);
        assert_eq!(cpu_percent(&sample), 0.0);
    }

    #[test]
    fn counters_that_went_backwards_report_nothing() {
        let sample = stats(cpu(50, 1_200, 4), cpu(100, 1_400, 4));

        assert_eq!(cpu_percent(&sample), 0.0);
    }

    #[test]
    fn page_cache_is_left_out_of_the_memory_reading() {
        let mut breakdown = HashMap::new();
        breakdown.insert("inactive_file".to_string(), 400u64);

        let sample = ContainerStatsResponse {
            memory_stats: Some(ContainerMemoryStats {
                usage: Some(1_000),
                stats: Some(breakdown),
                limit: Some(4_096),
                ..Default::default()
            }),
            ..Default::default()
        };

        assert_eq!(memory_used(&sample), 600);
    }

    #[test]
    fn memory_without_a_breakdown_is_taken_as_reported() {
        let sample = ContainerStatsResponse {
            memory_stats: Some(ContainerMemoryStats {
                usage: Some(1_000),
                ..Default::default()
            }),
            ..Default::default()
        };

        assert_eq!(memory_used(&sample), 1_000);
        assert_eq!(memory_used(&ContainerStatsResponse::default()), 0);
    }

    #[test]
    fn traffic_is_added_up_across_every_interface() {
        let mut networks = HashMap::new();
        networks.insert(
            "eth0".to_string(),
            ContainerNetworkStats {
                rx_bytes: Some(100),
                tx_bytes: Some(50),
                ..Default::default()
            },
        );
        networks.insert(
            "eth1".to_string(),
            ContainerNetworkStats {
                rx_bytes: Some(400),
                tx_bytes: Some(150),
                ..Default::default()
            },
        );

        let sample = ContainerStatsResponse {
            networks: Some(networks),
            ..Default::default()
        };

        assert_eq!(network_totals(&sample), (500, 200));
        assert_eq!(network_totals(&ContainerStatsResponse::default()), (0, 0));
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

    fn status_for(spec: &ContainerSpec) -> ContainerStatus {
        ContainerStatus {
            running: true,
            exit_code: None,
            published: spec.published_keys(),
            binds: spec.bind_specs(),
            env: spec.env.clone(),
            ip: None,
            health: ImageHealth::None,
        }
    }

    #[tokio::test]
    async fn logs_stream_live_lines_as_they_are_written() {
        if std::env::var("ARGES_DOCKER_LAB").is_err() {
            return;
        }

        use futures_util::StreamExt;
        use std::time::{Duration, Instant};

        let docker = DockerClient::connect().await.unwrap();
        let _ = docker.stop_and_remove("log-lab").await;

        docker.pull_image("alpine:3").await.unwrap();
        docker
            .create_raw(
                "log-lab",
                bollard::models::ContainerCreateBody {
                    image: Some("alpine:3".to_string()),
                    cmd: Some(vec![
                        "sh".to_string(),
                        "-c".to_string(),
                        "echo backlog-1; echo backlog-2; for i in 1 2 3; do sleep 1; echo live-$i; done"
                            .to_string(),
                    ]),
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        tokio::time::sleep(Duration::from_millis(600)).await;

        let started = Instant::now();
        let mut stream = Box::pin(docker.follow_logs("log-lab", 100));
        let mut seen: Vec<(String, u128)> = Vec::new();

        while let Ok(Some(chunk)) =
            tokio::time::timeout(Duration::from_secs(6), stream.next()).await
        {
            let chunk = chunk.unwrap();
            for line in chunk.lines().filter(|l| !l.trim().is_empty()) {
                seen.push((line.trim().to_string(), started.elapsed().as_millis()));
            }
            if seen.iter().any(|(l, _)| l == "live-3") {
                break;
            }
        }

        let names: Vec<&str> = seen.iter().map(|(l, _)| l.as_str()).collect();
        assert_eq!(
            names,
            vec!["backlog-1", "backlog-2", "live-1", "live-2", "live-3"],
            "tail must replay the backlog then keep following"
        );

        let backlog_at = seen[1].1;
        let last_at = seen[4].1;
        assert!(
            backlog_at < 500,
            "the backlog must arrive immediately, took {backlog_at}ms"
        );
        assert!(
            last_at > 1500,
            "live lines must arrive as written, not buffered to the end (last at {last_at}ms)"
        );

        docker.stop_and_remove("log-lab").await.unwrap();
    }

    #[test]
    fn an_untagged_image_is_pinned_to_latest() {
        assert_eq!(tagged("nginx"), "nginx:latest");
        assert_eq!(tagged("traefik/whoami"), "traefik/whoami:latest");
    }

    #[test]
    fn an_explicit_tag_is_left_alone() {
        assert_eq!(tagged("nginx:alpine"), "nginx:alpine");
        assert_eq!(tagged("caddy:2-alpine"), "caddy:2-alpine");
    }

    #[test]
    fn a_registry_port_is_not_mistaken_for_a_tag() {
        assert_eq!(tagged("localhost:5000/app"), "localhost:5000/app:latest");
        assert_eq!(tagged("localhost:5000/app:v1"), "localhost:5000/app:v1");
    }

    #[test]
    fn a_digest_reference_is_left_alone() {
        let by_digest = "localhost:5000/app@sha256:abc123";
        assert_eq!(tagged(by_digest), by_digest);
    }

    #[test]
    fn a_container_matching_its_spec_is_left_alone() {
        let spec = ContainerSpec::new("app", "nginx")
            .port(80, 8080)
            .volume("data", "/data")
            .env(vec!["A=1".to_string()]);

        assert!(spec_matches(&spec, &status_for(&spec)));
    }

    #[test]
    fn a_missing_volume_counts_as_drift() {
        let spec = ContainerSpec::new("app", "nginx").volume("data", "/data");
        let mut status = status_for(&spec);
        status.binds.clear();

        assert!(
            !spec_matches(&spec, &status),
            "a container running without its volume must be recreated"
        );
    }

    #[test]
    fn a_missing_env_var_counts_as_drift() {
        let spec = ContainerSpec::new("app", "nginx").env(vec!["WANTED=1".to_string()]);
        let mut status = status_for(&spec);
        status.env = vec!["OTHER=2".to_string()];

        assert!(!spec_matches(&spec, &status));
    }

    #[test]
    fn extra_env_from_the_image_is_not_drift() {
        let spec = ContainerSpec::new("app", "nginx").env(vec!["WANTED=1".to_string()]);
        let mut status = status_for(&spec);
        status.env.push("PATH=/usr/bin".to_string());

        assert!(
            spec_matches(&spec, &status),
            "images set their own env; only the spec's vars must be present"
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
