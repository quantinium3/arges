use std::sync::Arc;

use crate::config::Config;

#[derive(Debug, Clone)]
pub struct AppState {
    pub config: Arc<Config>,
}

impl AppState {
    pub fn new(config: Arc<Config>) -> Self {
        Self { config }
    }
}
