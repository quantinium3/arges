use anyhow::{Context, Result};
use sqlx::SqlitePool;

pub async fn migrate(pool: &SqlitePool) -> Result<()> {
    sqlx::migrate!("./migrations")
        .run(pool)
        .await
        .context("Failed to run database migrations")?;
    Ok(())
}
