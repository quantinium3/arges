use anyhow::{Context, Result};
use sqlx::SqlitePool;

use crate::{
    db::queries::packages::{self, DesiredState, PackageStatus},
    infra::packages::package_manager::PackageManager,
};

pub struct PackageDef {
    pub id: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub apt_name: Option<&'static str>,
    pub rpm_name: Option<&'static str>,
}

pub const CATALOG: &[PackageDef] = &[
    PackageDef {
        id: "git",
        name: "git",
        description: "distributed version control system",
        apt_name: Some("git"),
        rpm_name: Some("git"),
    },
    PackageDef {
        id: "curl",
        name: "curl",
        description: "command-line tool for transferring data with URLs",
        apt_name: Some("curl"),
        rpm_name: Some("curl"),
    },
    PackageDef {
        id: "curl-minimal",
        name: "curl-minimal",
        description: "curl built without libssh2/protocols not needed by dnf; ships by default on Fedora/RHEL minimal and cloud images, conflicts with full curl",
        apt_name: None,
        rpm_name: Some("curl-minimal"),
    },
    PackageDef {
        id: "htop",
        name: "htop",
        description: "interactive process viewer",
        apt_name: Some("htop"),
        rpm_name: Some("htop"),
    },
    PackageDef {
        id: "vim",
        name: "vim",
        description: "highly configurable text editor",
        apt_name: Some("vim"),
        rpm_name: Some("vim-enhanced"),
    },
];

impl PackageDef {
    fn name_for(&self, pm: &PackageManager) -> Option<&'static str> {
        match pm {
            PackageManager::APT => self.apt_name,
            PackageManager::RPM(_) => self.rpm_name,
        }
    }
}

pub async fn seed(pool: &SqlitePool, pm: &PackageManager) -> Result<()> {
    for def in CATALOG {
        if packages::exists(pool, def.id).await? {
            packages::update_metadata(pool, def.id, def.name, def.description).await?;
        } else {
            let (desired_state, status) = match def.name_for(pm) {
                Some(name) => {
                    let installed = pm
                        .is_installed(name)
                        .await
                        .with_context(|| format!("failed to probe install state of {}", def.id))?;
                    if installed {
                        (DesiredState::Installed, PackageStatus::Installed)
                    } else {
                        (DesiredState::Removed, PackageStatus::Removed)
                    }
                }
                // Not available via this host's package manager; harmless since with no
                // name mapping below it can never be actioned through the API either.
                None => (DesiredState::Removed, PackageStatus::Removed),
            };

            packages::insert_new(pool, def.id, def.name, def.description, desired_state, status)
                .await?;
        }

        if let Some(apt_name) = def.apt_name {
            packages::set_name_for_manager(pool, def.id, "apt", apt_name).await?;
        }
        if let Some(rpm_name) = def.rpm_name {
            packages::set_name_for_manager(pool, def.id, "rpm", rpm_name).await?;
        }
    }

    Ok(())
}
