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
    PackageDef {
        id: "tmux",
        name: "tmux",
        description: "terminal multiplexer for persistent remote sessions",
        apt_name: Some("tmux"),
        rpm_name: Some("tmux"),
    },
    PackageDef {
        id: "ripgrep",
        name: "ripgrep",
        description: "fast recursive search tool",
        apt_name: Some("ripgrep"),
        rpm_name: Some("ripgrep"),
    },
    PackageDef {
        id: "fd",
        name: "fd",
        description: "simple and fast alternative to find",
        apt_name: Some("fd-find"),
        rpm_name: Some("fd-find"),
    },
    PackageDef {
        id: "jq",
        name: "jq",
        description: "command-line JSON processor",
        apt_name: Some("jq"),
        rpm_name: Some("jq"),
    },
    PackageDef {
        id: "tree",
        name: "tree",
        description: "recursive directory listing as a tree",
        apt_name: Some("tree"),
        rpm_name: Some("tree"),
    },
    PackageDef {
        id: "rsync",
        name: "rsync",
        description: "fast incremental file transfer and synchronisation",
        apt_name: Some("rsync"),
        rpm_name: Some("rsync"),
    },
    PackageDef {
        id: "unzip",
        name: "unzip",
        description: "extractor for zip archives",
        apt_name: Some("unzip"),
        rpm_name: Some("unzip"),
    },
    PackageDef {
        id: "ncdu",
        name: "ncdu",
        description: "disk usage analyser with an ncurses interface",
        apt_name: Some("ncdu"),
        rpm_name: Some("ncdu"),
    },
    PackageDef {
        id: "lsof",
        name: "lsof",
        description: "lists open files and the processes holding them",
        apt_name: Some("lsof"),
        rpm_name: Some("lsof"),
    },
    PackageDef {
        id: "btop",
        name: "btop",
        description: "resource monitor with a graphical terminal interface",
        apt_name: Some("btop"),
        rpm_name: Some("btop"),
    },
    PackageDef {
        id: "sqlite",
        name: "sqlite",
        description: "command-line shell for sqlite databases",
        apt_name: Some("sqlite3"),
        rpm_name: Some("sqlite"),
    },
    PackageDef {
        id: "dnsutils",
        name: "dnsutils",
        description: "dns lookup tools including dig and nslookup",
        apt_name: Some("dnsutils"),
        rpm_name: Some("bind-utils"),
    },
    PackageDef {
        id: "netcat",
        name: "netcat",
        description: "tcp and udp connection and port scanning utility",
        apt_name: Some("netcat-openbsd"),
        rpm_name: Some("nmap-ncat"),
    },
    PackageDef {
        id: "iproute2",
        name: "iproute2",
        description: "ip, ss and other modern networking tools",
        apt_name: Some("iproute2"),
        rpm_name: Some("iproute"),
    },
    PackageDef {
        id: "fail2ban",
        name: "fail2ban",
        description: "bans hosts that show malicious sign-in patterns",
        apt_name: Some("fail2ban"),
        rpm_name: Some("fail2ban"),
    },
    PackageDef {
        id: "ca-certificates",
        name: "ca-certificates",
        description: "common ca certificates for tls verification",
        apt_name: Some("ca-certificates"),
        rpm_name: Some("ca-certificates"),
    },
    PackageDef {
        id: "man-db",
        name: "man-db",
        description: "manual page reader and index",
        apt_name: Some("man-db"),
        rpm_name: Some("man-db"),
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
