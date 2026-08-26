use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use http_body_util::{BodyExt, Full};
use hyper::{Request, body::Bytes};
use hyper_util::{client::legacy::Client, rt::TokioExecutor};
use tracing::debug;

use crate::infra::containers::docker::{DockerClient, ImageHealth};

const POLL_INTERVAL: Duration = Duration::from_millis(250);
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, Clone)]
pub struct HealthCheck {
    pub port: u16,
    pub path: Option<String>,
    pub timeout: Duration,
}

async fn probe(url: &str) -> Result<u16> {
    let client: Client<_, Full<Bytes>> = Client::builder(TokioExecutor::new()).build_http();

    let request = Request::get(url)
        .body(Full::new(Bytes::new()))
        .context("failed to build the health probe request")?;

    let response = tokio::time::timeout(PROBE_TIMEOUT, client.request(request))
        .await
        .context("health probe timed out")??;

    let status = response.status().as_u16();
    let _ = response.into_body().collect().await;

    Ok(status)
}

pub async fn wait_healthy(
    docker: &DockerClient,
    container: &str,
    check: &HealthCheck,
) -> Result<()> {
    let deadline = Instant::now() + check.timeout;
    let mut last;

    loop {
        match docker.inspect(container).await? {
            None => last = Some(format!("container {container} does not exist")),
            Some(status) if !status.running => {
                last = Some(match status.exit_code {
                    Some(code) => format!("container {container} exited with code {code}"),
                    None => format!("container {container} is not running"),
                });
            }
            Some(status) if status.health != ImageHealth::None => match status.health {
                ImageHealth::Healthy => return Ok(()),
                ImageHealth::Unhealthy => {
                    last = Some(format!("the image healthcheck for {container} is failing"));
                }
                _ => {
                    last = Some(format!(
                        "the image healthcheck for {container} is still starting"
                    ))
                }
            },
            Some(status) => match (&check.path, &status.ip) {
                (None, _) => return Ok(()),
                (Some(_), None) => {
                    last = Some(format!("container {container} has no address yet"));
                }
                (Some(path), Some(ip)) => {
                    let url = format!("http://{ip}:{}{path}", check.port);

                    match probe(&url).await {
                        Ok(code) if (200..400).contains(&code) => return Ok(()),
                        Ok(code) => last = Some(format!("{url} returned http {code}")),
                        Err(e) => last = Some(format!("{url} is not answering yet: {e}")),
                    }
                }
            },
        }

        if Instant::now() >= deadline {
            bail!(
                "{container} did not become healthy within {:?}: {}",
                check.timeout,
                last.unwrap_or_else(|| "no attempt was made".to_string())
            );
        }

        debug!(container, reason = last.as_deref(), "waiting for health");
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infra::containers::spec::{ContainerSpec, RestartPolicy};
    use bollard::models::{ContainerCreateBody, HealthConfig};

    fn enabled() -> bool {
        std::env::var("ARGES_DOCKER_LAB").is_ok()
    }

    fn check(path: Option<&str>, secs: u64) -> HealthCheck {
        HealthCheck {
            port: 80,
            path: path.map(str::to_string),
            timeout: Duration::from_secs(secs),
        }
    }

    async fn spawn(docker: &DockerClient, name: &str, image: &str) {
        let spec = ContainerSpec::new(name, image)
            .network("arges-health-lab")
            .restart(RestartPolicy::No);
        let _ = docker.stop_and_remove(name).await;
        docker.create_and_start(&spec).await.unwrap();
    }

    #[tokio::test]
    async fn a_container_on_the_bridge_answers_its_health_path() {
        if !enabled() {
            return;
        }

        let docker = DockerClient::connect().await.unwrap();
        docker.ensure_network("arges-health-lab").await.unwrap();

        spawn(&docker, "hc-ok", "traefik/whoami").await;

        wait_healthy(&docker, "hc-ok", &check(Some("/"), 20))
            .await
            .expect("a running container must be reachable from the host by bridge ip");

        let status = docker.inspect("hc-ok").await.unwrap().unwrap();
        assert!(
            status.ip.is_some(),
            "inspect must expose the bridge address"
        );

        docker.stop_and_remove("hc-ok").await.unwrap();
    }

    #[tokio::test]
    async fn a_404_path_never_becomes_healthy() {
        if !enabled() {
            return;
        }

        let docker = DockerClient::connect().await.unwrap();
        docker.ensure_network("arges-health-lab").await.unwrap();

        spawn(&docker, "hc-404", "nginx:alpine").await;

        let err = wait_healthy(&docker, "hc-404", &check(Some("/health"), 3))
            .await
            .unwrap_err()
            .to_string();

        assert!(err.contains("http 404"), "{err}");
        docker.stop_and_remove("hc-404").await.unwrap();
    }

    #[tokio::test]
    async fn without_a_path_running_is_enough() {
        if !enabled() {
            return;
        }

        let docker = DockerClient::connect().await.unwrap();
        docker.ensure_network("arges-health-lab").await.unwrap();

        spawn(&docker, "hc-nopath", "traefik/whoami").await;

        wait_healthy(&docker, "hc-nopath", &check(None, 10))
            .await
            .unwrap();

        docker.stop_and_remove("hc-nopath").await.unwrap();
    }

    #[tokio::test]
    async fn a_crashed_container_reports_its_exit_code() {
        if !enabled() {
            return;
        }

        let docker = DockerClient::connect().await.unwrap();
        docker.ensure_network("arges-health-lab").await.unwrap();

        let _ = docker.stop_and_remove("hc-crash").await;
        docker
            .create_and_start(
                &ContainerSpec::new("hc-crash", "alpine:3")
                    .network("arges-health-lab")
                    .restart(RestartPolicy::No),
            )
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_secs(1)).await;

        let err = wait_healthy(&docker, "hc-crash", &check(Some("/"), 3))
            .await
            .unwrap_err()
            .to_string();

        assert!(err.contains("exited"), "{err}");
        docker.stop_and_remove("hc-crash").await.unwrap();
    }

    #[tokio::test]
    async fn an_image_healthcheck_is_preferred_over_probing() {
        if !enabled() {
            return;
        }

        let docker = DockerClient::connect().await.unwrap();
        docker.ensure_network("arges-health-lab").await.unwrap();
        let _ = docker.stop_and_remove("hc-native").await;

        docker.pull_image("nginx:alpine").await.unwrap();
        let body = ContainerCreateBody {
            image: Some("nginx:alpine".to_string()),
            healthcheck: Some(HealthConfig {
                test: Some(vec![
                    "CMD-SHELL".to_string(),
                    "wget -q -O /dev/null http://localhost/ || exit 1".to_string(),
                ]),
                interval: Some(1_000_000_000),
                timeout: Some(1_000_000_000),
                retries: Some(3),
                start_period: Some(0),
                start_interval: None,
            }),
            ..Default::default()
        };
        docker.create_raw("hc-native", body).await.unwrap();

        wait_healthy(&docker, "hc-native", &check(Some("/never-used"), 30))
            .await
            .expect("the image healthcheck must decide, not the 404 path");

        assert_eq!(
            docker.inspect("hc-native").await.unwrap().unwrap().health,
            ImageHealth::Healthy
        );

        docker.stop_and_remove("hc-native").await.unwrap();
    }

    #[tokio::test]
    async fn a_missing_container_is_never_healthy() {
        if !enabled() {
            return;
        }

        let docker = DockerClient::connect().await.unwrap();

        let err = wait_healthy(&docker, "hc-ghost", &check(Some("/"), 2))
            .await
            .unwrap_err()
            .to_string();

        assert!(err.contains("does not exist"), "{err}");
    }
}
