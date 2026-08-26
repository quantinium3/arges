use anyhow::{Context, Result, bail};
use http_body_util::{BodyExt, Full};
use hyper::{Request, body::Bytes, header::ACCEPT};
use hyper_util::{client::legacy::Client, rt::TokioExecutor};

const MANIFEST_TYPES: &str = "application/vnd.oci.image.manifest.v1+json, \
application/vnd.oci.image.index.v1+json, \
application/vnd.docker.distribution.manifest.v2+json, \
application/vnd.docker.distribution.manifest.list.v2+json";

#[derive(Clone)]
pub struct RegistryClient {
    base: String,
}

impl RegistryClient {
    pub fn new(base: impl Into<String>) -> Self {
        Self { base: base.into() }
    }

    pub async fn manifest_digest(&self, repository: &str, tag: &str) -> Result<Option<String>> {
        let client: Client<_, Full<Bytes>> = Client::builder(TokioExecutor::new()).build_http();

        let request = Request::head(format!("{}/v2/{repository}/manifests/{tag}", self.base))
            .header(ACCEPT, MANIFEST_TYPES)
            .body(Full::new(Bytes::new()))
            .context("failed to build the manifest request")?;

        let response = client
            .request(request)
            .await
            .context("failed to reach the local registry")?;

        let status = response.status().as_u16();

        let digest = response
            .headers()
            .get("docker-content-digest")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);

        let _ = response.into_body().collect().await;

        match status {
            200 => Ok(digest),
            404 => Ok(None),
            other => bail!("the registry returned http {other} for {repository}:{tag}"),
        }
    }

    pub async fn delete_manifest(&self, repository: &str, digest: &str) -> Result<()> {
        let client: Client<_, Full<Bytes>> = Client::builder(TokioExecutor::new()).build_http();

        let request = Request::delete(format!("{}/v2/{repository}/manifests/{digest}", self.base))
            .header(ACCEPT, MANIFEST_TYPES)
            .body(Full::new(Bytes::new()))
            .context("failed to build the manifest delete request")?;

        let response = client
            .request(request)
            .await
            .context("failed to reach the local registry")?;

        let status = response.status().as_u16();
        let _ = response.into_body().collect().await;

        match status {
            202 | 404 => Ok(()),
            other => bail!("the registry refused to delete {repository}@{digest}: http {other}"),
        }
    }
}
