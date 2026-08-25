use std::time::Duration;

pub const PACKAGE_RESYNC_INTERVAL: Duration = Duration::from_secs(30 * 60);

pub const MAX_CAPTURED_OUTPUT_LINES: usize = 20;
pub const MAX_CAPTURED_OUTPUT_BYTES: usize = 4096;

pub const CONTAINER_NETWORK_NAME: &str = "arges";
pub const REGISTRY_CONTAINER_NAME: &str = "arges-registry";
pub const REGISTRY_IMAGE: &str = "registry:2";
pub const REGISTRY_PORT: u16 = 5000;

pub const MAX_CONTAINER_LOG_BYTES: usize = 256 * 1024;

pub const CADDY_CONTAINER_NAME: &str = "arges-caddy";
pub const CADDY_IMAGE: &str = "caddy:2-alpine";
pub const CADDY_HTTP_PORT: u16 = 80;
pub const CADDY_HTTPS_PORT: u16 = 443;
pub const CADDY_ADMIN_PORT: u16 = 2019;
pub const CADDY_DATA_VOLUME: &str = "arges-caddy-data";
pub const CADDY_CONFIG_VOLUME: &str = "arges-caddy-config";
