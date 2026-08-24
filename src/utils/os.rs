use anyhow::{Context, Result};
use tokio::process::Command;

pub async fn exit_ok(bin: &str, args: &[&str]) -> Result<bool> {
    let status = Command::new(bin)
        .args(args)
        .status()
        .await
        .with_context(|| format!("failed to spawn {bin}"))?;
    Ok(status.success())
}
