use std::{sync::Arc, time::Duration};

use anyhow::{Context, Result, bail};
use sqlx::SqlitePool;
use tokio::sync::Notify;
use tracing::{info, warn};

use crate::{
    constants::{CONTAINER_NETWORK_NAME, DEPLOYMENT_LABEL, DEPLOYMENT_RECONCILE_INTERVAL},
    db::queries::{
        audit,
        deployments::{
            self, Deployment, DeploymentRelease, DeploymentStatus, DesiredState, EnvScope,
        },
        parameters::{self, ParameterValue},
        proxy,
    },
    infra::{
        containers::{
            docker::DockerClient,
            health::{HealthCheck, wait_healthy},
            spec::{ContainerSpec, RestartPolicy},
        },
        parameters::secrets::MasterKey,
        proxy::{admin::CaddyAdmin, reconciler as proxy_reconciler},
    },
};

const SUBJECT: &str = "deployment";

pub fn container_name(deployment: &Deployment, release: &DeploymentRelease) -> String {
    format!("arges-{}-{}", deployment.name, release.tag)
}

async fn runtime_env(
    pool: &SqlitePool,
    key: &MasterKey,
    deployment: &Deployment,
) -> Result<Vec<String>> {
    let mut out = Vec::new();

    for env in &deployment.env {
        if env.scope == EnvScope::Build {
            continue;
        }

        let value = match (&env.value, &env.parameter_key) {
            (Some(value), _) => value.clone(),
            (None, Some(name)) => {
                let found = parameters::fetch(pool, name)
                    .await?
                    .with_context(|| format!("env {} references missing {name}", env.name))?;

                match found {
                    ParameterValue::String { value, .. } => value,
                    ParameterValue::Secure { version, value } => {
                        key.decrypt(name, version, &value)?.to_string()
                    }
                }
            }
            (None, None) => bail!("env {} has neither a value nor a reference", env.name),
        };

        out.push(format!("{}={value}", env.name));
    }

    Ok(out)
}

fn spec_for(
    deployment: &Deployment,
    release: &DeploymentRelease,
    env: Vec<String>,
) -> ContainerSpec {
    let mut spec = ContainerSpec::new(container_name(deployment, release), &release.image)
        .network(CONTAINER_NETWORK_NAME)
        .label(DEPLOYMENT_LABEL, &deployment.id)
        .env(env)
        .restart(RestartPolicy::UnlessStopped)
        .limits(deployment.memory_limit_mb, deployment.cpu_shares);

    for volume in &deployment.volumes {
        spec = if volume.read_only {
            spec.volume_ro(&volume.volume_name, &volume.container_path)
        } else {
            spec.volume(&volume.volume_name, &volume.container_path)
        };
    }

    for port in &deployment.ports {
        let host = port.host_port as u16;
        if let Some(container_port) = deployment.container_port {
            spec = spec.public_port(container_port as u16, host);
        }
    }

    spec
}

async fn sweep(docker: &DockerClient, deployment: &Deployment, keep: Option<&str>) -> Result<()> {
    let containers = docker
        .list_by_label(DEPLOYMENT_LABEL, &deployment.id)
        .await?;

    for name in containers {
        if Some(name.as_str()) == keep {
            continue;
        }

        docker
            .stop_and_remove(&name)
            .await
            .with_context(|| format!("failed to remove the superseded container {name}"))?;

        info!(deployment = %deployment.name, container = %name, "removed superseded container");
    }

    Ok(())
}

async fn point_proxy_at(
    pool: &SqlitePool,
    key: &MasterKey,
    caddy: &CaddyAdmin,
    deployment: &Deployment,
    container: &str,
) -> Result<()> {
    let Some(proxy_host_id) = &deployment.proxy_host_id else {
        return Ok(());
    };

    let Some(host) = proxy::fetch(pool, proxy_host_id).await? else {
        warn!(deployment = %deployment.name, "the linked proxy host no longer exists");
        return Ok(());
    };

    if host.upstream_container.as_deref() == Some(container) {
        return Ok(());
    }

    proxy::set_upstream_container(pool, proxy_host_id, container).await?;
    proxy_reconciler::apply(pool, key, caddy).await?;

    Ok(())
}

async fn deploy(
    pool: &SqlitePool,
    key: &MasterKey,
    docker: &DockerClient,
    caddy: &CaddyAdmin,
    deployment: &Deployment,
    release: &DeploymentRelease,
) -> Result<()> {
    let name = container_name(deployment, release);

    deployments::set_status(pool, &deployment.id, DeploymentStatus::Deploying, None).await?;

    let env = runtime_env(pool, key, deployment).await?;
    let spec = spec_for(deployment, release, env);

    if !deployment.ports.is_empty() {
        sweep(docker, deployment, None).await?;
    }

    docker.ensure_running(&spec).await?;

    let check = HealthCheck {
        port: deployment.container_port.unwrap_or(80) as u16,
        path: deployment.health_path.clone(),
        timeout: Duration::from_secs(deployment.health_timeout_seconds.max(1) as u64),
    };
    wait_healthy(docker, &name, &check).await?;

    point_proxy_at(pool, key, caddy, deployment, &name).await?;

    deployments::set_active_release(pool, &deployment.id, Some(&release.id)).await?;
    sweep(docker, deployment, Some(&name)).await?;

    deployments::set_status(pool, &deployment.id, DeploymentStatus::Running, None).await?;
    audit::record(
        pool,
        SUBJECT,
        Some(&deployment.id),
        "deployed",
        Some(&release.tag),
    )
    .await?;

    info!(deployment = %deployment.name, release = %release.tag, "deployed");

    Ok(())
}

pub async fn converge_one(
    pool: &SqlitePool,
    key: &MasterKey,
    docker: &DockerClient,
    caddy: &CaddyAdmin,
    deployment: &Deployment,
) -> Result<()> {
    if deployment.desired_state == DesiredState::Stopped {
        sweep(docker, deployment, None).await?;
        deployments::set_active_release(pool, &deployment.id, None).await?;
        deployments::set_status(pool, &deployment.id, DeploymentStatus::Stopped, None).await?;
        return Ok(());
    }

    let Some(desired_id) = &deployment.desired_release_id else {
        deployments::set_status(pool, &deployment.id, DeploymentStatus::Pending, None).await?;
        return Ok(());
    };

    let releases = deployments::releases(pool, &deployment.id).await?;
    let release = releases
        .iter()
        .find(|r| &r.id == desired_id)
        .with_context(|| format!("desired release {desired_id} is missing"))?;

    let name = container_name(deployment, release);
    let settled = deployment.active_release_id.as_ref() == Some(desired_id);
    let running = docker
        .inspect(&name)
        .await?
        .is_some_and(|status| status.running);

    if settled && running {
        sweep(docker, deployment, Some(&name)).await?;
        if deployment.status != DeploymentStatus::Running {
            deployments::set_status(pool, &deployment.id, DeploymentStatus::Running, None).await?;
        }
        return Ok(());
    }

    deploy(pool, key, docker, caddy, deployment, release).await
}

pub async fn converge_all(
    pool: &SqlitePool,
    key: &MasterKey,
    docker: &DockerClient,
    caddy: &CaddyAdmin,
) -> Result<()> {
    for deployment in deployments::list(pool).await? {
        if let Err(e) = converge_one(pool, key, docker, caddy, &deployment).await {
            let reason = format!("{e:#}");
            warn!(deployment = %deployment.name, error = %reason, "deployment converge failed");

            let _ = deployments::set_status(
                pool,
                &deployment.id,
                DeploymentStatus::Failed,
                Some(&reason),
            )
            .await;
            let _ = audit::record(
                pool,
                SUBJECT,
                Some(&deployment.id),
                "deploy_failed",
                Some(&reason),
            )
            .await;
        }
    }

    Ok(())
}

pub fn init(
    pool: SqlitePool,
    key: Arc<MasterKey>,
    docker: DockerClient,
    caddy: CaddyAdmin,
    notify: Arc<Notify>,
) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(DEPLOYMENT_RECONCILE_INTERVAL);

        loop {
            if let Err(e) = converge_all(&pool, &key, &docker, &caddy).await {
                warn!(error = ?e, "deployment reconcile failed");
            }

            tokio::select! {
                _ = notify.notified() => {}
                _ = ticker.tick() => {}
            }
        }
    });
}
