use serde_json::{Value, json};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TlsMode {
    Auto,
    Custom,
    Off,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Upstream {
    Container { name: String, port: u16 },
    Host { host: String, port: u16 },
}

impl Upstream {
    fn dial(&self) -> String {
        match self {
            Upstream::Container { name, port } => format!("{name}:{port}"),
            Upstream::Host { host, port } => format!("{host}:{port}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RouteKind {
    Proxy {
        upstream: Upstream,
        upstream_tls: bool,
    },
    Redirect {
        to: String,
        status: u16,
    },
    Static {
        root: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustomCertificate {
    pub certificate: String,
    pub private_key: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProxyRoute {
    pub domains: Vec<String>,
    pub kind: RouteKind,
    pub tls_mode: TlsMode,
    pub certificate: Option<CustomCertificate>,
}

impl ProxyRoute {
    fn handler(&self) -> Value {
        match &self.kind {
            RouteKind::Proxy {
                upstream,
                upstream_tls,
            } => {
                let mut handler = json!({
                    "handler": "reverse_proxy",
                    "upstreams": [{ "dial": upstream.dial() }],
                });

                if *upstream_tls {
                    handler["transport"] = json!({ "protocol": "http", "tls": {} });
                }

                handler
            }
            RouteKind::Redirect { to, status } => json!({
                "handler": "static_response",
                "status_code": status,
                "headers": { "Location": [to] },
            }),
            RouteKind::Static { root } => json!({
                "handler": "file_server",
                "root": root,
            }),
        }
    }

    fn to_route(&self) -> Value {
        json!({
            "match": [{ "host": self.domains }],
            "handle": [self.handler()],
            "terminal": true,
        })
    }
}

fn catch_all() -> Value {
    json!({
        "handle": [{
            "handler": "static_response",
            "status_code": 404,
            "body": "no route configured for this host",
        }],
        "terminal": true,
    })
}

pub fn render(routes: &[ProxyRoute]) -> Value {
    let mut http_routes: Vec<Value> = routes.iter().map(ProxyRoute::to_route).collect();
    http_routes.push(catch_all());

    let skip: Vec<&String> = routes
        .iter()
        .filter(|route| route.tls_mode == TlsMode::Off)
        .flat_map(|route| route.domains.iter())
        .collect();

    let mut server = json!({
        "listen": [":80", ":443"],
        "routes": http_routes,
    });

    if !skip.is_empty() {
        server["automatic_https"] = json!({ "skip": skip });
    }

    let certificates: Vec<Value> = routes
        .iter()
        .filter(|route| route.tls_mode == TlsMode::Custom)
        .filter_map(|route| {
            let certificate = route.certificate.as_ref()?;
            Some(json!({
                "certificate": certificate.certificate,
                "key": certificate.private_key,
                "tags": ["arges"],
            }))
        })
        .collect();

    let mut apps = json!({
        "http": { "servers": { "arges": server } },
    });

    if !certificates.is_empty() {
        apps["tls"] = json!({ "certificates": { "load_pem": certificates } });
    }

    json!({ "apps": apps })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proxy(domain: &str, container: &str, port: u16) -> ProxyRoute {
        ProxyRoute {
            domains: vec![domain.to_string()],
            kind: RouteKind::Proxy {
                upstream: Upstream::Container {
                    name: container.to_string(),
                    port,
                },
                upstream_tls: false,
            },
            tls_mode: TlsMode::Auto,
            certificate: None,
        }
    }

    #[test]
    fn a_proxy_route_dials_the_container() {
        let config = render(&[proxy("app.test", "whoami", 80)]);
        let route = &config["apps"]["http"]["servers"]["arges"]["routes"][0];

        assert_eq!(route["match"][0]["host"][0], "app.test");
        assert_eq!(route["handle"][0]["handler"], "reverse_proxy");
        assert_eq!(route["handle"][0]["upstreams"][0]["dial"], "whoami:80");
        assert!(route["handle"][0].get("transport").is_none());
        assert_eq!(route["terminal"], true);
    }

    #[test]
    fn an_https_upstream_gets_a_tls_transport() {
        let mut route = proxy("app.test", "whoami", 443);
        route.kind = RouteKind::Proxy {
            upstream: Upstream::Host {
                host: "backend.internal".to_string(),
                port: 8443,
            },
            upstream_tls: true,
        };

        let config = render(&[route]);
        let handler = &config["apps"]["http"]["servers"]["arges"]["routes"][0]["handle"][0];

        assert_eq!(handler["upstreams"][0]["dial"], "backend.internal:8443");
        assert_eq!(handler["transport"]["protocol"], "http");
        assert!(handler["transport"]["tls"].is_object());
    }

    #[test]
    fn routes_keep_their_given_order_and_end_with_a_catch_all() {
        let config = render(&[proxy("a.test", "x", 80), proxy("b.test", "y", 80)]);
        let routes = config["apps"]["http"]["servers"]["arges"]["routes"]
            .as_array()
            .unwrap();

        assert_eq!(routes.len(), 3);
        assert_eq!(routes[0]["match"][0]["host"][0], "a.test");
        assert_eq!(routes[1]["match"][0]["host"][0], "b.test");
        assert!(routes[2].get("match").is_none());
        assert_eq!(routes[2]["handle"][0]["status_code"], 404);
    }

    #[test]
    fn an_empty_config_still_answers_with_the_catch_all() {
        let routes = render(&[])["apps"]["http"]["servers"]["arges"]["routes"]
            .as_array()
            .unwrap()
            .len();

        assert_eq!(routes, 1);
    }

    #[test]
    fn a_redirect_renders_a_location_header() {
        let route = ProxyRoute {
            domains: vec!["old.test".to_string()],
            kind: RouteKind::Redirect {
                to: "https://new.test".to_string(),
                status: 308,
            },
            tls_mode: TlsMode::Auto,
            certificate: None,
        };

        let handler =
            &render(&[route])["apps"]["http"]["servers"]["arges"]["routes"][0]["handle"][0];
        assert_eq!(handler["handler"], "static_response");
        assert_eq!(handler["status_code"], 308);
        assert_eq!(handler["headers"]["Location"][0], "https://new.test");
    }

    #[test]
    fn tls_off_lands_in_the_automatic_https_skip_list() {
        let mut off = proxy("plain.test", "x", 80);
        off.tls_mode = TlsMode::Off;

        let config = render(&[proxy("secure.test", "y", 80), off]);
        let skip = &config["apps"]["http"]["servers"]["arges"]["automatic_https"]["skip"];

        assert_eq!(skip.as_array().unwrap().len(), 1);
        assert_eq!(skip[0], "plain.test");
    }

    #[test]
    fn a_custom_certificate_is_loaded_as_pem() {
        let mut route = proxy("custom.test", "x", 80);
        route.tls_mode = TlsMode::Custom;
        route.certificate = Some(CustomCertificate {
            certificate: "CERT-PEM".to_string(),
            private_key: "KEY-PEM".to_string(),
        });

        let pem = &render(&[route])["apps"]["tls"]["certificates"]["load_pem"][0];
        assert_eq!(pem["certificate"], "CERT-PEM");
        assert_eq!(pem["key"], "KEY-PEM");
    }

    #[test]
    fn no_tls_app_when_nothing_needs_a_custom_certificate() {
        let config = render(&[proxy("app.test", "x", 80)]);

        assert!(config["apps"].get("tls").is_none());
        assert!(
            config["apps"]["http"]["servers"]["arges"]
                .get("automatic_https")
                .is_none()
        );
    }
}
