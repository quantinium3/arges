use axum::extract::{Json, Path, Query, State};
use serde::{Deserialize, Serialize};

use crate::{
    db::queries::parameters::{self, Parameter, ParameterType, ParameterValue},
    state::AppState,
    utils::api_response::{ApiError, ApiResponse},
};

const MAX_KEY_LEN: usize = 512;
const MAX_VALUE_LEN: usize = 8 * 1024;

#[derive(Deserialize)]
pub struct ListQuery {
    pub prefix: Option<String>,
}

#[derive(Deserialize)]
pub struct GetQuery {
    pub decrypt: Option<bool>,
}

#[derive(Deserialize)]
pub struct PutParameterRequest {
    pub r#type: ParameterType,
    pub value: String,
}

#[derive(Serialize)]
pub struct ParameterSummary {
    pub key: String,
    pub r#type: ParameterType,
    pub version: i64,
    pub created_at: i64,
    pub updated_at: i64,
}

impl From<Parameter> for ParameterSummary {
    fn from(param: Parameter) -> Self {
        Self {
            key: param.key,
            r#type: param.kind,
            version: param.version,
            created_at: param.created_at,
            updated_at: param.updated_at,
        }
    }
}

#[derive(Serialize)]
pub struct ParameterResponse {
    pub key: String,
    pub r#type: ParameterType,
    pub version: i64,
    pub value: Option<String>,
}

#[derive(Serialize)]
pub struct PutResponse {
    pub key: String,
    pub version: i64,
}

fn normalize(key: &str) -> Result<String, ApiError> {
    let key = format!("/{}", key.trim_start_matches('/'));

    if key.len() > MAX_KEY_LEN {
        return Err(ApiError::bad_request(format!(
            "key must be at most {MAX_KEY_LEN} bytes, got {}",
            key.len()
        )));
    }
    if key == "/" || key.ends_with('/') || key.contains("//") {
        return Err(ApiError::bad_request(
            "key must look like /a/b: no empty segments and no trailing slash",
        ));
    }
    if key.chars().any(|c| c.is_control()) {
        return Err(ApiError::bad_request(
            "key must not contain control characters",
        ));
    }

    Ok(key)
}

fn normalize_prefix(prefix: Option<String>) -> Result<String, ApiError> {
    let Some(prefix) = prefix else {
        return Ok("/".to_string());
    };

    if prefix.len() > MAX_KEY_LEN {
        return Err(ApiError::bad_request(format!(
            "prefix must be at most {MAX_KEY_LEN} bytes, got {}",
            prefix.len()
        )));
    }
    if prefix.chars().any(|c| c.is_control()) {
        return Err(ApiError::bad_request(
            "prefix must not contain control characters",
        ));
    }

    Ok(format!("/{}", prefix.trim_start_matches('/')))
}

pub async fn list_parameters(
    State(state): State<AppState>,
    Query(query): Query<ListQuery>,
) -> Result<ApiResponse<Vec<ParameterSummary>>, ApiError> {
    let prefix = normalize_prefix(query.prefix)?;

    let listed = parameters::list(&state.db, &prefix)
        .await
        .map_err(ApiError::internal)?
        .into_iter()
        .map(ParameterSummary::from)
        .collect();

    Ok(ApiResponse::ok(listed, "parameters fetched"))
}

pub async fn get_parameter(
    State(state): State<AppState>,
    Path(key): Path<String>,
    Query(query): Query<GetQuery>,
) -> Result<ApiResponse<ParameterResponse>, ApiError> {
    let key = normalize(&key)?;

    let found = parameters::fetch(&state.db, &key)
        .await
        .map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::not_found(format!("parameter {key} not found")))?;

    let response = match found {
        ParameterValue::String { version, value } => ParameterResponse {
            key,
            r#type: ParameterType::String,
            version: version as i64,
            value: Some(value),
        },
        ParameterValue::Secure { version, value } => {
            let plaintext = if query.decrypt.unwrap_or(false) {
                let decrypted = state
                    .master_key
                    .decrypt(&key, version, &value)
                    .map_err(ApiError::internal)?;
                Some(decrypted.to_string())
            } else {
                None
            };

            ParameterResponse {
                key,
                r#type: ParameterType::SecureString,
                version: version as i64,
                value: plaintext,
            }
        }
    };

    Ok(ApiResponse::ok(response, "parameter fetched"))
}

pub async fn put_parameter(
    State(state): State<AppState>,
    Path(key): Path<String>,
    Json(body): Json<PutParameterRequest>,
) -> Result<ApiResponse<PutResponse>, ApiError> {
    let key = normalize(&key)?;

    if body.value.len() > MAX_VALUE_LEN {
        return Err(ApiError::bad_request(format!(
            "value must be at most {MAX_VALUE_LEN} bytes, got {}",
            body.value.len()
        )));
    }

    let version = parameters::next_version(&state.db, &key)
        .await
        .map_err(ApiError::internal)?;

    let stored = match body.r#type {
        ParameterType::String => parameters::put_string(&state.db, &key, version, &body.value)
            .await
            .map_err(ApiError::internal)?,
        ParameterType::SecureString => {
            let encrypted = state
                .master_key
                .encrypt(&key, version, &body.value)
                .map_err(ApiError::internal)?;

            parameters::put_secure(&state.db, &key, version, &encrypted)
                .await
                .map_err(ApiError::internal)?
        }
    };

    if !stored {
        return Err(ApiError::conflict(format!(
            "parameter {key} was modified concurrently, retry the write"
        )));
    }

    Ok(ApiResponse::ok(
        PutResponse {
            key,
            version: version as i64,
        },
        "parameter stored",
    ))
}

pub async fn delete_parameter(
    State(state): State<AppState>,
    Path(key): Path<String>,
) -> Result<ApiResponse<()>, ApiError> {
    let key = normalize(&key)?;

    let deleted = parameters::delete(&state.db, &key)
        .await
        .map_err(ApiError::internal)?;

    if !deleted {
        return Err(ApiError::not_found(format!("parameter {key} not found")));
    }

    Ok(ApiResponse::ok((), "parameter deleted"))
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
        infra::{packages::package_manager::PackageManager, parameters::secrets::MasterKey},
        router,
    };

    struct Harness {
        router: axum::Router,
        state: AppState,
        key_dir: std::path::PathBuf,
    }

    impl Harness {
        async fn new(pool: SqlitePool) -> Self {
            use std::os::unix::fs::PermissionsExt;

            let key_dir = std::env::temp_dir().join(format!(
                "arges-handler-{}-{}",
                std::process::id(),
                uuid::Uuid::new_v4()
            ));
            std::fs::create_dir_all(&key_dir).unwrap();
            let key_path = key_dir.join("master.key");

            let mut key = [0u8; 32];
            getrandom::fill(&mut key).unwrap();
            std::fs::write(&key_path, key).unwrap();
            std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600)).unwrap();

            let state = AppState {
                db: pool,
                package_manager: PackageManager::APT,
                reconcile_notify: Arc::new(Notify::new()),
                deploy_notify: Arc::new(Notify::new()),
                master_key: Arc::new(MasterKey::load(&key_path).await.unwrap()),
                docker: None,
                caddy: crate::infra::proxy::admin::CaddyAdmin::new(
                    crate::constants::CADDY_ADMIN_URL,
                ),
                registry: crate::infra::containers::registry::RegistryClient::new(
                    crate::constants::REGISTRY_URL,
                ),
            };

            Self {
                router: router::routes(state.clone()),
                state,
                key_dir,
            }
        }

        async fn send(&self, method: &str, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
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
            let bytes = to_bytes(response.into_body(), 64 * 1024).await.unwrap();

            (status, serde_json::from_slice(&bytes).unwrap())
        }

        async fn put_secret(&self, uri: &str, value: &str) {
            let (status, _) = self
                .send(
                    "PUT",
                    uri,
                    Some(json!({"type": "secure_string", "value": value})),
                )
                .await;
            assert_eq!(status, StatusCode::OK);
        }
    }

    impl Drop for Harness {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.key_dir);
        }
    }

    #[sqlx::test]
    async fn a_secret_is_hidden_unless_decryption_is_requested(pool: SqlitePool) {
        let h = Harness::new(pool).await;
        h.put_secret("/api/parameter/app/db/password", "hunter2")
            .await;

        let (status, body) = h.send("GET", "/api/parameter/app/db/password", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["data"]["type"], "secure_string");
        assert_eq!(body["data"]["key"], "/app/db/password");
        assert!(body["data"]["value"].is_null());

        let (status, body) = h
            .send("GET", "/api/parameter/app/db/password?decrypt=true", None)
            .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["data"]["value"], "hunter2");
    }

    #[sqlx::test]
    async fn a_plain_string_is_always_returned(pool: SqlitePool) {
        let h = Harness::new(pool).await;
        let (status, _) = h
            .send(
                "PUT",
                "/api/parameter/app/name",
                Some(json!({"type": "string", "value": "arges"})),
            )
            .await;
        assert_eq!(status, StatusCode::OK);

        let (_, body) = h.send("GET", "/api/parameter/app/name", None).await;
        assert_eq!(body["data"]["value"], "arges");
        assert_eq!(body["data"]["version"], 1);
    }

    #[sqlx::test]
    async fn rewriting_a_secret_bumps_the_version(pool: SqlitePool) {
        let h = Harness::new(pool).await;
        h.put_secret("/api/parameter/app/token", "first").await;
        h.put_secret("/api/parameter/app/token", "second").await;

        let (_, body) = h
            .send("GET", "/api/parameter/app/token?decrypt=true", None)
            .await;
        assert_eq!(body["data"]["version"], 2);
        assert_eq!(body["data"]["value"], "second");
    }

    #[sqlx::test]
    async fn listing_exposes_no_values(pool: SqlitePool) {
        let h = Harness::new(pool).await;
        h.put_secret("/api/parameter/app/token", "hunter2").await;

        let (status, body) = h.send("GET", "/api/parameter", None).await;
        assert_eq!(status, StatusCode::OK);

        let entry = &body["data"][0];
        assert_eq!(entry["key"], "/app/token");
        assert_eq!(entry["type"], "secure_string");
        assert!(entry.get("value").is_none());
        assert!(entry.get("key_id").is_none());
        assert!(!body.to_string().contains("hunter2"));
    }

    #[sqlx::test]
    async fn listing_honours_a_prefix(pool: SqlitePool) {
        let h = Harness::new(pool).await;
        h.put_secret("/api/parameter/app/token", "a").await;
        h.put_secret("/api/parameter/other/token", "b").await;

        let (_, body) = h.send("GET", "/api/parameter?prefix=/app/", None).await;
        assert_eq!(body["data"].as_array().unwrap().len(), 1);
        assert_eq!(body["data"][0]["key"], "/app/token");
    }

    #[sqlx::test]
    async fn malformed_keys_are_rejected_with_400(pool: SqlitePool) {
        let h = Harness::new(pool).await;

        for uri in ["/api/parameter/app//name", "/api/parameter/app/name/"] {
            let (status, body) = h
                .send("PUT", uri, Some(json!({"type": "string", "value": "x"})))
                .await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{uri} -> {body}");
            assert_eq!(body["code"], "bad_request");
        }
    }

    #[sqlx::test]
    async fn a_keyless_write_does_not_reach_the_handler(pool: SqlitePool) {
        let h = Harness::new(pool).await;

        let (status, _) = h
            .send(
                "PUT",
                "/api/parameter/",
                Some(json!({"type": "string", "value": "x"})),
            )
            .await;

        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(parameters::list(&h.state.db, "/").await.unwrap().len(), 0);
    }

    #[sqlx::test]
    async fn an_oversized_value_is_rejected_with_400(pool: SqlitePool) {
        let h = Harness::new(pool).await;
        let value = "x".repeat(MAX_VALUE_LEN + 1);

        let (status, body) = h
            .send(
                "PUT",
                "/api/parameter/app/big",
                Some(json!({"type": "string", "value": value})),
            )
            .await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["code"], "bad_request");
    }

    #[sqlx::test]
    async fn missing_parameters_are_404_on_get_and_delete(pool: SqlitePool) {
        let h = Harness::new(pool).await;

        let (status, body) = h.send("GET", "/api/parameter/app/nope", None).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["code"], "not_found");

        let (status, _) = h.send("DELETE", "/api/parameter/app/nope", None).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[sqlx::test]
    async fn delete_removes_the_parameter(pool: SqlitePool) {
        let h = Harness::new(pool).await;
        h.put_secret("/api/parameter/app/token", "hunter2").await;

        let (status, _) = h.send("DELETE", "/api/parameter/app/token", None).await;
        assert_eq!(status, StatusCode::OK);

        let (status, _) = h.send("GET", "/api/parameter/app/token", None).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }
}
