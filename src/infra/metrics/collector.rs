use std::{
    collections::{HashMap, HashSet},
    time::Instant,
};

use sysinfo::{CpuRefreshKind, Disks, MemoryRefreshKind, Networks, RefreshKind, System};

use crate::db::queries::metrics::{DeploymentSample, FilesystemSample, HostSample};

const SKIPPED_INTERFACES: [&str; 2] = ["lo", "veth"];

pub struct ContainerStats {
    pub deployment_id: String,
    pub cpu_percent: f64,
    pub memory_used: i64,
    pub memory_limit: Option<i64>,
    pub network_rx_total: u64,
    pub network_tx_total: u64,
}

pub struct Collector {
    system: System,
    networks: Networks,
    taken_at: Instant,
    container_totals: HashMap<String, (u64, u64)>,
}

fn counts_towards_host(name: &str) -> bool {
    !SKIPPED_INTERFACES
        .iter()
        .any(|skipped| name == *skipped || name.starts_with(skipped))
}

impl Collector {
    pub fn new() -> Self {
        let system = System::new_with_specifics(
            RefreshKind::nothing()
                .with_cpu(CpuRefreshKind::nothing().with_cpu_usage())
                .with_memory(MemoryRefreshKind::everything()),
        );

        Self {
            system,
            networks: Networks::new_with_refreshed_list(),
            taken_at: Instant::now(),
            container_totals: HashMap::new(),
        }
    }

    pub fn host(&mut self, ts: i64) -> HostSample {
        self.system.refresh_cpu_usage();
        self.system.refresh_memory();
        self.networks.refresh(true);

        let elapsed = self.taken_at.elapsed().as_secs().max(1) as i64;
        self.taken_at = Instant::now();

        let (rx, tx) = self
            .networks
            .list()
            .iter()
            .filter(|(name, _)| counts_towards_host(name))
            .fold((0u64, 0u64), |(rx, tx), (_, data)| {
                (rx + data.received(), tx + data.transmitted())
            });

        let load = System::load_average();

        HostSample {
            ts,
            window_seconds: elapsed,
            cpu_percent: f64::from(self.system.global_cpu_usage()).clamp(0.0, 100.0),
            memory_used: self.system.used_memory() as i64,
            memory_total: self.system.total_memory() as i64,
            swap_used: self.system.used_swap() as i64,
            swap_total: self.system.total_swap() as i64,
            load_one: load.one.max(0.0),
            load_five: load.five.max(0.0),
            load_fifteen: load.fifteen.max(0.0),
            network_rx_bytes: rx as i64,
            network_tx_bytes: tx as i64,
        }
    }

    pub fn filesystems(&self, ts: i64) -> Vec<FilesystemSample> {
        let mut seen = HashSet::new();
        let mut samples = Vec::new();

        for disk in Disks::new_with_refreshed_list().list() {
            let mount_point = disk.mount_point().to_string_lossy().into_owned();

            if disk.total_space() == 0 || !seen.insert(mount_point.clone()) {
                continue;
            }

            samples.push(FilesystemSample {
                ts,
                mount_point,
                total_bytes: disk.total_space() as i64,
                available_bytes: disk.available_space().min(disk.total_space()) as i64,
            });
        }

        samples.sort_by(|a, b| a.mount_point.cmp(&b.mount_point));
        samples
    }

    pub fn deployments(
        &mut self,
        ts: i64,
        window_seconds: i64,
        stats: Vec<ContainerStats>,
    ) -> Vec<DeploymentSample> {
        let mut totals: HashMap<String, (u64, u64)> = HashMap::new();
        let mut merged: HashMap<String, DeploymentSample> = HashMap::new();

        for container in stats {
            let entry = totals
                .entry(container.deployment_id.clone())
                .or_insert((0, 0));
            entry.0 += container.network_rx_total;
            entry.1 += container.network_tx_total;

            merged
                .entry(container.deployment_id.clone())
                .and_modify(|sample| {
                    sample.cpu_percent += container.cpu_percent;
                    sample.memory_used += container.memory_used;
                    sample.memory_limit = sample.memory_limit.max(container.memory_limit);
                })
                .or_insert(DeploymentSample {
                    ts,
                    deployment_id: container.deployment_id,
                    window_seconds,
                    cpu_percent: container.cpu_percent,
                    memory_used: container.memory_used,
                    memory_limit: container.memory_limit,
                    network_rx_bytes: 0,
                    network_tx_bytes: 0,
                });
        }

        let mut samples = Vec::with_capacity(merged.len());

        for (deployment_id, mut sample) in merged {
            let (rx, tx) = totals.get(&deployment_id).copied().unwrap_or((0, 0));
            let (last_rx, last_tx) = self
                .container_totals
                .insert(deployment_id.clone(), (rx, tx))
                .unwrap_or((rx, tx));

            sample.network_rx_bytes = rx.saturating_sub(last_rx) as i64;
            sample.network_tx_bytes = tx.saturating_sub(last_tx) as i64;
            samples.push(sample);
        }

        self.container_totals
            .retain(|deployment_id, _| totals.contains_key(deployment_id));

        samples.sort_by(|a, b| a.deployment_id.cmp(&b.deployment_id));
        samples
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stats(deployment_id: &str, cpu: f64, memory: i64, rx: u64, tx: u64) -> ContainerStats {
        ContainerStats {
            deployment_id: deployment_id.to_string(),
            cpu_percent: cpu,
            memory_used: memory,
            memory_limit: Some(512),
            network_rx_total: rx,
            network_tx_total: tx,
        }
    }

    #[test]
    fn loopback_and_container_interfaces_are_left_out_of_host_traffic() {
        assert!(counts_towards_host("eth0"));
        assert!(counts_towards_host("enp3s0"));
        assert!(!counts_towards_host("lo"));
        assert!(!counts_towards_host("veth1a2b3c"));
    }

    #[test]
    fn the_first_sample_of_a_container_reports_no_traffic() {
        let mut collector = Collector::new();

        let samples = collector.deployments(10, 15, vec![stats("one", 4.0, 100, 5_000, 900)]);

        assert_eq!(samples.len(), 1);
        assert_eq!(samples[0].network_rx_bytes, 0);
        assert_eq!(samples[0].network_tx_bytes, 0);
    }

    #[test]
    fn later_samples_report_the_traffic_since_the_previous_one() {
        let mut collector = Collector::new();

        collector.deployments(10, 15, vec![stats("one", 4.0, 100, 5_000, 900)]);
        let samples = collector.deployments(25, 15, vec![stats("one", 6.0, 120, 8_000, 1_400)]);

        assert_eq!(samples[0].network_rx_bytes, 3_000);
        assert_eq!(samples[0].network_tx_bytes, 500);
        assert_eq!(samples[0].cpu_percent, 6.0);
    }

    #[test]
    fn a_replaced_container_never_reports_negative_traffic() {
        let mut collector = Collector::new();

        collector.deployments(10, 15, vec![stats("one", 4.0, 100, 9_000, 9_000)]);
        let samples = collector.deployments(25, 15, vec![stats("one", 4.0, 100, 40, 10)]);

        assert_eq!(samples[0].network_rx_bytes, 0);
        assert_eq!(samples[0].network_tx_bytes, 0);
    }

    #[test]
    fn the_containers_of_one_deployment_are_added_together() {
        let mut collector = Collector::new();

        let samples = collector.deployments(
            10,
            15,
            vec![stats("one", 4.0, 100, 0, 0), stats("one", 2.5, 250, 0, 0)],
        );

        assert_eq!(samples.len(), 1);
        assert_eq!(samples[0].cpu_percent, 6.5);
        assert_eq!(samples[0].memory_used, 350);
    }

    #[test]
    fn a_deployment_that_stopped_is_forgotten() {
        let mut collector = Collector::new();

        collector.deployments(10, 15, vec![stats("one", 4.0, 100, 5_000, 900)]);
        collector.deployments(25, 15, vec![stats("two", 4.0, 100, 100, 100)]);

        assert!(!collector.container_totals.contains_key("one"));
        assert!(collector.container_totals.contains_key("two"));
    }
}
