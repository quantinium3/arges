use anyhow::{Context, Result, bail, ensure};
use tokio::process::Command;

use crate::{
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

    pub async fn install(&self, name: &str) -> Result<()> {
        let status = self
            .mutate_command("install", name)
            .status()
            .await
            .with_context(|| format!("failed to spawn install of {name}"))?;
        ensure!(status.success(), "install of {name} failed ({status})");
        Ok(())
    }

    pub async fn remove(&self, name: &str) -> Result<()> {
        let status = self
            .mutate_command("remove", name)
            .status()
            .await
            .with_context(|| format!("failed to spawn removal of {name}"))?;
        ensure!(status.success(), "removal of {name} failed ({status})");
        Ok(())
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
