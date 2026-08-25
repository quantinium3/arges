use anyhow::{Context, Result};
use serde::Serialize;
use sqlx::{FromRow, SqlitePool};

#[derive(Debug, Clone, FromRow, Serialize)]
pub struct AuditEntry {
    pub id: i64,
    pub subject_type: String,
    pub subject_id: Option<String>,
    pub action: String,
    pub detail: Option<String>,
    pub created_at: i64,
}

pub async fn record(
    pool: &SqlitePool,
    subject_type: &str,
    subject_id: Option<&str>,
    action: &str,
    detail: Option<&str>,
) -> Result<()> {
    sqlx::query!(
        r#"insert into audit_log (subject_type, subject_id, action, detail)
        values (?1, ?2, ?3, ?4)"#,
        subject_type,
        subject_id,
        action,
        detail
    )
    .execute(pool)
    .await
    .with_context(|| format!("failed to record {action} on {subject_type}"))?;

    Ok(())
}

pub async fn recent(pool: &SqlitePool, subject_type: &str, limit: i64) -> Result<Vec<AuditEntry>> {
    sqlx::query_as!(
        AuditEntry,
        r#"select
            id as "id!",
            subject_type as "subject_type!",
            subject_id,
            action as "action!",
            detail,
            created_at as "created_at!"
        from audit_log
        where subject_type = ?1
        order by id desc
        limit ?2"#,
        subject_type,
        limit
    )
    .fetch_all(pool)
    .await
    .context("failed to read the audit log")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[sqlx::test]
    async fn entries_come_back_newest_first(pool: SqlitePool) {
        for action in ["created", "updated", "deleted"] {
            record(&pool, "proxy_host", Some("h1"), action, None)
                .await
                .unwrap();
        }
        record(&pool, "parameter", Some("/a"), "read", None)
            .await
            .unwrap();

        let entries = recent(&pool, "proxy_host", 10).await.unwrap();

        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].action, "deleted");
        assert_eq!(entries[2].action, "created");
    }

    #[sqlx::test]
    async fn history_survives_the_subject_being_deleted(pool: SqlitePool) {
        record(
            &pool,
            "proxy_host",
            Some("gone"),
            "created",
            Some("example.com"),
        )
        .await
        .unwrap();
        record(&pool, "proxy_host", Some("gone"), "deleted", None)
            .await
            .unwrap();

        assert_eq!(recent(&pool, "proxy_host", 10).await.unwrap().len(), 2);
    }
}
