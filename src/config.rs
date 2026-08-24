use std::{env, path::PathBuf};

use anyhow::{Context, Result, ensure};

const MAX_SOCKET_PATH_LEN: usize = 107;

#[derive(Debug)]
pub struct Config {
    pub socket_path: PathBuf,
    pub db_path: PathBuf,
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

        let db_path = absolute_path_var("ARGES_DB_PATH")?;

        Ok(Self {
            socket_path,
            db_path,
        })
    }
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
