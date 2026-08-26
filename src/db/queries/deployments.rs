use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::Type, Serialize, Deserialize)]
#[sqlx(type_name = "text", rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum DesiredState {
    Running,
    Stopped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::Type, Serialize, Deserialize)]
#[sqlx(type_name = "text", rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum DeploymentStatus {
    Pending,
    Deploying,
    Running,
    Failed,
    Stopping,
    Stopped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::Type, Serialize, Deserialize)]
#[sqlx(type_name = "text", rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum Builder {
    Railpack,
    Nixpacks,
    Dockerfile,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::Type, Serialize, Deserialize)]
#[sqlx(type_name = "text", rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum EnvScope {
    Runtime,
    Build,
    Both,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::Type, Serialize, Deserialize)]
#[sqlx(type_name = "text", rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    Tcp,
    Udp,
}

#[derive(Debug, Clone, Serialize)]
pub struct DeploymentEnv {
    pub name: String,
    pub scope: EnvScope,
    pub value: Option<String>,
    pub parameter_key: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DeploymentVolume {
    pub container_path: String,
    pub volume_name: String,
    pub read_only: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct DeploymentPort {
    pub host_port: i64,
    pub protocol: Protocol,
    pub exposed: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct DeploymentSource {
    pub repository: String,
    pub git_ref: String,
    pub subdirectory: Option<String>,
    pub credential_key: Option<String>,
    pub builder: Builder,
    pub dockerfile_path: Option<String>,
    pub install_command: Option<String>,
    pub build_command: Option<String>,
    pub start_command: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DeploymentRelease {
    pub id: String,
    pub deployment_id: String,
    pub tag: String,
    pub image: String,
    pub digest: Option<String>,
    pub source_ref: Option<String>,
    pub commit_sha: Option<String>,
    pub created_at: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Deployment {
    pub id: String,
    pub name: String,
    pub desired_state: DesiredState,
    pub status: DeploymentStatus,
    pub last_error: Option<String>,
    pub desired_release_id: Option<String>,
    pub active_release_id: Option<String>,
    pub container_port: Option<i64>,
    pub memory_limit_mb: Option<i64>,
    pub cpu_shares: Option<i64>,
    pub health_path: Option<String>,
    pub health_timeout_seconds: i64,
    pub proxy_host_id: Option<String>,
    pub retained_releases: i64,
    pub created_at: i64,
    pub updated_at: i64,
    pub source: Option<DeploymentSource>,
    pub env: Vec<DeploymentEnv>,
    pub volumes: Vec<DeploymentVolume>,
    pub ports: Vec<DeploymentPort>,
}

#[derive(Debug, Clone)]
pub struct NewDeployment {
    pub name: String,
    pub container_port: Option<i64>,
    pub memory_limit_mb: Option<i64>,
    pub cpu_shares: Option<i64>,
    pub health_path: Option<String>,
    pub health_timeout_seconds: i64,
    pub proxy_host_id: Option<String>,
    pub retained_releases: i64,
    pub source: Option<DeploymentSource>,
    pub env: Vec<DeploymentEnv>,
    pub volumes: Vec<DeploymentVolume>,
    pub ports: Vec<DeploymentPort>,
}

async fn attach_children(pool: &SqlitePool, deployments: &mut [Deployment]) -> Result<()> {
    if deployments.is_empty() {
        return Ok(());
    }

    let sources = sqlx::query!(
        r#"select deployment_id as "deployment_id!", repository as "repository!",
            git_ref as "git_ref!", subdirectory, credential_key,
            builder as "builder!: Builder", dockerfile_path,
            install_command, build_command, start_command
        from deployment_sources"#
    )
    .fetch_all(pool)
    .await
    .context("failed to read deployment sources")?;

    let env = sqlx::query!(
        r#"select deployment_id as "deployment_id!", name as "name!",
            scope as "scope!: EnvScope", value, parameter_key
        from deployment_env order by name"#
    )
    .fetch_all(pool)
    .await
    .context("failed to read deployment env")?;

    let volumes = sqlx::query!(
        r#"select deployment_id as "deployment_id!", container_path as "container_path!",
            volume_name as "volume_name!", read_only as "read_only!: bool"
        from deployment_volumes order by container_path"#
    )
    .fetch_all(pool)
    .await
    .context("failed to read deployment volumes")?;

    let ports = sqlx::query!(
        r#"select deployment_id, host_port as "host_port!",
            protocol as "protocol!: Protocol", exposed as "exposed!: bool"
        from port_allocations where deployment_id is not null order by host_port"#
    )
    .fetch_all(pool)
    .await
    .context("failed to read port allocations")?;

    for deployment in deployments.iter_mut() {
        deployment.source = sources
            .iter()
            .find(|row| row.deployment_id == deployment.id)
            .map(|row| DeploymentSource {
                repository: row.repository.clone(),
                git_ref: row.git_ref.clone(),
                subdirectory: row.subdirectory.clone(),
                credential_key: row.credential_key.clone(),
                builder: row.builder,
                dockerfile_path: row.dockerfile_path.clone(),
                install_command: row.install_command.clone(),
                build_command: row.build_command.clone(),
                start_command: row.start_command.clone(),
            });

        deployment.env = env
            .iter()
            .filter(|row| row.deployment_id == deployment.id)
            .map(|row| DeploymentEnv {
                name: row.name.clone(),
                scope: row.scope,
                value: row.value.clone(),
                parameter_key: row.parameter_key.clone(),
            })
            .collect();

        deployment.volumes = volumes
            .iter()
            .filter(|row| row.deployment_id == deployment.id)
            .map(|row| DeploymentVolume {
                container_path: row.container_path.clone(),
                volume_name: row.volume_name.clone(),
                read_only: row.read_only,
            })
            .collect();

        deployment.ports = ports
            .iter()
            .filter(|row| row.deployment_id.as_deref() == Some(deployment.id.as_str()))
            .map(|row| DeploymentPort {
                host_port: row.host_port,
                protocol: row.protocol,
                exposed: row.exposed,
            })
            .collect();
    }

    Ok(())
}

pub async fn list(pool: &SqlitePool) -> Result<Vec<Deployment>> {
    let rows = sqlx::query!(
        r#"select
            id as "id!", name as "name!",
            desired_state as "desired_state!: DesiredState",
            status as "status!: DeploymentStatus",
            last_error, desired_release_id, active_release_id,
            container_port, memory_limit_mb, cpu_shares,
            health_path, health_timeout_seconds as "health_timeout_seconds!",
            proxy_host_id, retained_releases as "retained_releases!",
            created_at as "created_at!", updated_at as "updated_at!"
        from deployments order by name"#
    )
    .fetch_all(pool)
    .await
    .context("failed to list deployments")?;

    let mut deployments: Vec<Deployment> = rows
        .into_iter()
        .map(|row| Deployment {
            id: row.id,
            name: row.name,
            desired_state: row.desired_state,
            status: row.status,
            last_error: row.last_error,
            desired_release_id: row.desired_release_id,
            active_release_id: row.active_release_id,
            container_port: row.container_port,
            memory_limit_mb: row.memory_limit_mb,
            cpu_shares: row.cpu_shares,
            health_path: row.health_path,
            health_timeout_seconds: row.health_timeout_seconds,
            proxy_host_id: row.proxy_host_id,
            retained_releases: row.retained_releases,
            created_at: row.created_at,
            updated_at: row.updated_at,
            source: None,
            env: Vec::new(),
            volumes: Vec::new(),
            ports: Vec::new(),
        })
        .collect();

    attach_children(pool, &mut deployments).await?;

    Ok(deployments)
}

pub async fn fetch(pool: &SqlitePool, id: &str) -> Result<Option<Deployment>> {
    Ok(list(pool).await?.into_iter().find(|d| d.id == id))
}

async fn write_children(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    id: &str,
    new: &NewDeployment,
) -> Result<()> {
    if let Some(source) = &new.source {
        sqlx::query!(
            r#"insert into deployment_sources (
                deployment_id, repository, git_ref, subdirectory, credential_key,
                builder, dockerfile_path, install_command, build_command, start_command
            ) values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)"#,
            id,
            source.repository,
            source.git_ref,
            source.subdirectory,
            source.credential_key,
            source.builder,
            source.dockerfile_path,
            source.install_command,
            source.build_command,
            source.start_command
        )
        .execute(&mut **tx)
        .await
        .context("failed to store the deployment source")?;
    }

    for env in &new.env {
        sqlx::query!(
            r#"insert into deployment_env (deployment_id, name, scope, value, parameter_key)
            values (?1, ?2, ?3, ?4, ?5)"#,
            id,
            env.name,
            env.scope,
            env.value,
            env.parameter_key
        )
        .execute(&mut **tx)
        .await
        .with_context(|| format!("failed to store env {}", env.name))?;
    }

    for volume in &new.volumes {
        sqlx::query!(
            r#"insert into deployment_volumes (deployment_id, container_path, volume_name, read_only)
            values (?1, ?2, ?3, ?4)"#,
            id,
            volume.container_path,
            volume.volume_name,
            volume.read_only
        )
        .execute(&mut **tx)
        .await
        .with_context(|| format!("failed to store volume {}", volume.container_path))?;
    }

    for port in &new.ports {
        sqlx::query!(
            r#"insert into port_allocations (host_port, protocol, deployment_id, exposed)
            values (?1, ?2, ?3, ?4)"#,
            port.host_port,
            port.protocol,
            id,
            port.exposed
        )
        .execute(&mut **tx)
        .await
        .with_context(|| format!("port {} is already allocated", port.host_port))?;
    }

    Ok(())
}

pub async fn create(pool: &SqlitePool, id: &str, new: &NewDeployment) -> Result<()> {
    let mut tx = pool.begin().await?;

    sqlx::query!(
        r#"insert into deployments (
            id, name, container_port, memory_limit_mb, cpu_shares,
            health_path, health_timeout_seconds, proxy_host_id, retained_releases
        ) values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)"#,
        id,
        new.name,
        new.container_port,
        new.memory_limit_mb,
        new.cpu_shares,
        new.health_path,
        new.health_timeout_seconds,
        new.proxy_host_id,
        new.retained_releases
    )
    .execute(&mut *tx)
    .await
    .with_context(|| format!("failed to create deployment {}", new.name))?;

    write_children(&mut tx, id, new).await?;
    tx.commit().await?;

    Ok(())
}

pub async fn update(pool: &SqlitePool, id: &str, new: &NewDeployment) -> Result<bool> {
    let mut tx = pool.begin().await?;

    let result = sqlx::query!(
        r#"update deployments set
            name = ?2, container_port = ?3, memory_limit_mb = ?4, cpu_shares = ?5,
            health_path = ?6, health_timeout_seconds = ?7, proxy_host_id = ?8,
            retained_releases = ?9, updated_at = unixepoch()
        where id = ?1"#,
        id,
        new.name,
        new.container_port,
        new.memory_limit_mb,
        new.cpu_shares,
        new.health_path,
        new.health_timeout_seconds,
        new.proxy_host_id,
        new.retained_releases
    )
    .execute(&mut *tx)
    .await
    .with_context(|| format!("failed to update deployment {id}"))?;

    if result.rows_affected() == 0 {
        return Ok(false);
    }

    sqlx::query!("delete from deployment_sources where deployment_id = ?", id)
        .execute(&mut *tx)
        .await?;
    sqlx::query!("delete from deployment_env where deployment_id = ?", id)
        .execute(&mut *tx)
        .await?;
    sqlx::query!("delete from deployment_volumes where deployment_id = ?", id)
        .execute(&mut *tx)
        .await?;
    sqlx::query!("delete from port_allocations where deployment_id = ?", id)
        .execute(&mut *tx)
        .await?;

    write_children(&mut tx, id, new).await?;
    tx.commit().await?;

    Ok(true)
}

pub async fn delete(pool: &SqlitePool, id: &str) -> Result<bool> {
    let mut tx = pool.begin().await?;

    sqlx::query!(
        "update deployments set desired_release_id = null, active_release_id = null where id = ?",
        id
    )
    .execute(&mut *tx)
    .await?;

    let result = sqlx::query!("delete from deployments where id = ?", id)
        .execute(&mut *tx)
        .await
        .with_context(|| format!("failed to delete deployment {id}"))?;

    tx.commit().await?;

    Ok(result.rows_affected() > 0)
}

pub async fn set_desired_state(pool: &SqlitePool, id: &str, state: DesiredState) -> Result<bool> {
    let result = sqlx::query!(
        "update deployments set desired_state = ?2, updated_at = unixepoch() where id = ?1",
        id,
        state
    )
    .execute(pool)
    .await
    .with_context(|| format!("failed to set the desired state of {id}"))?;

    Ok(result.rows_affected() > 0)
}

pub async fn set_status(
    pool: &SqlitePool,
    id: &str,
    status: DeploymentStatus,
    error: Option<&str>,
) -> Result<()> {
    sqlx::query!(
        "update deployments set status = ?2, last_error = ?3, updated_at = unixepoch() where id = ?1",
        id,
        status,
        error
    )
    .execute(pool)
    .await
    .with_context(|| format!("failed to set the status of {id}"))?;

    Ok(())
}

pub async fn set_active_release(pool: &SqlitePool, id: &str, release: Option<&str>) -> Result<()> {
    sqlx::query!(
        "update deployments set active_release_id = ?2, updated_at = unixepoch() where id = ?1",
        id,
        release
    )
    .execute(pool)
    .await
    .with_context(|| format!("failed to set the active release of {id}"))?;

    Ok(())
}

pub async fn set_desired_release(pool: &SqlitePool, id: &str, release: &str) -> Result<()> {
    sqlx::query!(
        "update deployments set desired_release_id = ?2, updated_at = unixepoch() where id = ?1",
        id,
        release
    )
    .execute(pool)
    .await
    .with_context(|| format!("failed to set the desired release of {id}"))?;

    Ok(())
}

pub async fn releases(pool: &SqlitePool, deployment_id: &str) -> Result<Vec<DeploymentRelease>> {
    sqlx::query_as!(
        DeploymentRelease,
        r#"select id as "id!", deployment_id as "deployment_id!", tag as "tag!",
            image as "image!", digest, source_ref, commit_sha, created_at as "created_at!"
        from deployment_releases where deployment_id = ?
        order by created_at desc, id desc"#,
        deployment_id
    )
    .fetch_all(pool)
    .await
    .context("failed to list releases")
}

pub async fn create_release(
    pool: &SqlitePool,
    id: &str,
    deployment_id: &str,
    tag: &str,
    image: &str,
    digest: Option<&str>,
    commit_sha: Option<&str>,
    source_ref: Option<&str>,
) -> Result<()> {
    sqlx::query!(
        r#"insert into deployment_releases
            (id, deployment_id, tag, image, digest, commit_sha, source_ref)
        values (?1, ?2, ?3, ?4, ?5, ?6, ?7)"#,
        id,
        deployment_id,
        tag,
        image,
        digest,
        commit_sha,
        source_ref
    )
    .execute(pool)
    .await
    .with_context(|| format!("failed to register release {tag}"))?;

    Ok(())
}

pub async fn prunable_releases(
    pool: &SqlitePool,
    deployment_id: &str,
) -> Result<Vec<DeploymentRelease>> {
    let all = releases(pool, deployment_id).await?;

    let keep = sqlx::query_scalar!(
        r#"select retained_releases as "retained_releases!" from deployments where id = ?"#,
        deployment_id
    )
    .fetch_optional(pool)
    .await?
    .unwrap_or(5) as usize;

    let pinned = sqlx::query!(
        "select desired_release_id, active_release_id from deployments where id = ?",
        deployment_id
    )
    .fetch_optional(pool)
    .await?;

    let (desired, active) = pinned
        .map(|row| (row.desired_release_id, row.active_release_id))
        .unwrap_or((None, None));

    Ok(all
        .into_iter()
        .skip(keep)
        .filter(|release| {
            Some(&release.id) != desired.as_ref() && Some(&release.id) != active.as_ref()
        })
        .collect())
}

pub async fn delete_release(pool: &SqlitePool, id: &str) -> Result<bool> {
    let result = sqlx::query!("delete from deployment_releases where id = ?", id)
        .execute(pool)
        .await
        .with_context(|| format!("failed to delete release {id}"))?;

    Ok(result.rows_affected() > 0)
}
