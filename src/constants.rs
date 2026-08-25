use std::time::Duration;

pub const PACKAGE_RESYNC_INTERVAL: Duration = Duration::from_secs(30 * 60);

pub const MAX_CAPTURED_OUTPUT_LINES: usize = 20;
pub const MAX_CAPTURED_OUTPUT_BYTES: usize = 4096;

pub const CONTAINER_NETWORK_NAME: &str = "arges";
pub const REGISTRY_CONTAINER_NAME: &str = "arges-registry";
pub const REGISTRY_IMAGE: &str = "registry:2";
pub const REGISTRY_PORT: u16 = 5000;

pub const MAX_CONTAINER_LOG_BYTES: usize = 256 * 1024;
