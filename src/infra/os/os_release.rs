use tokio::fs::read_to_string;

use anyhow::{Context, Result};

pub struct OSRelease {
    pub name: Option<String>,
    pub version: Option<String>,
    pub id: Option<String>,
    pub id_like: Vec<String>,
    pub version_id: Option<String>,
    pub pretty_name: Option<String>,
}

pub enum OSFamily {
    DEB,
    RPM,
    UNKNOWN,
}

impl OSRelease {
    pub async fn load() -> Result<Self> {
        let content = read_to_string("/etc/os-release")
            .await
            .context("Failed to read /etc/os-release")?;
        Ok(Self::parse(&content))
    }

    pub fn family(&self) -> OSFamily {
        let tokens = self
            .id
            .iter()
            .map(String::as_str)
            .chain(self.id_like.iter().map(String::as_str));

        for token in tokens {
            match token {
                "debian" | "ubuntu" => return OSFamily::DEB,
                "rhel" | "fedora" | "centos" | "amzn" | "rocky" | "almalinux" => {
                    return OSFamily::RPM;
                }
                _ => {}
            }
        }
        OSFamily::UNKNOWN
    }

    fn parse(content: &str) -> Self {
        let mut name = None;
        let mut version = None;
        let mut id = None;
        let mut id_like = Vec::new();
        let mut version_id = None;
        let mut pretty_name = None;

        for line in content.lines() {
            let Some((key, val)) = line.split_once('=') else {
                continue;
            };
            let value = val.trim().trim_matches(['"', '\'']);
            match key.trim() {
                "NAME" => name = Some(value.to_string()),
                "VERSION" => version = Some(value.to_string()),
                "ID" => id = Some(value.to_string()),
                "ID_LIKE" => id_like = value.split_whitespace().map(str::to_string).collect(),
                "VERSION_ID" => version_id = Some(value.to_string()),
                "PRETTY_NAME" => pretty_name = Some(value.to_string()),
                _ => {}
            }
        }

        Self {
            name,
            version,
            id,
            id_like,
            version_id,
            pretty_name,
        }
    }
}
