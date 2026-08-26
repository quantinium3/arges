use axum::extract::{Json, Path, State};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    constants::MAX_PROXY_DOMAIN_LEN,
    db::queries::{
        audit,
        proxy::{self, NewProxyHost, ProxyHost, ProxyKind, TlsMode, UpstreamScheme},
    },
    infra::proxy::reconciler,
    state::AppState,
    utils::api_response::{ApiError, ApiResponse},
};

const SUBJECT: &str = "proxy_host";

#[derive(Deserialize)]
pub struct ProxyHostRequest {
    pub kind: ProxyKind,
    pub domains: Vec<String>,
    pub priority: Option<i64>,
    pub enabled: Option<bool>,
    pub upstream_container: Option<String>,
    pub upstream_host: Option<String>,
    pub upstream_port: Option<i64>,
    pub upstream_scheme: Option<UpstreamScheme>,
    pub redirect_to: Option<String>,
    pub redirect_status: Option<i64>,
    pub static_root: Option<String>,
    pub tls_mode: Option<TlsMode>,
    pub tls_certificate_parameter: Option<String>,
    pub tls_private_key_parameter: Option<String>,
    pub dns_provider_id: Option<String>,
}

#[derive(Serialize)]
pub struct ProxyStatus {
    pub hosts: usize,
    pub enabled_hosts: usize,
    pub last_applied_hash: Option<String>,
    pub pending: bool,
}

fn normalize_domains(raw: &[String]) -> Result<Vec<String>, ApiError> {
    if raw.is_empty() {
        return Err(ApiError::bad_request("at least one domain is required"));
    }

    let mut domains = Vec::with_capacity(raw.len());

    for domain in raw {
        let domain = domain.trim().to_lowercase();

        if domain.is_empty() || domain.len() > MAX_PROXY_DOMAIN_LEN {
            return Err(ApiError::bad_request(format!(
                "domain must be between 1 and {MAX_PROXY_DOMAIN_LEN} characters"
            )));
        }
        if domain.contains('/') || domain.contains(' ') || domain.contains(':') {
            return Err(ApiError::bad_request(format!(
                "{domain} must be a bare hostname, without a scheme, port or path"
            )));
        }
        if domains.contains(&domain) {
            return Err(ApiError::bad_request(format!("{domain} is listed twice")));
        }

        domains.push(domain);
    }

    Ok(domains)
}

fn to_new(request: ProxyHostRequest) -> Result<NewProxyHost, ApiError> {
    let domains = normalize_domains(&request.domains)?;
    let tls_mode = request.tls_mode.unwrap_or(TlsMode::Auto);

    match request.kind {
        ProxyKind::Proxy => {
            let has_container = request.upstream_container.is_some();
            let has_host = request.upstream_host.is_some();

            if has_container == has_host {
                return Err(ApiError::bad_request(
                    "a proxy needs exactly one of upstream_container or upstream_host",
                ));
            }
            match request.upstream_port {
                Some(port) if (1..=65535).contains(&port) => {}
                _ => {
                    return Err(ApiError::bad_request(
                        "a proxy needs an upstream_port between 1 and 65535",
                    ));
                }
            }
        }
        ProxyKind::Redirect => {
            if request.redirect_to.is_none() {
                return Err(ApiError::bad_request("a redirect needs a redirect_to"));
            }
            if let Some(status) = request.redirect_status
                && ![301, 302, 307, 308].contains(&status)
            {
                return Err(ApiError::bad_request(
                    "redirect_status must be 301, 302, 307 or 308",
                ));
            }
        }
        ProxyKind::Static => {
            if request.static_root.is_none() {
                return Err(ApiError::bad_request("a static host needs a static_root"));
            }
        }
    }

    if tls_mode == TlsMode::Custom
        && (request.tls_certificate_parameter.is_none()
            || request.tls_private_key_parameter.is_none())
    {
        return Err(ApiError::bad_request(
            "a custom tls_mode needs both tls_certificate_parameter and tls_private_key_parameter",
        ));
    }

    let is_proxy = request.kind == ProxyKind::Proxy;
    let is_redirect = request.kind == ProxyKind::Redirect;
    let is_static = request.kind == ProxyKind::Static;

    Ok(NewProxyHost {
        kind: request.kind,
        domains,
        priority: request.priority.unwrap_or(100),
        enabled: request.enabled.unwrap_or(true),
        upstream_container: is_proxy.then_some(request.upstream_container).flatten(),
        upstream_host: is_proxy.then_some(request.upstream_host).flatten(),
        upstream_port: is_proxy.then_some(request.upstream_port).flatten(),
        upstream_scheme: request.upstream_scheme.unwrap_or(UpstreamScheme::Http),
        redirect_to: is_redirect.then_some(request.redirect_to).flatten(),
        redirect_status: is_redirect.then(|| request.redirect_status.unwrap_or(308)),
        static_root: is_static.then_some(request.static_root).flatten(),
        tls_mode,
        tls_certificate_parameter: request.tls_certificate_parameter,
        tls_private_key_parameter: request.tls_private_key_parameter,
        dns_provider_id: request.dns_provider_id,
    })
}

async fn reapply(state: &AppState) -> &'static str {
    match reconciler::apply(&state.db, &state.master_key, &state.caddy).await {
        Ok(true) => "proxy reloaded",
        Ok(false) => "no proxy change to apply",
        Err(e) => {
            tracing::warn!(error = ?e, "saved the proxy host but could not reload caddy");
            "saved, but the proxy could not be reloaded"
        }
    }
}

async fn load(state: &AppState, id: &str) -> Result<ProxyHost, ApiError> {
    proxy::fetch(&state.db, id)
        .await
        .map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::not_found(format!("proxy host {id} not found")))
}

pub async fn list_proxy_hosts(
    State(state): State<AppState>,
) -> Result<ApiResponse<Vec<ProxyHost>>, ApiError> {
    let hosts = proxy::list(&state.db).await.map_err(ApiError::internal)?;

    Ok(ApiResponse::ok(hosts, "proxy hosts fetched"))
}

pub async fn get_proxy_host(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<ApiResponse<ProxyHost>, ApiError> {
    let host = load(&state, &id).await?;

    Ok(ApiResponse::ok(host, "proxy host fetched"))
}

pub async fn create_proxy_host(
    State(state): State<AppState>,
    Json(request): Json<ProxyHostRequest>,
) -> Result<ApiResponse<ProxyHost>, ApiError> {
    let new = to_new(request)?;
    let id = Uuid::new_v4().to_string();

    proxy::create(&state.db, &id, &new).await.map_err(|e| {
        ApiError::conflict(format!(
            "could not create the proxy host: {}",
            root_cause(&e)
        ))
    })?;

    audit::record(
        &state.db,
        SUBJECT,
        Some(&id),
        "created",
        Some(&new.domains.join(", ")),
    )
    .await
    .map_err(ApiError::internal)?;

    let message = reapply(&state).await;

    Ok(ApiResponse::ok(load(&state, &id).await?, message))
}

pub async fn update_proxy_host(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(request): Json<ProxyHostRequest>,
) -> Result<ApiResponse<ProxyHost>, ApiError> {
    let new = to_new(request)?;

    let found = proxy::update(&state.db, &id, &new).await.map_err(|e| {
        ApiError::conflict(format!(
            "could not update the proxy host: {}",
            root_cause(&e)
        ))
    })?;

    if !found {
        return Err(ApiError::not_found(format!("proxy host {id} not found")));
    }

    audit::record(
        &state.db,
        SUBJECT,
        Some(&id),
        "updated",
        Some(&new.domains.join(", ")),
    )
    .await
    .map_err(ApiError::internal)?;

    let message = reapply(&state).await;

    Ok(ApiResponse::ok(load(&state, &id).await?, message))
}

pub async fn delete_proxy_host(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<ApiResponse<()>, ApiError> {
    if !proxy::delete(&state.db, &id)
        .await
        .map_err(ApiError::internal)?
    {
        return Err(ApiError::not_found(format!("proxy host {id} not found")));
    }

    audit::record(&state.db, SUBJECT, Some(&id), "deleted", None)
        .await
        .map_err(ApiError::internal)?;

    let message = reapply(&state).await;

    Ok(ApiResponse::ok((), message))
}

async fn set_enabled(
    state: AppState,
    id: &str,
    enabled: bool,
) -> Result<ApiResponse<ProxyHost>, ApiError> {
    if !proxy::set_enabled(&state.db, id, enabled)
        .await
        .map_err(ApiError::internal)?
    {
        return Err(ApiError::not_found(format!("proxy host {id} not found")));
    }

    let action = if enabled { "enabled" } else { "disabled" };
    audit::record(&state.db, SUBJECT, Some(id), action, None)
        .await
        .map_err(ApiError::internal)?;

    let message = reapply(&state).await;

    Ok(ApiResponse::ok(load(&state, id).await?, message))
}

pub async fn enable_proxy_host(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<ApiResponse<ProxyHost>, ApiError> {
    set_enabled(state, &id, true).await
}

pub async fn disable_proxy_host(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<ApiResponse<ProxyHost>, ApiError> {
    set_enabled(state, &id, false).await
}

pub async fn apply_proxy_config(
    State(state): State<AppState>,
) -> Result<ApiResponse<ProxyStatus>, ApiError> {
    let changed = reconciler::apply(&state.db, &state.master_key, &state.caddy)
        .await
        .map_err(|e| ApiError::bad_gateway(format!("{}", root_cause(&e))))?;

    let message = if changed {
        "proxy reloaded"
    } else {
        "no proxy change to apply"
    };

    Ok(ApiResponse::ok(status_of(&state).await?, message))
}

pub async fn get_proxy_status(
    State(state): State<AppState>,
) -> Result<ApiResponse<ProxyStatus>, ApiError> {
    Ok(ApiResponse::ok(
        status_of(&state).await?,
        "proxy status fetched",
    ))
}

async fn status_of(state: &AppState) -> Result<ProxyStatus, ApiError> {
    let hosts = proxy::list(&state.db).await.map_err(ApiError::internal)?;

    Ok(ProxyStatus {
        hosts: hosts.len(),
        enabled_hosts: hosts.iter().filter(|host| host.enabled).count(),
        last_applied_hash: proxy::last_applied_hash(&state.db)
            .await
            .map_err(ApiError::internal)?,
        pending: reconciler::is_pending(&state.db, &state.master_key)
            .await
            .map_err(ApiError::internal)?,
    })
}

fn root_cause(error: &anyhow::Error) -> String {
    error
        .chain()
        .last()
        .map_or_else(|| error.to_string(), |e| e.to_string())
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

    use super::*;
    use crate::{
        constants::CADDY_ADMIN_URL,
        infra::{
            packages::package_manager::PackageManager, parameters::secrets::MasterKey,
            proxy::admin::CaddyAdmin,
        },
        router,
    };
    use tower::ServiceExt;

    struct Harness {
        router: axum::Router,
        dir: std::path::PathBuf,
    }

    impl Harness {
        async fn new(pool: SqlitePool) -> Self {
            use std::os::unix::fs::PermissionsExt;

            let dir = std::env::temp_dir().join(format!("arges-proxy-{}", Uuid::new_v4()));
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
                agent_log: crate::logging::buffer::AgentLog::new(),
                master_key: Arc::new(MasterKey::load(&path).await.unwrap()),
                docker: None,
                caddy: CaddyAdmin::new(CADDY_ADMIN_URL),
                registry: crate::infra::containers::registry::RegistryClient::new(
                    crate::constants::REGISTRY_URL,
                ),
            };

            Self {
                router: router::routes(state),
                dir,
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
            let bytes = to_bytes(response.into_body(), 256 * 1024).await.unwrap();

            (status, serde_json::from_slice(&bytes).unwrap())
        }
    }

    impl Drop for Harness {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn proxy_body(domain: &str) -> Value {
        json!({
            "kind": "proxy",
            "domains": [domain],
            "upstream_container": "app",
            "upstream_port": 3000
        })
    }

    #[sqlx::test]
    async fn a_proxy_host_is_created_and_listed(pool: SqlitePool) {
        let h = Harness::new(pool).await;

        let (status, body) = h
            .send("POST", "/api/proxy", Some(proxy_body("App.Test ")))
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(
            body["data"]["domains"][0], "app.test",
            "domain is normalised"
        );
        assert_eq!(body["data"]["kind"], "proxy");
        assert_eq!(body["data"]["enabled"], true);

        let (_, list) = h.send("GET", "/api/proxy", None).await;
        assert_eq!(list["data"].as_array().unwrap().len(), 1);
    }

    #[sqlx::test]
    async fn apply_and_status_are_not_swallowed_by_the_id_route(pool: SqlitePool) {
        let h = Harness::new(pool).await;

        let (status, body) = h.send("GET", "/api/proxy/status", None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["data"]["hosts"], 0);
        assert_eq!(
            body["data"]["pending"], true,
            "even an empty config is pending until the catch-all reaches caddy"
        );

        let (status, _) = h.send("POST", "/api/proxy/apply", None).await;
        assert_eq!(
            status,
            StatusCode::BAD_GATEWAY,
            "no caddy running, so apply must report a gateway failure"
        );
    }

    #[sqlx::test]
    async fn status_reports_pending_work(pool: SqlitePool) {
        let h = Harness::new(pool).await;
        h.send("POST", "/api/proxy", Some(proxy_body("a.test")))
            .await;

        let (_, body) = h.send("GET", "/api/proxy/status", None).await;
        assert_eq!(body["data"]["hosts"], 1);
        assert_eq!(body["data"]["enabled_hosts"], 1);
        assert_eq!(body["data"]["pending"], true);
        assert!(body["data"]["last_applied_hash"].is_null());
    }

    #[sqlx::test]
    async fn a_saved_host_survives_caddy_being_unreachable(pool: SqlitePool) {
        let h = Harness::new(pool).await;

        let (status, body) = h
            .send("POST", "/api/proxy", Some(proxy_body("a.test")))
            .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            body["message"],
            "saved, but the proxy could not be reloaded"
        );
        assert_eq!(
            h.send("GET", "/api/proxy", None).await.1["data"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
    }

    #[sqlx::test]
    async fn a_duplicate_domain_is_a_conflict(pool: SqlitePool) {
        let h = Harness::new(pool).await;
        h.send("POST", "/api/proxy", Some(proxy_body("a.test")))
            .await;

        let (status, body) = h
            .send("POST", "/api/proxy", Some(proxy_body("a.test")))
            .await;

        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(body["code"], "conflict");
    }

    #[sqlx::test]
    async fn invalid_requests_are_rejected(pool: SqlitePool) {
        let h = Harness::new(pool).await;

        let cases = [
            (
                json!({"kind":"proxy","domains":[],"upstream_container":"a","upstream_port":80}),
                "no domains",
            ),
            (
                json!({"kind":"proxy","domains":["a.test"],"upstream_port":80}),
                "no upstream",
            ),
            (
                json!({"kind":"proxy","domains":["a.test"],"upstream_container":"a","upstream_host":"b","upstream_port":80}),
                "two upstreams",
            ),
            (
                json!({"kind":"proxy","domains":["a.test"],"upstream_container":"a"}),
                "no port",
            ),
            (
                json!({"kind":"proxy","domains":["http://a.test/x"],"upstream_container":"a","upstream_port":80}),
                "url as domain",
            ),
            (
                json!({"kind":"proxy","domains":["a.test","A.TEST"],"upstream_container":"a","upstream_port":80}),
                "duplicate domain",
            ),
            (
                json!({"kind":"redirect","domains":["a.test"]}),
                "redirect without target",
            ),
            (
                json!({"kind":"redirect","domains":["a.test"],"redirect_to":"x","redirect_status":399}),
                "bad redirect status",
            ),
            (
                json!({"kind":"static","domains":["a.test"]}),
                "static without root",
            ),
            (
                json!({"kind":"proxy","domains":["a.test"],"upstream_container":"a","upstream_port":80,"tls_mode":"custom"}),
                "custom tls without params",
            ),
        ];

        for (body, label) in cases {
            let (status, response) = h.send("POST", "/api/proxy", Some(body)).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{label} -> {response}");
        }
    }

    #[sqlx::test]
    async fn a_host_is_updated_toggled_and_deleted(pool: SqlitePool) {
        let h = Harness::new(pool).await;
        let (_, created) = h
            .send("POST", "/api/proxy", Some(proxy_body("a.test")))
            .await;
        let id = created["data"]["id"].as_str().unwrap().to_string();

        let (status, body) = h
            .send(
                "PUT",
                &format!("/api/proxy/{id}"),
                Some(json!({
                    "kind": "redirect",
                    "domains": ["b.test"],
                    "redirect_to": "https://c.test"
                })),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["data"]["kind"], "redirect");
        assert_eq!(body["data"]["domains"][0], "b.test");
        assert_eq!(body["data"]["redirect_status"], 308);
        assert!(body["data"]["upstream_container"].is_null());

        let (_, body) = h
            .send("POST", &format!("/api/proxy/{id}/disable"), None)
            .await;
        assert_eq!(body["data"]["enabled"], false);

        let (_, body) = h
            .send("POST", &format!("/api/proxy/{id}/enable"), None)
            .await;
        assert_eq!(body["data"]["enabled"], true);

        let (status, _) = h.send("DELETE", &format!("/api/proxy/{id}"), None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            h.send("GET", &format!("/api/proxy/{id}"), None).await.0,
            StatusCode::NOT_FOUND
        );
    }

    #[sqlx::test]
    async fn a_missing_host_is_a_404(pool: SqlitePool) {
        let h = Harness::new(pool).await;

        for (method, uri) in [
            ("GET", "/api/proxy/nope"),
            ("DELETE", "/api/proxy/nope"),
            ("POST", "/api/proxy/nope/enable"),
        ] {
            assert_eq!(
                h.send(method, uri, None).await.0,
                StatusCode::NOT_FOUND,
                "{uri}"
            );
        }
    }
}
