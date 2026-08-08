use std::{env, net::IpAddr, path::PathBuf};

use anyhow::{Context, Result, ensure};

#[derive(Debug)]
pub struct Config {
    pub host: IpAddr,
    pub port: u16,
    pub database_url: String,
    pub master_key_path: PathBuf,
}

impl Config {
    pub fn new() -> Result<Self> {
        let host = env::var("ARGES_APP_HOST")
            .context("ARGES_APP_HOST must be set")?
            .parse::<IpAddr>()
            .context("ARGES_APP_HOST must be a valid IP address")?;
        let port = env::var("ARGES_APP_PORT")
            .context("ARGES_APP_PORT must be set")?
            .parse::<u16>()
            .context("ARGES_APP_PORT must be a valid TCP port")?;
        let database_url =
            env::var("ARGES_DATABASE_URL").context("ARGES_DATABASE_URL must be set")?;
        let raw_master_key_path =
            env::var("ARGES_MASTER_KEY_PATH").context("ARGES_MASTER_KEY_PATH must be set")?;
        let master_key_path = PathBuf::from(raw_master_key_path);
        ensure!(
            master_key_path.is_absolute(),
            "ARGES_MASTER_KEY_PATH must be an absolute path got {}",
            master_key_path.display()
        );
        Ok(Self {
            host,
            port,
            database_url,
            master_key_path,
        })
    }
}
