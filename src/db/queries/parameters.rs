use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, SqlitePool};

use crate::infra::parameters::secrets::EncryptedValue;

#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::Type, Serialize, Deserialize)]
#[sqlx(type_name = "text", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum ParameterType {
    String,
    SecureString,
}

#[derive(Debug, Clone, FromRow)]
pub struct Parameter {
    pub key: String,
    pub kind: ParameterType,
    pub version: i64,
    pub key_id: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

pub enum ParameterValue {
    String { version: u64, value: String },
    Secure { version: u64, value: EncryptedValue },
}

fn required<T>(key: &str, field: &str, value: Option<T>) -> Result<T> {
    value.with_context(|| format!("parameter {key} is a secure_string but has no {field}"))
}

pub async fn list(pool: &SqlitePool, prefix: &str) -> Result<Vec<Parameter>> {
    let pattern = format!(
        "{}%",
        prefix
            .replace('\\', r"\\")
            .replace('%', r"\%")
            .replace('_', r"\_")
    );

    sqlx::query_as!(
        Parameter,
        r#"select
            key as "key!",
            type as "kind!: ParameterType",
            version as "version!",
            key_id,
            created_at as "created_at!",
            updated_at as "updated_at!"
        from parameters
        where key like ?1 escape '\'
        order by key"#,
        pattern
    )
    .fetch_all(pool)
    .await
    .context("failed to list parameters")
}

pub async fn next_version(pool: &SqlitePool, key: &str) -> Result<u64> {
    let current: Option<i64> = sqlx::query_scalar!(
        r#"select version as "version!" from parameters where key = ?"#,
        key
    )
    .fetch_optional(pool)
    .await
    .with_context(|| format!("failed to read version of parameter {key}"))?;

    Ok(current.unwrap_or(0) as u64 + 1)
}

pub async fn fetch(pool: &SqlitePool, key: &str) -> Result<Option<ParameterValue>> {
    let row = sqlx::query!(
        r#"select
            type as "kind!: ParameterType",
            version as "version!",
            value,
            key_id,
            ciphertext,
            nonce,
            wrapped_dek,
            dek_nonce
        from parameters
        where key = ?"#,
        key
    )
    .fetch_optional(pool)
    .await
    .with_context(|| format!("failed to fetch parameter {key}"))?;

    let Some(row) = row else {
        return Ok(None);
    };

    match row.kind {
        ParameterType::String => Ok(Some(ParameterValue::String {
            version: row.version as u64,
            value: required(key, "value", row.value)?,
        })),
        ParameterType::SecureString => Ok(Some(ParameterValue::Secure {
            version: row.version as u64,
            value: EncryptedValue {
                key_id: required(key, "key_id", row.key_id)?,
                ciphertext: required(key, "ciphertext", row.ciphertext)?,
                nonce: required(key, "nonce", row.nonce)?,
                wrapped_dek: required(key, "wrapped_dek", row.wrapped_dek)?,
                dek_nonce: required(key, "dek_nonce", row.dek_nonce)?,
            },
        })),
    }
}

pub async fn put_string(pool: &SqlitePool, key: &str, version: u64, value: &str) -> Result<bool> {
    let version = version as i64;

    let result = sqlx::query!(
        r#"insert into parameters (key, type, version, value)
        values (?1, 'string', ?2, ?3)
        on conflict (key) do update set
            type = 'string',
            version = excluded.version,
            value = excluded.value,
            key_id = null,
            ciphertext = null,
            nonce = null,
            wrapped_dek = null,
            dek_nonce = null,
            updated_at = unixepoch()
        where parameters.version = excluded.version - 1"#,
        key,
        version,
        value
    )
    .execute(pool)
    .await
    .with_context(|| format!("failed to store parameter {key}"))?;

    Ok(result.rows_affected() > 0)
}

pub async fn put_secure(
    pool: &SqlitePool,
    key: &str,
    version: u64,
    value: &EncryptedValue,
) -> Result<bool> {
    let version = version as i64;

    let result = sqlx::query!(
        r#"insert into parameters
            (key, type, version, key_id, ciphertext, nonce, wrapped_dek, dek_nonce)
        values (?1, 'secure_string', ?2, ?3, ?4, ?5, ?6, ?7)
        on conflict (key) do update set
            type = 'secure_string',
            version = excluded.version,
            value = null,
            key_id = excluded.key_id,
            ciphertext = excluded.ciphertext,
            nonce = excluded.nonce,
            wrapped_dek = excluded.wrapped_dek,
            dek_nonce = excluded.dek_nonce,
            updated_at = unixepoch()
        where parameters.version = excluded.version - 1"#,
        key,
        version,
        value.key_id,
        value.ciphertext,
        value.nonce,
        value.wrapped_dek,
        value.dek_nonce
    )
    .execute(pool)
    .await
    .with_context(|| format!("failed to store parameter {key}"))?;

    Ok(result.rows_affected() > 0)
}

pub async fn delete(pool: &SqlitePool, key: &str) -> Result<bool> {
    let result = sqlx::query!("delete from parameters where key = ?", key)
        .execute(pool)
        .await
        .with_context(|| format!("failed to delete parameter {key}"))?;

    Ok(result.rows_affected() > 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infra::parameters::secrets::MasterKey;

    async fn master_key() -> (MasterKey, std::path::PathBuf) {
        use std::os::unix::fs::PermissionsExt;

        let dir = std::env::temp_dir().join(format!(
            "arges-params-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("master.key");

        let mut key = [0u8; 32];
        getrandom::fill(&mut key).unwrap();
        std::fs::write(&path, key).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();

        (MasterKey::load(&path).await.unwrap(), dir)
    }

    async fn put_new_string(pool: &SqlitePool, key: &str, value: &str) -> u64 {
        let version = next_version(pool, key).await.unwrap();
        assert!(put_string(pool, key, version, value).await.unwrap());
        version
    }

    #[sqlx::test]
    async fn string_round_trips(pool: SqlitePool) {
        assert_eq!(put_new_string(&pool, "/app/name", "arges").await, 1);

        match fetch(&pool, "/app/name").await.unwrap().unwrap() {
            ParameterValue::String { version, value } => {
                assert_eq!(version, 1);
                assert_eq!(value, "arges");
            }
            _ => panic!("expected a plain string"),
        }
    }

    #[sqlx::test]
    async fn secure_round_trips_through_the_master_key(pool: SqlitePool) {
        let (mk, dir) = master_key().await;
        let key = "/app/db_password";

        let version = next_version(&pool, key).await.unwrap();
        let enc = mk.encrypt(key, version, "hunter2").unwrap();
        assert!(put_secure(&pool, key, version, &enc).await.unwrap());

        match fetch(&pool, key).await.unwrap().unwrap() {
            ParameterValue::Secure { version, value } => {
                assert_eq!(value.key_id, mk.id());
                assert_eq!(&*mk.decrypt(key, version, &value).unwrap(), "hunter2");
            }
            _ => panic!("expected a secure string"),
        }

        std::fs::remove_dir_all(dir).unwrap();
    }

    #[sqlx::test]
    async fn updating_a_secret_bumps_the_version_and_stays_decryptable(pool: SqlitePool) {
        let (mk, dir) = master_key().await;
        let key = "/app/db_password";

        for (expected, secret) in [(1u64, "first"), (2, "second"), (3, "third")] {
            let version = next_version(&pool, key).await.unwrap();
            assert_eq!(version, expected);
            let enc = mk.encrypt(key, version, secret).unwrap();
            assert!(put_secure(&pool, key, version, &enc).await.unwrap());
        }

        let ParameterValue::Secure { version, value } = fetch(&pool, key).await.unwrap().unwrap()
        else {
            panic!("expected a secure string");
        };
        assert_eq!(version, 3);
        assert_eq!(&*mk.decrypt(key, version, &value).unwrap(), "third");

        std::fs::remove_dir_all(dir).unwrap();
    }

    #[sqlx::test]
    async fn a_stale_write_is_rejected_instead_of_clobbering(pool: SqlitePool) {
        let key = "/app/name";
        let stale = next_version(&pool, key).await.unwrap();
        put_new_string(&pool, key, "winner").await;

        assert!(!put_string(&pool, key, stale, "loser").await.unwrap());

        match fetch(&pool, key).await.unwrap().unwrap() {
            ParameterValue::String { version, value } => {
                assert_eq!(version, 1);
                assert_eq!(value, "winner");
            }
            _ => panic!("expected a plain string"),
        }
    }

    #[sqlx::test]
    async fn switching_type_clears_the_columns_of_the_old_type(pool: SqlitePool) {
        let (mk, dir) = master_key().await;
        let key = "/app/token";

        let version = next_version(&pool, key).await.unwrap();
        let enc = mk.encrypt(key, version, "secret").unwrap();
        assert!(put_secure(&pool, key, version, &enc).await.unwrap());

        let version = next_version(&pool, key).await.unwrap();
        assert!(put_string(&pool, key, version, "public").await.unwrap());

        let row = sqlx::query!(
            r#"select type as "kind!: ParameterType", value, key_id, ciphertext, nonce,
                wrapped_dek, dek_nonce from parameters where key = ?"#,
            key
        )
        .fetch_one(&pool)
        .await
        .unwrap();

        assert_eq!(row.kind, ParameterType::String);
        assert_eq!(row.value.as_deref(), Some("public"));
        assert!(row.key_id.is_none());
        assert!(row.ciphertext.is_none());
        assert!(row.nonce.is_none());
        assert!(row.wrapped_dek.is_none());
        assert!(row.dek_nonce.is_none());

        std::fs::remove_dir_all(dir).unwrap();
    }

    #[sqlx::test]
    async fn list_matches_a_literal_prefix(pool: SqlitePool) {
        put_new_string(&pool, "/app/db_password", "a").await;
        put_new_string(&pool, "/app/dbXpassword", "b").await;
        put_new_string(&pool, "/other/thing", "c").await;

        let matched = list(&pool, "/app/db_").await.unwrap();
        assert_eq!(matched.len(), 1);
        assert_eq!(matched[0].key, "/app/db_password");
        assert_eq!(matched[0].kind, ParameterType::String);

        assert_eq!(list(&pool, "/app/").await.unwrap().len(), 2);
        assert_eq!(list(&pool, "/").await.unwrap().len(), 3);
    }

    #[sqlx::test]
    async fn list_never_exposes_values(pool: SqlitePool) {
        let (mk, dir) = master_key().await;
        let key = "/app/db_password";
        let version = next_version(&pool, key).await.unwrap();
        let enc = mk.encrypt(key, version, "hunter2").unwrap();
        put_secure(&pool, key, version, &enc).await.unwrap();

        let listed = list(&pool, "/").await.unwrap();
        assert_eq!(listed[0].kind, ParameterType::SecureString);
        assert_eq!(listed[0].key_id.as_deref(), Some(mk.id()));

        std::fs::remove_dir_all(dir).unwrap();
    }

    #[sqlx::test]
    async fn malformed_keys_are_rejected_by_the_schema(pool: SqlitePool) {
        for key in ["app/name", "/app//name", "/app/", "/"] {
            assert!(
                put_string(&pool, key, 1, "x").await.is_err(),
                "{key} should have been rejected"
            );
        }
    }

    #[sqlx::test]
    async fn fetch_and_delete_handle_a_missing_key(pool: SqlitePool) {
        assert!(fetch(&pool, "/nope").await.unwrap().is_none());
        assert!(!delete(&pool, "/nope").await.unwrap());

        put_new_string(&pool, "/app/name", "arges").await;
        assert!(delete(&pool, "/app/name").await.unwrap());
        assert!(fetch(&pool, "/app/name").await.unwrap().is_none());
    }
}
