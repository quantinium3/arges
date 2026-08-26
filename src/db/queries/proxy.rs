use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::Type, Serialize, Deserialize)]
#[sqlx(type_name = "text", rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum ProxyKind {
    Proxy,
    Redirect,
    Static,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::Type, Serialize, Deserialize)]
#[sqlx(type_name = "text", rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum TlsMode {
    Auto,
    Custom,
    Off,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::Type, Serialize, Deserialize)]
#[sqlx(type_name = "text", rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum UpstreamScheme {
    Http,
    Https,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProxyHost {
    pub id: String,
    pub kind: ProxyKind,
    pub priority: i64,
    pub enabled: bool,
    pub upstream_container: Option<String>,
    pub upstream_host: Option<String>,
    pub upstream_port: Option<i64>,
    pub upstream_scheme: UpstreamScheme,
    pub redirect_to: Option<String>,
    pub redirect_status: Option<i64>,
    pub static_root: Option<String>,
    pub tls_mode: TlsMode,
    pub tls_certificate_parameter: Option<String>,
    pub tls_private_key_parameter: Option<String>,
    pub dns_provider_id: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
    pub domains: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct NewProxyHost {
    pub kind: ProxyKind,
    pub domains: Vec<String>,
    pub priority: i64,
    pub enabled: bool,
    pub upstream_container: Option<String>,
    pub upstream_host: Option<String>,
    pub upstream_port: Option<i64>,
    pub upstream_scheme: UpstreamScheme,
    pub redirect_to: Option<String>,
    pub redirect_status: Option<i64>,
    pub static_root: Option<String>,
    pub tls_mode: TlsMode,
    pub tls_certificate_parameter: Option<String>,
    pub tls_private_key_parameter: Option<String>,
    pub dns_provider_id: Option<String>,
}

async fn attach_domains(pool: &SqlitePool, hosts: &mut [ProxyHost]) -> Result<()> {
    if hosts.is_empty() {
        return Ok(());
    }

    let rows = sqlx::query!(
        r#"select proxy_host_id as "proxy_host_id!", domain as "domain!"
        from proxy_host_domains order by domain"#
    )
    .fetch_all(pool)
    .await
    .context("failed to read proxy host domains")?;

    for host in hosts.iter_mut() {
        host.domains = rows
            .iter()
            .filter(|row| row.proxy_host_id == host.id)
            .map(|row| row.domain.clone())
            .collect();
    }

    Ok(())
}

pub async fn list(pool: &SqlitePool) -> Result<Vec<ProxyHost>> {
    let rows = sqlx::query!(
        r#"select
            id as "id!",
            kind as "kind!: ProxyKind",
            priority as "priority!",
            enabled as "enabled!: bool",
            upstream_container,
            upstream_host,
            upstream_port,
            upstream_scheme as "upstream_scheme!: UpstreamScheme",
            redirect_to,
            redirect_status,
            static_root,
            tls_mode as "tls_mode!: TlsMode",
            tls_certificate_parameter,
            tls_private_key_parameter,
            dns_provider_id,
            created_at as "created_at!",
            updated_at as "updated_at!"
        from proxy_hosts
        order by priority, id"#
    )
    .fetch_all(pool)
    .await
    .context("failed to list proxy hosts")?;

    let mut hosts: Vec<ProxyHost> = rows
        .into_iter()
        .map(|row| ProxyHost {
            id: row.id,
            kind: row.kind,
            priority: row.priority,
            enabled: row.enabled,
            upstream_container: row.upstream_container,
            upstream_host: row.upstream_host,
            upstream_port: row.upstream_port,
            upstream_scheme: row.upstream_scheme,
            redirect_to: row.redirect_to,
            redirect_status: row.redirect_status,
            static_root: row.static_root,
            tls_mode: row.tls_mode,
            tls_certificate_parameter: row.tls_certificate_parameter,
            tls_private_key_parameter: row.tls_private_key_parameter,
            dns_provider_id: row.dns_provider_id,
            created_at: row.created_at,
            updated_at: row.updated_at,
            domains: Vec::new(),
        })
        .collect();

    attach_domains(pool, &mut hosts).await?;

    Ok(hosts)
}

pub async fn fetch(pool: &SqlitePool, id: &str) -> Result<Option<ProxyHost>> {
    Ok(list(pool).await?.into_iter().find(|host| host.id == id))
}

pub async fn create(pool: &SqlitePool, id: &str, new: &NewProxyHost) -> Result<()> {
    let mut tx = pool.begin().await?;

    sqlx::query!(
        r#"insert into proxy_hosts (
            id, kind, priority, enabled,
            upstream_container, upstream_host, upstream_port, upstream_scheme,
            redirect_to, redirect_status, static_root,
            tls_mode, tls_certificate_parameter, tls_private_key_parameter, dns_provider_id
        ) values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)"#,
        id,
        new.kind,
        new.priority,
        new.enabled,
        new.upstream_container,
        new.upstream_host,
        new.upstream_port,
        new.upstream_scheme,
        new.redirect_to,
        new.redirect_status,
        new.static_root,
        new.tls_mode,
        new.tls_certificate_parameter,
        new.tls_private_key_parameter,
        new.dns_provider_id
    )
    .execute(&mut *tx)
    .await
    .with_context(|| format!("failed to create proxy host {id}"))?;

    for domain in &new.domains {
        sqlx::query!(
            "insert into proxy_host_domains (proxy_host_id, domain) values (?1, ?2)",
            id,
            domain
        )
        .execute(&mut *tx)
        .await
        .with_context(|| format!("failed to attach domain {domain} to proxy host {id}"))?;
    }

    tx.commit().await?;

    Ok(())
}

pub async fn update(pool: &SqlitePool, id: &str, new: &NewProxyHost) -> Result<bool> {
    let mut tx = pool.begin().await?;

    let result = sqlx::query!(
        r#"update proxy_hosts set
            kind = ?2, priority = ?3, enabled = ?4,
            upstream_container = ?5, upstream_host = ?6, upstream_port = ?7, upstream_scheme = ?8,
            redirect_to = ?9, redirect_status = ?10, static_root = ?11,
            tls_mode = ?12, tls_certificate_parameter = ?13, tls_private_key_parameter = ?14,
            dns_provider_id = ?15, updated_at = unixepoch()
        where id = ?1"#,
        id,
        new.kind,
        new.priority,
        new.enabled,
        new.upstream_container,
        new.upstream_host,
        new.upstream_port,
        new.upstream_scheme,
        new.redirect_to,
        new.redirect_status,
        new.static_root,
        new.tls_mode,
        new.tls_certificate_parameter,
        new.tls_private_key_parameter,
        new.dns_provider_id
    )
    .execute(&mut *tx)
    .await
    .with_context(|| format!("failed to update proxy host {id}"))?;

    if result.rows_affected() == 0 {
        return Ok(false);
    }

    sqlx::query!("delete from proxy_host_domains where proxy_host_id = ?", id)
        .execute(&mut *tx)
        .await?;

    for domain in &new.domains {
        sqlx::query!(
            "insert into proxy_host_domains (proxy_host_id, domain) values (?1, ?2)",
            id,
            domain
        )
        .execute(&mut *tx)
        .await
        .with_context(|| format!("failed to attach domain {domain} to proxy host {id}"))?;
    }

    tx.commit().await?;

    Ok(true)
}

pub async fn set_enabled(pool: &SqlitePool, id: &str, enabled: bool) -> Result<bool> {
    let result = sqlx::query!(
        "update proxy_hosts set enabled = ?2, updated_at = unixepoch() where id = ?1",
        id,
        enabled
    )
    .execute(pool)
    .await
    .with_context(|| format!("failed to toggle proxy host {id}"))?;

    Ok(result.rows_affected() > 0)
}

pub async fn set_upstream_container(pool: &SqlitePool, id: &str, container: &str) -> Result<bool> {
    let result = sqlx::query!(
        r#"update proxy_hosts set upstream_container = ?2, upstream_host = null,
            updated_at = unixepoch() where id = ?1"#,
        id,
        container
    )
    .execute(pool)
    .await
    .with_context(|| format!("failed to point proxy host {id} at {container}"))?;

    Ok(result.rows_affected() > 0)
}

pub async fn delete(pool: &SqlitePool, id: &str) -> Result<bool> {
    let result = sqlx::query!("delete from proxy_hosts where id = ?", id)
        .execute(pool)
        .await
        .with_context(|| format!("failed to delete proxy host {id}"))?;

    Ok(result.rows_affected() > 0)
}

pub async fn record_pending(pool: &SqlitePool, hash: &str, config: &str) -> Result<i64> {
    let id = sqlx::query!(
        r#"insert into proxy_config_revisions (config_hash, config_json, status)
        values (?1, ?2, 'pending')"#,
        hash,
        config
    )
    .execute(pool)
    .await
    .context("failed to record a proxy config revision")?
    .last_insert_rowid();

    Ok(id)
}

pub async fn mark_applied(pool: &SqlitePool, revision: i64) -> Result<()> {
    sqlx::query!(
        r#"update proxy_config_revisions
        set status = 'applied', applied_at = unixepoch(), error = null
        where id = ?"#,
        revision
    )
    .execute(pool)
    .await
    .context("failed to mark the proxy config revision applied")?;

    Ok(())
}

pub async fn mark_failed(pool: &SqlitePool, revision: i64, error: &str) -> Result<()> {
    sqlx::query!(
        "update proxy_config_revisions set status = 'failed', error = ?2 where id = ?1",
        revision,
        error
    )
    .execute(pool)
    .await
    .context("failed to mark the proxy config revision failed")?;

    Ok(())
}

pub async fn last_applied_hash(pool: &SqlitePool) -> Result<Option<String>> {
    sqlx::query_scalar!(
        r#"select config_hash as "config_hash!" from proxy_config_revisions
        where status = 'applied' order by id desc limit 1"#
    )
    .fetch_optional(pool)
    .await
    .context("failed to read the last applied proxy config")
}
