use std::sync::Arc;

use sqlx::SqlitePool;
use tokio::sync::Notify;

use crate::infra::packages::package_manager::PackageManager;

#[derive(Clone)]
pub struct AppState {
    pub db: SqlitePool,
    pub package_manager: PackageManager,
    pub reconcile_notify: Arc<Notify>,
}
