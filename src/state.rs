use std::sync::Arc;

use sqlx::SqlitePool;
use tokio::sync::Notify;

use crate::infra::{
    containers::docker::DockerClient, packages::package_manager::PackageManager,
    parameters::secrets::MasterKey,
};

#[derive(Clone)]
pub struct AppState {
    pub db: SqlitePool,
    pub package_manager: PackageManager,
    pub reconcile_notify: Arc<Notify>,
    pub master_key: Arc<MasterKey>,
    pub docker: Option<DockerClient>,
}
