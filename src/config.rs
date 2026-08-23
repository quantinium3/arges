use std::{env, path::PathBuf};

use anyhow::{Context, Result, ensure};

const MAX_SOCKET_PATH_LEN: usize = 107;

#[derive(Debug)]
pub struct Config {
    pub socket_path: PathBuf,
    pub database_url: String,
    pub master_key_path: PathBuf,
}

impl Config {
    pub fn new() -> Result<Self> {
        let socket_path = absolute_path_var("ARGES_APP_SOCKET")?;
        ensure!(
            socket_path.as_os_str().len() <= MAX_SOCKET_PATH_LEN,
            "ARGES_APP_SOCKET must be at most {MAX_SOCKET_PATH_LEN} bytes, got {} bytes in {}",
            socket_path.as_os_str().len(),
            socket_path.display()
        );

        let database_url = var("ARGES_DATABASE_URL")?;
        ensure!(
            !database_url.trim().is_empty(),
            "ARGES_DATABASE_URL must not be empty"
        );

        let master_key_path = absolute_path_var("ARGES_MASTER_KEY_PATH")?;

        Ok(Self {
            socket_path,
            database_url,
            master_key_path,
        })
    }
}

fn var(name: &str) -> Result<String> {
    env::var(name).with_context(|| format!("{name} must be set"))
}

fn absolute_path_var(name: &str) -> Result<PathBuf> {
    let raw = env::var_os(name).with_context(|| format!("{name} must be set"))?;
    ensure!(!raw.is_empty(), "{name} must not be empty");

    let path = PathBuf::from(raw);
    ensure!(
        path.is_absolute(),
        "{name} must be an absolute path got {}",
        path.display()
    );
    Ok(path)
}
