use std::time::Duration;

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use sqlx::SqlitePool;
use tracing::info;

use crate::{
    db::queries::{
        audit,
        parameters::{self, ParameterValue},
        proxy::{self, ProxyHost, ProxyKind, TlsMode, UpstreamScheme},
    },
    infra::{
        parameters::secrets::MasterKey,
        proxy::{
            admin::CaddyAdmin,
            caddy::{
                CustomCertificate, ProxyRoute, RouteKind, TlsMode as RenderTlsMode, Upstream,
                render,
            },
        },
    },
};

const SUBJECT: &str = "proxy_config";

async fn read_secret(pool: &SqlitePool, key: &MasterKey, name: &str) -> Result<String> {
    let found = parameters::fetch(pool, name)
        .await?
        .with_context(|| format!("parameter {name} does not exist"))?;

    match found {
        ParameterValue::String { value, .. } => Ok(value),
        ParameterValue::Secure { version, value } => {
            Ok(key.decrypt(name, version, &value)?.to_string())
        }
    }
}

async fn to_route(pool: &SqlitePool, key: &MasterKey, host: &ProxyHost) -> Result<ProxyRoute> {
    let kind = match host.kind {
        ProxyKind::Proxy => {
            let port = host
                .upstream_port
                .with_context(|| format!("proxy host {} has no upstream port", host.id))?
                as u16;

            let upstream = match (&host.upstream_container, &host.upstream_host) {
                (Some(name), _) => Upstream::Container {
                    name: name.clone(),
                    port,
                },
                (None, Some(address)) => Upstream::Host {
                    host: address.clone(),
                    port,
                },
                (None, None) => {
                    anyhow::bail!("proxy host {} has no upstream", host.id)
                }
            };

            RouteKind::Proxy {
                upstream,
                upstream_tls: host.upstream_scheme == UpstreamScheme::Https,
            }
        }
        ProxyKind::Redirect => RouteKind::Redirect {
            to: host
                .redirect_to
                .clone()
                .with_context(|| format!("redirect host {} has no target", host.id))?,
            status: host.redirect_status.unwrap_or(308) as u16,
        },
        ProxyKind::Static => RouteKind::Static {
            root: host
                .static_root
                .clone()
                .with_context(|| format!("static host {} has no root", host.id))?,
        },
    };

    let certificate =
        match host.tls_mode {
            TlsMode::Custom => {
                let cert_name = host.tls_certificate_parameter.as_deref().with_context(|| {
                    format!("host {} uses a custom cert but names none", host.id)
                })?;
                let key_name = host.tls_private_key_parameter.as_deref().with_context(|| {
                    format!("host {} uses a custom cert but names no key", host.id)
                })?;

                Some(CustomCertificate {
                    certificate: read_secret(pool, key, cert_name).await?,
                    private_key: read_secret(pool, key, key_name).await?,
                })
            }
            _ => None,
        };

    // TODO: dns_provider_id is loaded but not rendered. DNS-01 needs a caddy image
    // built with a dns provider module (xcaddy), which caddy:2-alpine does not ship.

    Ok(ProxyRoute {
        domains: host.domains.clone(),
        kind,
        tls_mode: match host.tls_mode {
            TlsMode::Auto => RenderTlsMode::Auto,
            TlsMode::Custom => RenderTlsMode::Custom,
            TlsMode::Off => RenderTlsMode::Off,
        },
        certificate,
    })
}

pub struct PreparedConfig {
    pub config: serde_json::Value,
    pub serialised: String,
    pub hash: String,
    pub routes: usize,
}

pub async fn prepare(pool: &SqlitePool, key: &MasterKey) -> Result<PreparedConfig> {
    let hosts: Vec<ProxyHost> = proxy::list(pool)
        .await?
        .into_iter()
        .filter(|host| host.enabled && !host.domains.is_empty())
        .collect();

    let mut routes = Vec::with_capacity(hosts.len());
    for host in &hosts {
        routes.push(to_route(pool, key, host).await?);
    }

    let config = render(&routes);
    let serialised = serde_json::to_string(&config).context("failed to serialise proxy config")?;
    let hash = hex::encode(Sha256::digest(serialised.as_bytes()));

    Ok(PreparedConfig {
        config,
        serialised,
        hash,
        routes: routes.len(),
    })
}

pub async fn is_pending(pool: &SqlitePool, key: &MasterKey) -> Result<bool> {
    let prepared = prepare(pool, key).await?;
    Ok(proxy::last_applied_hash(pool).await? != Some(prepared.hash))
}

pub async fn apply(pool: &SqlitePool, key: &MasterKey, admin: &CaddyAdmin) -> Result<bool> {
    let PreparedConfig {
        config,
        serialised,
        hash,
        routes,
    } = prepare(pool, key).await?;

    if proxy::last_applied_hash(pool).await? == Some(hash.clone()) {
        return Ok(false);
    }

    let revision = proxy::record_pending(pool, &hash, &serialised).await?;

    match admin.load(&config).await {
        Ok(()) => {
            proxy::mark_applied(pool, revision).await?;
            audit::record(
                pool,
                SUBJECT,
                None,
                "applied",
                Some(&format!("{routes} route(s), revision {revision}")),
            )
            .await?;
            info!(revision, routes, "applied proxy config");
            Ok(true)
        }
        Err(e) => {
            let reason = format!("{e:#}");
            proxy::mark_failed(pool, revision, &reason).await?;
            audit::record(pool, SUBJECT, None, "apply_failed", Some(&reason)).await?;
            Err(e)
        }
    }
}

pub async fn apply_when_ready(
    pool: &SqlitePool,
    key: &MasterKey,
    admin: &CaddyAdmin,
    timeout: Duration,
) -> Result<bool> {
    admin.wait_ready(timeout).await?;
    apply(pool, key, admin).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::queries::proxy::NewProxyHost;

    fn proxy_host(domains: &[&str], container: &str, port: i64) -> NewProxyHost {
        NewProxyHost {
            kind: ProxyKind::Proxy,
            domains: domains.iter().map(|d| d.to_string()).collect(),
            priority: 100,
            enabled: true,
            upstream_container: Some(container.to_string()),
            upstream_host: None,
            upstream_port: Some(port),
            upstream_scheme: UpstreamScheme::Http,
            redirect_to: None,
            redirect_status: None,
            static_root: None,
            tls_mode: TlsMode::Auto,
            tls_certificate_parameter: None,
            tls_private_key_parameter: None,
            dns_provider_id: None,
        }
    }

    #[sqlx::test]
    async fn a_host_round_trips_with_its_domains(pool: SqlitePool) {
        proxy::create(&pool, "h1", &proxy_host(&["a.test", "b.test"], "app", 3000))
            .await
            .unwrap();

        let host = proxy::fetch(&pool, "h1").await.unwrap().unwrap();

        assert_eq!(host.domains, vec!["a.test", "b.test"]);
        assert_eq!(host.upstream_container.as_deref(), Some("app"));
        assert!(host.enabled);
    }

    #[sqlx::test]
    async fn updating_replaces_the_domain_set(pool: SqlitePool) {
        proxy::create(&pool, "h1", &proxy_host(&["a.test", "b.test"], "app", 3000))
            .await
            .unwrap();

        let mut changed = proxy_host(&["c.test"], "app", 3000);
        changed.priority = 5;
        assert!(proxy::update(&pool, "h1", &changed).await.unwrap());

        let host = proxy::fetch(&pool, "h1").await.unwrap().unwrap();
        assert_eq!(host.domains, vec!["c.test"]);
        assert_eq!(host.priority, 5);
    }

    #[sqlx::test]
    async fn a_domain_cannot_be_claimed_twice(pool: SqlitePool) {
        proxy::create(&pool, "h1", &proxy_host(&["a.test"], "app", 3000))
            .await
            .unwrap();

        assert!(
            proxy::create(&pool, "h2", &proxy_host(&["a.test"], "other", 3000))
                .await
                .is_err()
        );
        assert_eq!(proxy::list(&pool).await.unwrap().len(), 1);
    }

    #[sqlx::test]
    async fn deleting_a_host_takes_its_domains_with_it(pool: SqlitePool) {
        proxy::create(&pool, "h1", &proxy_host(&["a.test"], "app", 3000))
            .await
            .unwrap();
        assert!(proxy::delete(&pool, "h1").await.unwrap());

        proxy::create(&pool, "h2", &proxy_host(&["a.test"], "other", 3000))
            .await
            .unwrap();
        assert_eq!(proxy::list(&pool).await.unwrap().len(), 1);
    }

    #[sqlx::test]
    async fn hosts_come_back_in_priority_order(pool: SqlitePool) {
        let mut low = proxy_host(&["low.test"], "app", 80);
        low.priority = 900;
        let mut high = proxy_host(&["high.test"], "app", 80);
        high.priority = 10;

        proxy::create(&pool, "low", &low).await.unwrap();
        proxy::create(&pool, "high", &high).await.unwrap();

        let ids: Vec<String> = proxy::list(&pool)
            .await
            .unwrap()
            .into_iter()
            .map(|h| h.id)
            .collect();
        assert_eq!(ids, vec!["high", "low"]);
    }

    #[sqlx::test]
    async fn revisions_track_the_applied_hash(pool: SqlitePool) {
        assert_eq!(proxy::last_applied_hash(&pool).await.unwrap(), None);

        let first = proxy::record_pending(&pool, "hash-a", "{}").await.unwrap();
        proxy::mark_applied(&pool, first).await.unwrap();
        assert_eq!(
            proxy::last_applied_hash(&pool).await.unwrap().as_deref(),
            Some("hash-a")
        );

        let second = proxy::record_pending(&pool, "hash-b", "{}").await.unwrap();
        proxy::mark_failed(&pool, second, "caddy said no")
            .await
            .unwrap();
        assert_eq!(
            proxy::last_applied_hash(&pool).await.unwrap().as_deref(),
            Some("hash-a"),
            "a failed apply must not become the last applied config"
        );
    }
}

#[cfg(test)]
mod live {
    use super::*;
    use crate::db::queries::proxy::NewProxyHost;

    fn enabled() -> bool {
        std::env::var("ARGES_CADDY_LAB").is_ok()
    }

    async fn master_key() -> MasterKey {
        use std::os::unix::fs::PermissionsExt;

        let dir = std::env::temp_dir().join(format!("arges-lab-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("master.key");
        let mut bytes = [0u8; 32];
        getrandom::fill(&mut bytes).unwrap();
        std::fs::write(&path, bytes).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();

        MasterKey::load(&path).await.unwrap()
    }

    fn host(domains: &[&str], container: &str, port: i64) -> NewProxyHost {
        NewProxyHost {
            kind: ProxyKind::Proxy,
            domains: domains.iter().map(|d| d.to_string()).collect(),
            priority: 100,
            enabled: true,
            upstream_container: Some(container.to_string()),
            upstream_host: None,
            upstream_port: Some(port),
            upstream_scheme: UpstreamScheme::Http,
            redirect_to: None,
            redirect_status: None,
            static_root: None,
            tls_mode: TlsMode::Off,
            tls_certificate_parameter: None,
            tls_private_key_parameter: None,
            dns_provider_id: None,
        }
    }

    async fn get(host_header: &str) -> (u16, String) {
        let output = tokio::process::Command::new("curl")
            .args([
                "-s",
                "-o",
                "/dev/null",
                "-w",
                "%{http_code}",
                "-H",
                &format!("Host: {host_header}"),
                "http://127.0.0.1:8080/",
            ])
            .output()
            .await
            .unwrap();
        let code = String::from_utf8_lossy(&output.stdout).trim().to_string();
        (code.parse().unwrap_or(0), code)
    }

    #[sqlx::test]
    async fn rows_become_a_live_caddy_config(pool: SqlitePool) {
        if !enabled() {
            return;
        }

        let key = master_key().await;
        let admin = CaddyAdmin::new("http://127.0.0.1:2019");

        proxy::create(
            &pool,
            "h1",
            &host(&["app.test", "www.app.test"], "whoami", 80),
        )
        .await
        .unwrap();

        assert!(apply(&pool, &key, &admin).await.unwrap(), "first apply");
        assert_eq!(get("app.test").await.0, 200);
        assert_eq!(get("www.app.test").await.0, 200);
        assert_eq!(get("nothing.test").await.0, 404, "catch-all");

        assert!(
            !apply(&pool, &key, &admin).await.unwrap(),
            "an unchanged config must be a no-op"
        );

        assert!(proxy::set_enabled(&pool, "h1", false).await.unwrap());
        assert!(
            apply(&pool, &key, &admin).await.unwrap(),
            "toggle re-applies"
        );
        assert_eq!(get("app.test").await.0, 404, "disabled host stops routing");

        let log = audit::recent(&pool, "proxy_config", 10).await.unwrap();
        assert_eq!(log.len(), 2);
        assert!(log.iter().all(|e| e.action == "applied"));
    }

    #[sqlx::test]
    async fn a_config_caddy_rejects_is_recorded_as_failed(pool: SqlitePool) {
        if !enabled() {
            return;
        }

        let key = master_key().await;
        let broken = CaddyAdmin::new("http://127.0.0.1:59999");

        proxy::create(&pool, "h1", &host(&["app.test"], "whoami", 80))
            .await
            .unwrap();

        assert!(apply(&pool, &key, &broken).await.is_err());
        assert_eq!(proxy::last_applied_hash(&pool).await.unwrap(), None);

        let log = audit::recent(&pool, "proxy_config", 10).await.unwrap();
        assert_eq!(log[0].action, "apply_failed");
    }
}
