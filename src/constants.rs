use std::time::Duration;

pub const PACKAGE_RESYNC_INTERVAL: Duration = Duration::from_secs(30 * 60);

pub const MAX_CAPTURED_OUTPUT_LINES: usize = 20;
pub const MAX_CAPTURED_OUTPUT_BYTES: usize = 4096;

pub const CONTAINER_NETWORK_NAME: &str = "arges";
pub const REGISTRY_CONTAINER_NAME: &str = "arges-registry";
pub const REGISTRY_IMAGE: &str = "registry:2";
pub const REGISTRY_PORT: u16 = 5000;
pub const REGISTRY_DATA_VOLUME: &str = "arges-registry-data";
pub const REGISTRY_URL: &str = "http://127.0.0.1:5000";

pub const CADDY_CONTAINER_NAME: &str = "arges-caddy";
pub const CADDY_IMAGE: &str = "caddy:2-alpine";
pub const CADDY_HTTP_PORT: u16 = 80;
pub const CADDY_HTTPS_PORT: u16 = 443;
pub const CADDY_ADMIN_PORT: u16 = 2019;
pub const CADDY_DATA_VOLUME: &str = "arges-caddy-data";
pub const CADDY_CONFIG_VOLUME: &str = "arges-caddy-config";

pub const CADDY_ADMIN_URL: &str = "http://127.0.0.1:2019";
pub const MAX_PROXY_DOMAIN_LEN: usize = 253;

pub const CADDY_READY_TIMEOUT: Duration = Duration::from_secs(15);

pub const DEPLOYMENT_LABEL: &str = "arges.deployment";
pub const DEPLOYMENT_RECONCILE_INTERVAL: Duration = Duration::from_secs(60);

pub const RETENTION_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);

pub const DEFAULT_JOURNAL_UNIT: &str = "arges.service";

pub const METRICS_SAMPLE_INTERVAL: Duration = Duration::from_secs(15);
pub const METRICS_RETENTION: Duration = Duration::from_secs(7 * 24 * 60 * 60);
pub const METRICS_DEFAULT_RANGE: Duration = Duration::from_secs(60 * 60);
pub const METRICS_MAX_POINTS: i64 = 240;
pub const METRICS_STATS_TIMEOUT: Duration = Duration::from_secs(10);
