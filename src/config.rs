use std::{env, path::PathBuf};

use anyhow::{Context, Result, ensure};

#[derive(Debug)]
pub struct Config {
    pub socket_path: PathBuf,
    pub database_url: String,
    pub master_key_path: PathBuf,
}

impl Config {
    pub fn new() -> Result<Self> {
        let raw_socket_path =
            env::var("ARGES_APP_SOCKET").context("ARGES_APP_SOCKET must be set")?;
        let socket_path = PathBuf::from(raw_socket_path);
        ensure!(
            socket_path.is_absolute(),
            "ARGES_APP_SOCKET must be an absolute path got {}",
            socket_path.display()
        );
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
            socket_path,
            database_url,
            master_key_path,
        })
    }
}
