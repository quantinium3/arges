use anyhow::{Context, Result, bail};
use tokio::process::Command;
use tracing::{debug, error};

use crate::{
    constants::{MAX_CAPTURED_OUTPUT_BYTES, MAX_CAPTURED_OUTPUT_LINES},
    infra::{
        os::os_release::{OSFamily, OSRelease},
        packages::rpm_frontend::RPMFrontends,
    },
    utils::os::exit_ok,
};

#[derive(Debug, PartialEq, Clone, Copy)]
pub enum PackageManager {
    APT,
    RPM(RPMFrontends),
}

impl PackageManager {
    pub async fn detect() -> Result<Self> {
        let os = OSRelease::load().await?;
        match os.family() {
            OSFamily::DEB => Ok(PackageManager::APT),
            OSFamily::RPM => {
                let frontend = RPMFrontends::detect()
                    .context("rpm based host but no dnf/yum/microdnf frontend found")?;
                Ok(PackageManager::RPM(frontend))
            }
            OSFamily::UNKNOWN => bail!(
                "could not determine package manager (ID={:?}, ID_LIKE={:?}, VERSION_ID={:?})",
                os.id,
                os.id_like,
                os.version_id
            ),
        }
    }
    pub fn id(self) -> &'static str {
        match self {
            PackageManager::APT => "apt",
            PackageManager::RPM(_) => "rpm",
        }
    }

    fn mutate_command(&self, op: &str, name: &str) -> Command {
        match self {
            PackageManager::APT => {
                let mut c = Command::new("apt-get");
                c.args([op, "-y", name])
                    .env("DEBIAN_FRONTEND", "noninteractive");
                c
            }
            PackageManager::RPM(f) => {
                let mut c = Command::new(f.binary());
                c.args([op, "-y", name]);
                c
            }
        }
    }

    async fn mutate(&self, op: &str, label: &str, name: &str) -> Result<()> {
        let out = self
            .mutate_command(op, name)
            .output()
            .await
            .with_context(|| format!("failed to spawn {label} of {name}"))?;

        let output = captured_output(&out.stdout, &out.stderr);

        if out.status.success() {
            debug!(package = name, op, output = %output, "package mutation succeeded");
            return Ok(());
        }

        error!(package = name, op, output = %output, "package mutation failed");

        let status = out.status;
        if output.is_empty() {
            bail!("{label} of {name} failed ({status})")
        }
        bail!("{label} of {name} failed ({status}): {output}")
    }

    pub async fn install(&self, name: &str) -> Result<()> {
        self.mutate("install", "install", name).await
    }

    pub async fn remove(&self, name: &str) -> Result<()> {
        self.mutate("remove", "removal", name).await
    }

    pub async fn is_installed(&self, name: &str) -> Result<bool> {
        match self {
            PackageManager::APT => {
                let out = Command::new("dpkg-query")
                    .args(["-W", "-f=${Status}", name])
                    .output()
                    .await
                    .context("Failed to spawn dpkg-query")?;
                Ok(out.status.success()
                    && String::from_utf8_lossy(&out.stdout).contains("install ok installed"))
            }
            PackageManager::RPM(_) => exit_ok("rpm", &["-q", name]).await,
        }
    }
}

fn captured_output(stdout: &[u8], stderr: &[u8]) -> String {
    let err = String::from_utf8_lossy(stderr);
    let text = if err.trim().is_empty() {
        String::from_utf8_lossy(stdout).into_owned()
    } else {
        err.into_owned()
    };
    truncate_output(text.trim())
}

fn truncate_output(text: &str) -> String {
    if text.is_empty() {
        return String::new();
    }

    let lines: Vec<&str> = text.lines().collect();
    let kept = lines
        .len()
        .saturating_sub(MAX_CAPTURED_OUTPUT_LINES)
        .min(lines.len());
    let mut out = lines[kept..].join("\n");

    while out.len() > MAX_CAPTURED_OUTPUT_BYTES {
        let cut = out.len() - MAX_CAPTURED_OUTPUT_BYTES;
        let start = (cut..out.len())
            .find(|i| out.is_char_boundary(*i))
            .unwrap_or(out.len());
        out = out[start..].to_string();
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stderr_is_preferred_over_stdout() {
        let out = captured_output(b"downloading...\n", b"Error: nothing provides foo\n");
        assert_eq!(out, "Error: nothing provides foo");
    }

    #[test]
    fn stdout_is_used_when_stderr_is_blank() {
        let out = captured_output(b"Error: transaction failed\n", b"   \n");
        assert_eq!(out, "Error: transaction failed");
    }

    #[test]
    fn empty_output_stays_empty() {
        assert_eq!(captured_output(b"", b""), "");
    }

    #[test]
    fn only_the_last_lines_are_kept() {
        let text = (1..=50)
            .map(|i| i.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        let out = truncate_output(&text);
        let lines: Vec<&str> = out.lines().collect();

        assert_eq!(lines.len(), MAX_CAPTURED_OUTPUT_LINES);
        assert_eq!(lines.first(), Some(&"31"));
        assert_eq!(lines.last(), Some(&"50"));
    }

    #[test]
    fn output_is_capped_in_bytes() {
        let text = "x".repeat(MAX_CAPTURED_OUTPUT_BYTES * 2);
        assert_eq!(truncate_output(&text).len(), MAX_CAPTURED_OUTPUT_BYTES);
    }

    #[test]
    fn byte_cap_respects_char_boundaries() {
        let text = "é".repeat(MAX_CAPTURED_OUTPUT_BYTES);
        let out = truncate_output(&text);

        assert!(out.len() <= MAX_CAPTURED_OUTPUT_BYTES);
        assert!(out.chars().all(|c| c == 'é'));
    }
}
