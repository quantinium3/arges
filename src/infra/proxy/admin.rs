use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use http_body_util::{BodyExt, Full};
use hyper::{Request, body::Bytes, header::CONTENT_TYPE};
use hyper_util::{client::legacy::Client, rt::TokioExecutor};
use serde_json::Value;

#[derive(Clone)]
pub struct CaddyAdmin {
    base: String,
    client: Client<hyper_util::client::legacy::connect::HttpConnector, Full<Bytes>>,
}

impl CaddyAdmin {
    pub fn new(base: impl Into<String>) -> Self {
        Self {
            base: base.into(),
            client: Client::builder(TokioExecutor::new()).build_http(),
        }
    }

    async fn send(&self, request: Request<Full<Bytes>>) -> Result<(u16, String)> {
        let response = self
            .client
            .request(request)
            .await
            .context("failed to reach the caddy admin api")?;

        let status = response.status().as_u16();
        let body = response
            .into_body()
            .collect()
            .await
            .context("failed to read the caddy admin api response")?
            .to_bytes();

        Ok((status, String::from_utf8_lossy(&body).into_owned()))
    }

    pub async fn load(&self, config: &Value) -> Result<()> {
        let body = serde_json::to_vec(config).context("failed to serialise the caddy config")?;

        let request = Request::post(format!("{}/load", self.base))
            .header(CONTENT_TYPE, "application/json")
            .body(Full::new(Bytes::from(body)))
            .context("failed to build the caddy load request")?;

        let (status, body) = self.send(request).await?;

        if !(200..300).contains(&status) {
            bail!("caddy rejected the config (http {status}): {}", body.trim());
        }

        Ok(())
    }

    pub async fn wait_ready(&self, timeout: Duration) -> Result<()> {
        let deadline = Instant::now() + timeout;

        loop {
            if self.current().await.is_ok() {
                return Ok(());
            }
            if Instant::now() >= deadline {
                bail!(
                    "the caddy admin api at {} did not become reachable within {timeout:?}",
                    self.base
                );
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }

    pub async fn current(&self) -> Result<Value> {
        let request = Request::get(format!("{}/config/", self.base))
            .body(Full::new(Bytes::new()))
            .context("failed to build the caddy config request")?;

        let (status, body) = self.send(request).await?;

        if !(200..300).contains(&status) {
            bail!(
                "caddy returned http {status} for its config: {}",
                body.trim()
            );
        }

        serde_json::from_str(&body).context("caddy returned a config that is not valid json")
    }
}
