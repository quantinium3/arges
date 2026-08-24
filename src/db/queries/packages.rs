use anyhow::{Context, Result};
use serde::Serialize;
use sqlx::{FromRow, SqlitePool};

#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::Type, Serialize)]
#[sqlx(type_name = "text", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum DesiredState {
    Installed,
    Removed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::Type, Serialize)]
#[sqlx(type_name = "text", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum PackageStatus {
    Pending,
    Installing,
    Installed,
    Failed,
    Removing,
    Removed,
}

#[derive(Debug, Clone, FromRow)]
pub struct Package {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub desired_state: DesiredState,
    pub status: PackageStatus,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, FromRow)]
pub struct PackageView {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub desired_state: DesiredState,
    pub status: PackageStatus,
    pub last_error: Option<String>,
}

pub async fn exists(pool: &SqlitePool, id: &str) -> Result<bool> {
    Ok(sqlx::query_scalar!(
        r#"select count(*) as "count!: i64" from skirnir_packages where id = ?"#,
        id
    )
    .fetch_one(pool)
    .await?
        > 0)
}

pub async fn insert_new(
    pool: &SqlitePool,
    id: &str,
    name: &str,
    description: &str,
    desired_state: DesiredState,
    status: PackageStatus,
) -> Result<()> {
    sqlx::query!(
        r#"insert into skirnir_packages (id, name, description, desired_state, status)
        values (?, ?, ?, ?, ?)"#,
        id,
        name,
        description,
        desired_state,
        status
    )
    .execute(pool)
    .await
    .with_context(|| format!("failed to insert package {id}"))?;
    Ok(())
}

pub async fn update_metadata(pool: &SqlitePool, id: &str, name: &str, description: &str) -> Result<()> {
    sqlx::query!(
        "update skirnir_packages set name = ?, description = ? where id = ?",
        name,
        description,
        id
    )
    .execute(pool)
    .await
    .with_context(|| format!("failed to update package {id}"))?;
    Ok(())
}

pub async fn set_name_for_manager(
    pool: &SqlitePool,
    package_id: &str,
    manager: &str,
    name: &str,
) -> Result<()> {
    sqlx::query!(
        r#"insert into package_names (package_id, package_manager, name)
        values (?, ?, ?)
        on conflict (package_id, package_manager) do update set name = excluded.name"#,
        package_id,
        manager,
        name
    )
    .execute(pool)
    .await
    .with_context(|| format!("failed to set {manager} name for {package_id}"))?;
    Ok(())
}

pub async fn fetch_all(pool: &SqlitePool) -> Result<Vec<Package>> {
    Ok(sqlx::query_as!(
        Package,
        r#"select
            id as "id!",
            name as "name!",
            description,
            desired_state as "desired_state!: DesiredState",
            status as "status!: PackageStatus",
            created_at as "created_at!",
            updated_at as "updated_at!"
        from skirnir_packages"#
    )
    .fetch_all(pool)
    .await?)
}

pub async fn fetch_all_for_manager(pool: &SqlitePool, manager: &str) -> Result<Vec<PackageView>> {
    Ok(sqlx::query_as!(
        PackageView,
        r#"select
            p.id as "id!",
            p.name as "name!",
            p.description,
            p.desired_state as "desired_state!: DesiredState",
            p.status as "status!: PackageStatus",
            case when p.status = 'failed' then (
                select t.reason
                from package_state_transitions t
                where t.package_id = p.id and t.to_status = 'failed'
                order by t.id desc
                limit 1
            ) end as "last_error?: String"
        from skirnir_packages p
        inner join package_names pn on pn.package_id = p.id and pn.package_manager = ?"#,
        manager
    )
    .fetch_all(pool)
    .await?)
}

pub async fn set_desired_state(
    pool: &SqlitePool,
    id: &str,
    manager: &str,
    to: DesiredState,
) -> Result<bool> {
    let result = sqlx::query!(
        r#"update skirnir_packages
        set desired_state = ?, updated_at = unixepoch()
        where id = ? and exists (
            select 1 from package_names
            where package_id = skirnir_packages.id and package_manager = ?
        )"#,
        to,
        id,
        manager
    )
    .execute(pool)
    .await
    .context("failed to update desired state")?;

    Ok(result.rows_affected() > 0)
}

pub async fn lookup_name_for_manager(
    pool: &SqlitePool,
    package_id: &str,
    manager: &str,
) -> Result<Option<String>> {
    sqlx::query_scalar!(
        r#"select name as "name!" from package_names where package_id = ? and package_manager = ?"#,
        package_id,
        manager
    )
    .fetch_optional(pool)
    .await
    .context("failed to look up package name mapping")
}

pub async fn transition(
    pool: &SqlitePool,
    pkg: &Package,
    to: PackageStatus,
    reason: Option<&str>,
) -> Result<()> {
    let mut tx = pool.begin().await?;

    sqlx::query!(
        "update skirnir_packages set status = ?, updated_at = unixepoch() where id = ?",
        to,
        pkg.id
    )
    .execute(&mut *tx)
    .await?;

    sqlx::query!(
        "insert into package_state_transitions (package_id, from_status, to_status, reason) values (?, ?, ?, ?)",
        pkg.id,
        pkg.status,
        to,
        reason
    )
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;
    Ok(())
}

pub async fn transition_failed(
    pool: &SqlitePool,
    pkg: &Package,
    err: &anyhow::Error,
) -> Result<()> {
    transition(pool, pkg, PackageStatus::Failed, Some(&format!("{err:#}"))).await
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn seed(pool: &SqlitePool) -> Package {
        insert_new(
            pool,
            "htop",
            "htop",
            "interactive process viewer",
            DesiredState::Installed,
            PackageStatus::Removed,
        )
        .await
        .unwrap();
        set_name_for_manager(pool, "htop", "rpm", "htop")
            .await
            .unwrap();
        fetch_all(pool).await.unwrap().pop().unwrap()
    }

    #[sqlx::test]
    async fn last_error_surfaces_for_failed_package(pool: SqlitePool) {
        let pkg = seed(&pool).await;
        let err = anyhow::anyhow!("conflicts with curl").context("install of htop failed");
        transition_failed(&pool, &pkg, &err).await.unwrap();

        let view = fetch_all_for_manager(&pool, "rpm").await.unwrap();

        assert_eq!(view.len(), 1);
        assert_eq!(view[0].status, PackageStatus::Failed);
        assert_eq!(
            view[0].last_error.as_deref(),
            Some("install of htop failed: conflicts with curl")
        );
    }

    #[sqlx::test]
    async fn last_error_clears_after_a_successful_retry(pool: SqlitePool) {
        let pkg = seed(&pool).await;
        transition_failed(&pool, &pkg, &anyhow::anyhow!("conflicts with curl"))
            .await
            .unwrap();

        let failed = Package {
            status: PackageStatus::Failed,
            ..pkg
        };
        transition(&pool, &failed, PackageStatus::Installed, None)
            .await
            .unwrap();

        let view = fetch_all_for_manager(&pool, "rpm").await.unwrap();

        assert_eq!(view[0].status, PackageStatus::Installed);
        assert_eq!(view[0].last_error, None);
    }

    #[sqlx::test]
    async fn healthy_package_has_no_last_error(pool: SqlitePool) {
        seed(&pool).await;

        let view = fetch_all_for_manager(&pool, "rpm").await.unwrap();

        assert_eq!(view[0].last_error, None);
    }
}
