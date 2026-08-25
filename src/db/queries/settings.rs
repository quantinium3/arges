use anyhow::{Context, Result};
use sqlx::SqlitePool;

pub async fn get_bool(pool: &SqlitePool, key: &str, default: bool) -> Result<bool> {
    let stored: Option<String> = sqlx::query_scalar!(
        r#"select value as "value!" from settings where key = ?"#,
        key
    )
    .fetch_optional(pool)
    .await
    .with_context(|| format!("failed to read setting {key}"))?;

    Ok(match stored.as_deref() {
        Some("true") => true,
        Some("false") => false,
        _ => default,
    })
}

pub async fn set_bool(pool: &SqlitePool, key: &str, value: bool) -> Result<()> {
    let value = if value { "true" } else { "false" };

    sqlx::query!(
        r#"insert into settings (key, value) values (?1, ?2)
        on conflict (key) do update set value = excluded.value, updated_at = unixepoch()"#,
        key,
        value
    )
    .execute(pool)
    .await
    .with_context(|| format!("failed to store setting {key}"))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[sqlx::test]
    async fn an_unset_key_falls_back_to_the_default(pool: SqlitePool) {
        assert!(get_bool(&pool, "missing", true).await.unwrap());
        assert!(!get_bool(&pool, "missing", false).await.unwrap());
    }

    #[sqlx::test]
    async fn a_stored_value_overrides_the_default(pool: SqlitePool) {
        set_bool(&pool, "flag", false).await.unwrap();
        assert!(!get_bool(&pool, "flag", true).await.unwrap());

        set_bool(&pool, "flag", true).await.unwrap();
        assert!(get_bool(&pool, "flag", false).await.unwrap());
    }

    #[sqlx::test]
    async fn a_corrupt_value_falls_back_to_the_default(pool: SqlitePool) {
        sqlx::query!("insert into settings (key, value) values ('flag', 'garbage')")
            .execute(&pool)
            .await
            .unwrap();

        assert!(get_bool(&pool, "flag", true).await.unwrap());
    }
}
