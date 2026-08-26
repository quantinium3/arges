use axum::extract::{Json, Path, State};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    constants::REGISTRY_PORT,
    db::queries::{
        audit,
        deployments::{
            self, Builder, Deployment, DeploymentEnv, DeploymentPort, DeploymentRelease,
            DeploymentSource, DeploymentVolume, DesiredState, EnvScope, Protocol,
        },
    },
    infra::deployments::retention,
    state::AppState,
    utils::api_response::{ApiError, ApiResponse},
};

const SUBJECT: &str = "deployment";
const MAX_NAME_LEN: usize = 63;

#[derive(Deserialize)]
pub struct SourceRequest {
    pub repository: String,
    pub git_ref: Option<String>,
    pub subdirectory: Option<String>,
    pub credential_key: Option<String>,
    pub builder: Option<Builder>,
    pub dockerfile_path: Option<String>,
    pub install_command: Option<String>,
    pub build_command: Option<String>,
    pub start_command: Option<String>,
}

#[derive(Deserialize)]
pub struct EnvRequest {
    pub name: String,
    pub scope: Option<EnvScope>,
    pub value: Option<String>,
    pub parameter_key: Option<String>,
}

#[derive(Deserialize)]
pub struct VolumeRequest {
    pub container_path: String,
    pub volume_name: Option<String>,
    pub read_only: Option<bool>,
}

#[derive(Deserialize)]
pub struct PortRequest {
    pub host_port: i64,
    pub protocol: Option<Protocol>,
    pub exposed: Option<bool>,
}

#[derive(Deserialize)]
pub struct DeploymentRequest {
    pub name: String,
    pub container_port: Option<i64>,
    pub memory_limit_mb: Option<i64>,
    pub cpu_shares: Option<i64>,
    pub health_path: Option<String>,
    pub health_timeout_seconds: Option<i64>,
    pub proxy_host_id: Option<String>,
    pub retained_releases: Option<i64>,
    pub source: Option<SourceRequest>,
    pub env: Option<Vec<EnvRequest>>,
    pub volumes: Option<Vec<VolumeRequest>>,
    pub ports: Option<Vec<PortRequest>>,
}

#[derive(Deserialize)]
pub struct ReleaseRequest {
    pub tag: String,
    pub commit_sha: Option<String>,
    pub source_ref: Option<String>,
    pub deploy: Option<bool>,
}

#[derive(Serialize)]
pub struct ReleaseResponse {
    pub release: DeploymentRelease,
    pub deploying: bool,
}

fn validate_name(name: &str) -> Result<String, ApiError> {
    let name = name.trim().to_lowercase();

    if name.is_empty() || name.len() > MAX_NAME_LEN {
        return Err(ApiError::bad_request(format!(
            "name must be between 1 and {MAX_NAME_LEN} characters"
        )));
    }
    if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        return Err(ApiError::bad_request(
            "name may only contain lowercase letters, digits and dashes",
        ));
    }
    if name.starts_with('-') || name.ends_with('-') {
        return Err(ApiError::bad_request(
            "name may not start or end with a dash",
        ));
    }

    Ok(name)
}

fn to_new(request: DeploymentRequest) -> Result<deployments::NewDeployment, ApiError> {
    let name = validate_name(&request.name)?;

    let mut env = Vec::new();
    for item in request.env.unwrap_or_default() {
        if item.value.is_some() == item.parameter_key.is_some() {
            return Err(ApiError::bad_request(format!(
                "env {} needs exactly one of value or parameter_key",
                item.name
            )));
        }
        env.push(DeploymentEnv {
            name: item.name,
            scope: item.scope.unwrap_or(EnvScope::Runtime),
            value: item.value,
            parameter_key: item.parameter_key,
        });
    }

    let volumes = request
        .volumes
        .unwrap_or_default()
        .into_iter()
        .map(|item| DeploymentVolume {
            volume_name: item.volume_name.unwrap_or_else(|| {
                format!(
                    "arges-{name}-{}",
                    item.container_path.trim_matches('/').replace('/', "-")
                )
            }),
            container_path: item.container_path,
            read_only: item.read_only.unwrap_or(false),
        })
        .collect();

    let ports = request
        .ports
        .unwrap_or_default()
        .into_iter()
        .map(|item| DeploymentPort {
            host_port: item.host_port,
            protocol: item.protocol.unwrap_or(Protocol::Tcp),
            exposed: item.exposed.unwrap_or(true),
        })
        .collect();

    let source = request.source.map(|item| DeploymentSource {
        repository: item.repository,
        git_ref: item.git_ref.unwrap_or_else(|| "main".to_string()),
        subdirectory: item.subdirectory,
        credential_key: item.credential_key,
        builder: item.builder.unwrap_or(Builder::Railpack),
        dockerfile_path: item.dockerfile_path,
        install_command: item.install_command,
        build_command: item.build_command,
        start_command: item.start_command,
    });

    if request.proxy_host_id.is_some() && request.container_port.is_none() {
        return Err(ApiError::bad_request(
            "a proxy-backed deployment needs a container_port",
        ));
    }
    if request.health_path.is_some() && request.container_port.is_none() {
        return Err(ApiError::bad_request(
            "a health_path needs a container_port to probe",
        ));
    }

    Ok(deployments::NewDeployment {
        name,
        container_port: request.container_port,
        memory_limit_mb: request.memory_limit_mb,
        cpu_shares: request.cpu_shares,
        health_path: request.health_path,
        health_timeout_seconds: request.health_timeout_seconds.unwrap_or(30),
        proxy_host_id: request.proxy_host_id,
        retained_releases: request.retained_releases.unwrap_or(5),
        source,
        env,
        volumes,
        ports,
    })
}

fn root_cause(error: &anyhow::Error) -> String {
    error
        .chain()
        .last()
        .map_or_else(|| error.to_string(), |e| e.to_string())
}

async fn load(state: &AppState, id: &str) -> Result<Deployment, ApiError> {
    deployments::fetch(&state.db, id)
        .await
        .map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::not_found(format!("deployment {id} not found")))
}

pub async fn list_deployments(
    State(state): State<AppState>,
) -> Result<ApiResponse<Vec<Deployment>>, ApiError> {
    let all = deployments::list(&state.db)
        .await
        .map_err(ApiError::internal)?;

    Ok(ApiResponse::ok(all, "deployments fetched"))
}

pub async fn get_deployment(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<ApiResponse<Deployment>, ApiError> {
    Ok(ApiResponse::ok(
        load(&state, &id).await?,
        "deployment fetched",
    ))
}

pub async fn create_deployment(
    State(state): State<AppState>,
    Json(request): Json<DeploymentRequest>,
) -> Result<ApiResponse<Deployment>, ApiError> {
    let new = to_new(request)?;
    let id = Uuid::new_v4().to_string();

    deployments::create(&state.db, &id, &new)
        .await
        .map_err(|e| {
            ApiError::conflict(format!(
                "could not create the deployment: {}",
                root_cause(&e)
            ))
        })?;

    audit::record(&state.db, SUBJECT, Some(&id), "created", Some(&new.name))
        .await
        .map_err(ApiError::internal)?;

    Ok(ApiResponse::ok(
        load(&state, &id).await?,
        "deployment created",
    ))
}

pub async fn update_deployment(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(request): Json<DeploymentRequest>,
) -> Result<ApiResponse<Deployment>, ApiError> {
    let new = to_new(request)?;

    let found = deployments::update(&state.db, &id, &new)
        .await
        .map_err(|e| {
            ApiError::conflict(format!(
                "could not update the deployment: {}",
                root_cause(&e)
            ))
        })?;

    if !found {
        return Err(ApiError::not_found(format!("deployment {id} not found")));
    }

    audit::record(&state.db, SUBJECT, Some(&id), "updated", Some(&new.name))
        .await
        .map_err(ApiError::internal)?;

    Ok(ApiResponse::ok(
        load(&state, &id).await?,
        "deployment updated",
    ))
}

pub async fn delete_deployment(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<ApiResponse<()>, ApiError> {
    if !deployments::delete(&state.db, &id)
        .await
        .map_err(ApiError::internal)?
    {
        return Err(ApiError::not_found(format!("deployment {id} not found")));
    }

    audit::record(&state.db, SUBJECT, Some(&id), "deleted", None)
        .await
        .map_err(ApiError::internal)?;

    Ok(ApiResponse::ok((), "deployment deleted"))
}

pub async fn list_releases(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<ApiResponse<Vec<DeploymentRelease>>, ApiError> {
    load(&state, &id).await?;

    let all = deployments::releases(&state.db, &id)
        .await
        .map_err(ApiError::internal)?;

    Ok(ApiResponse::ok(all, "releases fetched"))
}

pub async fn register_release(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(request): Json<ReleaseRequest>,
) -> Result<ApiResponse<ReleaseResponse>, ApiError> {
    let deployment = load(&state, &id).await?;
    let tag = request.tag.trim().to_string();

    if tag.is_empty() {
        return Err(ApiError::bad_request("a release needs a tag"));
    }

    let digest = state
        .registry
        .manifest_digest(&deployment.name, &tag)
        .await
        .map_err(|e| ApiError::bad_gateway(root_cause(&e)))?
        .ok_or_else(|| {
            ApiError::bad_request(format!(
                "{}:{tag} is not in the registry, push the image before registering it",
                deployment.name
            ))
        })?;

    let release_id = Uuid::new_v4().to_string();
    let image = format!("localhost:{REGISTRY_PORT}/{}:{tag}", deployment.name);

    deployments::create_release(
        &state.db,
        &release_id,
        &id,
        &tag,
        &image,
        Some(&digest),
        request.commit_sha.as_deref(),
        request.source_ref.as_deref(),
    )
    .await
    .map_err(|e| {
        ApiError::conflict(format!(
            "could not register the release: {}",
            root_cause(&e)
        ))
    })?;

    let deploying = request.deploy.unwrap_or(true);
    if deploying {
        deployments::set_desired_release(&state.db, &id, &release_id)
            .await
            .map_err(ApiError::internal)?;
        state.deploy_notify.notify_one();
    }

    audit::record(
        &state.db,
        SUBJECT,
        Some(&id),
        "release_registered",
        Some(&tag),
    )
    .await
    .map_err(ApiError::internal)?;

    let release = deployments::releases(&state.db, &id)
        .await
        .map_err(ApiError::internal)?
        .into_iter()
        .find(|r| r.id == release_id)
        .ok_or_else(|| ApiError::internal(anyhow::anyhow!("release vanished after insert")))?;

    Ok(ApiResponse::ok(
        ReleaseResponse { release, deploying },
        if deploying {
            "release registered and queued for deployment"
        } else {
            "release registered"
        },
    ))
}

#[derive(Serialize)]
pub struct RetentionResponse {
    pub pruned_releases: usize,
    pub garbage_collected: bool,
}

pub async fn run_retention(
    State(state): State<AppState>,
) -> Result<ApiResponse<RetentionResponse>, ApiError> {
    let docker = state.docker.as_ref().ok_or_else(|| {
        ApiError::unavailable("docker is not available on this host, retention cannot run")
    })?;

    let pruned = retention::run(&state.db, &state.registry, docker)
        .await
        .map_err(ApiError::internal)?;

    Ok(ApiResponse::ok(
        RetentionResponse {
            pruned_releases: pruned.releases,
            garbage_collected: pruned.collected,
        },
        "retention complete",
    ))
}

async fn set_state(
    state: AppState,
    id: &str,
    desired: DesiredState,
) -> Result<ApiResponse<Deployment>, ApiError> {
    if !deployments::set_desired_state(&state.db, id, desired)
        .await
        .map_err(ApiError::internal)?
    {
        return Err(ApiError::not_found(format!("deployment {id} not found")));
    }

    let action = match desired {
        DesiredState::Running => "started",
        DesiredState::Stopped => "stopped",
    };
    audit::record(&state.db, SUBJECT, Some(id), action, None)
        .await
        .map_err(ApiError::internal)?;

    state.deploy_notify.notify_one();

    Ok(ApiResponse::ok(
        load(&state, id).await?,
        "deployment updated",
    ))
}

pub async fn start_deployment(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<ApiResponse<Deployment>, ApiError> {
    set_state(state, &id, DesiredState::Running).await
}

pub async fn stop_deployment(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<ApiResponse<Deployment>, ApiError> {
    set_state(state, &id, DesiredState::Stopped).await
}

pub async fn rollback_deployment(
    State(state): State<AppState>,
    Path((id, release_id)): Path<(String, String)>,
) -> Result<ApiResponse<Deployment>, ApiError> {
    load(&state, &id).await?;

    let exists = deployments::releases(&state.db, &id)
        .await
        .map_err(ApiError::internal)?
        .into_iter()
        .any(|r| r.id == release_id);

    if !exists {
        return Err(ApiError::not_found(format!(
            "release {release_id} does not belong to deployment {id}"
        )));
    }

    deployments::set_desired_release(&state.db, &id, &release_id)
        .await
        .map_err(ApiError::internal)?;

    audit::record(
        &state.db,
        SUBJECT,
        Some(&id),
        "rolled_back",
        Some(&release_id),
    )
    .await
    .map_err(ApiError::internal)?;

    state.deploy_notify.notify_one();

    Ok(ApiResponse::ok(load(&state, &id).await?, "rollback queued"))
}

#[cfg(test)]
pub mod tests_support {
    pub use super::tests::harness;
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::{
        body::{Body, to_bytes},
        http::{Request, StatusCode},
    };
    use serde_json::{Value, json};
    use sqlx::SqlitePool;
    use tokio::sync::Notify;
    use tower::ServiceExt;

    use super::*;
    use crate::{
        constants::{CADDY_ADMIN_URL, REGISTRY_URL},
        infra::{
            containers::registry::RegistryClient, packages::package_manager::PackageManager,
            parameters::secrets::MasterKey, proxy::admin::CaddyAdmin,
        },
        router,
    };

    pub struct Harness {
        router: axum::Router,
        dir: std::path::PathBuf,
    }

    pub async fn harness(pool: SqlitePool) -> Harness {
        Harness::new(pool).await
    }

    impl Harness {
        pub async fn new(pool: SqlitePool) -> Self {
            use std::os::unix::fs::PermissionsExt;

            let dir = std::env::temp_dir().join(format!("arges-dep-{}", Uuid::new_v4()));
            std::fs::create_dir_all(&dir).unwrap();
            let path = dir.join("master.key");
            let mut bytes = [0u8; 32];
            getrandom::fill(&mut bytes).unwrap();
            std::fs::write(&path, bytes).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();

            let state = AppState {
                db: pool,
                package_manager: PackageManager::APT,
                reconcile_notify: Arc::new(Notify::new()),
                deploy_notify: Arc::new(Notify::new()),
                retention_notify: Arc::new(Notify::new()),
                master_key: Arc::new(MasterKey::load(&path).await.unwrap()),
                docker: None,
                caddy: CaddyAdmin::new(CADDY_ADMIN_URL),
                registry: RegistryClient::new(REGISTRY_URL),
            };

            Self {
                router: router::routes(state),
                dir,
            }
        }

        pub async fn send(
            &self,
            method: &str,
            uri: &str,
            body: Option<Value>,
        ) -> (StatusCode, Value) {
            let request = Request::builder().method(method).uri(uri);
            let request = match body {
                Some(body) => request
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
                None => request.body(Body::empty()).unwrap(),
            };

            let response = self.router.clone().oneshot(request).await.unwrap();
            let status = response.status();
            let bytes = to_bytes(response.into_body(), 256 * 1024).await.unwrap();

            (status, serde_json::from_slice(&bytes).unwrap())
        }
    }

    impl Drop for Harness {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn basic(name: &str) -> Value {
        json!({ "name": name, "container_port": 3000 })
    }

    #[sqlx::test]
    async fn a_deployment_is_created_with_its_children(pool: SqlitePool) {
        let h = Harness::new(pool).await;

        let (status, body) = h
            .send(
                "POST",
                "/api/deployment",
                Some(json!({
                    "name": "My-App ",
                    "container_port": 3000,
                    "memory_limit_mb": 512,
                    "source": { "repository": "https://github.com/me/app.git", "builder": "railpack" },
                    "env": [
                        { "name": "PORT", "value": "3000" },
                        { "name": "DB_PASSWORD", "parameter_key": "/app/db", "scope": "runtime" },
                        { "name": "NEXT_PUBLIC_URL", "value": "https://x.test", "scope": "build" }
                    ],
                    "volumes": [ { "container_path": "/data" } ],
                    "ports": [ { "host_port": 8080 } ]
                })),
            )
            .await;

        assert_eq!(status, StatusCode::OK, "{body}");
        let d = &body["data"];
        assert_eq!(d["name"], "my-app", "name is normalised");
        assert_eq!(d["status"], "pending");
        assert_eq!(d["desired_state"], "running");
        assert!(d["desired_release_id"].is_null());
        assert_eq!(d["source"]["builder"], "railpack");
        assert_eq!(d["source"]["git_ref"], "main");
        assert_eq!(d["env"].as_array().unwrap().len(), 3);
        assert_eq!(d["volumes"][0]["volume_name"], "arges-my-app-data");
        assert_eq!(d["ports"][0]["host_port"], 8080);
        assert_eq!(d["ports"][0]["protocol"], "tcp");
    }

    #[sqlx::test]
    async fn invalid_deployments_are_rejected(pool: SqlitePool) {
        let h = Harness::new(pool).await;

        let cases = [
            (json!({ "name": "" }), "empty name"),
            (json!({ "name": "my_app" }), "underscore in name"),
            (json!({ "name": "-app" }), "leading dash"),
            (
                json!({ "name": "a", "proxy_host_id": "p1" }),
                "proxy without container port",
            ),
            (
                json!({ "name": "a", "health_path": "/h" }),
                "health path without container port",
            ),
            (
                json!({ "name": "a", "env": [{ "name": "X", "value": "1", "parameter_key": "/a" }] }),
                "env with both value and reference",
            ),
            (
                json!({ "name": "a", "env": [{ "name": "X" }] }),
                "env with neither",
            ),
        ];

        for (body, label) in cases {
            let (status, response) = h.send("POST", "/api/deployment", Some(body)).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{label} -> {response}");
        }
    }

    #[sqlx::test]
    async fn a_duplicate_host_port_is_a_conflict(pool: SqlitePool) {
        let h = Harness::new(pool).await;
        let with_port = |n: &str| json!({ "name": n, "ports": [{ "host_port": 9100 }] });

        let (status, _) = h
            .send("POST", "/api/deployment", Some(with_port("one")))
            .await;
        assert_eq!(status, StatusCode::OK);

        let (status, body) = h
            .send("POST", "/api/deployment", Some(with_port("two")))
            .await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
    }

    #[sqlx::test]
    async fn a_release_needs_the_image_to_exist_in_the_registry(pool: SqlitePool) {
        let h = Harness::new(pool).await;
        let (_, created) = h.send("POST", "/api/deployment", Some(basic("web"))).await;
        let id = created["data"]["id"].as_str().unwrap().to_string();

        let (status, body) = h
            .send(
                "POST",
                &format!("/api/deployment/{id}/release"),
                Some(json!({ "tag": "build-001" })),
            )
            .await;

        assert!(
            status == StatusCode::BAD_GATEWAY || status == StatusCode::BAD_REQUEST,
            "with no registry running this must fail rather than register a phantom release: {status} {body}"
        );

        let (_, releases) = h
            .send("GET", &format!("/api/deployment/{id}/release"), None)
            .await;
        assert_eq!(releases["data"].as_array().unwrap().len(), 0);
    }

    #[sqlx::test]
    async fn start_and_stop_move_the_desired_state(pool: SqlitePool) {
        let h = Harness::new(pool).await;
        let (_, created) = h.send("POST", "/api/deployment", Some(basic("web"))).await;
        let id = created["data"]["id"].as_str().unwrap().to_string();

        let (_, body) = h
            .send("POST", &format!("/api/deployment/{id}/stop"), None)
            .await;
        assert_eq!(body["data"]["desired_state"], "stopped");

        let (_, body) = h
            .send("POST", &format!("/api/deployment/{id}/start"), None)
            .await;
        assert_eq!(body["data"]["desired_state"], "running");
    }

    #[sqlx::test]
    async fn updating_replaces_the_child_rows(pool: SqlitePool) {
        let h = Harness::new(pool).await;
        let (_, created) = h
            .send(
                "POST",
                "/api/deployment",
                Some(json!({
                    "name": "web", "container_port": 3000,
                    "env": [{ "name": "A", "value": "1" }],
                    "ports": [{ "host_port": 9200 }]
                })),
            )
            .await;
        let id = created["data"]["id"].as_str().unwrap().to_string();

        let (status, body) = h
            .send(
                "PUT",
                &format!("/api/deployment/{id}"),
                Some(json!({
                    "name": "web", "container_port": 4000,
                    "env": [{ "name": "B", "parameter_key": "/b" }]
                })),
            )
            .await;

        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["data"]["container_port"], 4000);
        assert_eq!(body["data"]["env"].as_array().unwrap().len(), 1);
        assert_eq!(body["data"]["env"][0]["name"], "B");
        assert_eq!(
            body["data"]["ports"].as_array().unwrap().len(),
            0,
            "the old port allocation must be released"
        );
    }

    #[sqlx::test]
    async fn rollback_rejects_a_release_from_another_deployment(pool: SqlitePool) {
        let h = Harness::new(pool).await;
        let (_, created) = h.send("POST", "/api/deployment", Some(basic("web"))).await;
        let id = created["data"]["id"].as_str().unwrap().to_string();

        let (status, _) = h
            .send("POST", &format!("/api/deployment/{id}/rollback/nope"), None)
            .await;

        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[sqlx::test]
    async fn a_missing_deployment_is_a_404(pool: SqlitePool) {
        let h = Harness::new(pool).await;

        for (method, uri) in [
            ("GET", "/api/deployment/ghost"),
            ("DELETE", "/api/deployment/ghost"),
            ("POST", "/api/deployment/ghost/start"),
            ("GET", "/api/deployment/ghost/release"),
        ] {
            assert_eq!(
                h.send(method, uri, None).await.0,
                StatusCode::NOT_FOUND,
                "{uri}"
            );
        }
    }
}

#[cfg(test)]
mod live {
    use super::tests_support::*;
    use axum::http::StatusCode;
    use serde_json::json;
    use sqlx::SqlitePool;

    fn enabled() -> bool {
        std::env::var("ARGES_REGISTRY_LAB").is_ok()
    }

    #[sqlx::test]
    async fn a_pushed_image_registers_with_its_digest(pool: SqlitePool) {
        if !enabled() {
            return;
        }

        let h = harness(pool).await;
        let (_, created) = h
            .send(
                "POST",
                "/api/deployment",
                Some(json!({ "name": "demo-app" })),
            )
            .await;
        let id = created["data"]["id"].as_str().unwrap().to_string();

        let (status, body) = h
            .send(
                "POST",
                &format!("/api/deployment/{id}/release"),
                Some(json!({ "tag": "build-001", "commit_sha": "a1b2c3d4e5f" })),
            )
            .await;

        assert_eq!(status, StatusCode::OK, "{body}");
        let release = &body["data"]["release"];
        assert_eq!(release["tag"], "build-001");
        assert_eq!(release["image"], "localhost:5000/demo-app:build-001");
        assert!(
            release["digest"].as_str().unwrap().starts_with("sha256:"),
            "the digest must come from the registry"
        );
        assert_eq!(body["data"]["deploying"], true);

        let (_, after) = h.send("GET", &format!("/api/deployment/{id}"), None).await;
        assert_eq!(after["data"]["desired_release_id"], release["id"]);
    }

    #[sqlx::test]
    async fn an_unpushed_tag_is_refused(pool: SqlitePool) {
        if !enabled() {
            return;
        }

        let h = harness(pool).await;
        let (_, created) = h
            .send(
                "POST",
                "/api/deployment",
                Some(json!({ "name": "demo-app" })),
            )
            .await;
        let id = created["data"]["id"].as_str().unwrap().to_string();

        let (status, body) = h
            .send(
                "POST",
                &format!("/api/deployment/{id}/release"),
                Some(json!({ "tag": "never-pushed" })),
            )
            .await;

        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert!(
            body["message"]
                .as_str()
                .unwrap()
                .contains("not in the registry")
        );
    }
}
