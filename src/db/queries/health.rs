use sqlx::SqlitePool;

pub async fn is_healthy(pool: &SqlitePool) -> bool {
    sqlx::query_scalar!("SELECT 1")
        .fetch_one(pool)
        .await
        .is_ok()
}
