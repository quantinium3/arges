use std::time::Duration;

pub const PACKAGE_RESYNC_INTERVAL: Duration = Duration::from_secs(30 * 60);

pub const MAX_CAPTURED_OUTPUT_LINES: usize = 20;
pub const MAX_CAPTURED_OUTPUT_BYTES: usize = 4096;
