use std::env;

use anyhow::{Context, Result, bail};
use tracing_subscriber::{EnvFilter, prelude::*};

const DEFAULT_FILTER: &str = "arges=info,tower_http=info";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LogFormat {
    Journald,
    Json,
    Text,
}

impl LogFormat {
    fn parse(raw: &str) -> Result<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "journald" | "journal" => Ok(Self::Journald),
            "json" => Ok(Self::Json),
            "text" | "plain" | "pretty" => Ok(Self::Text),
            other => bail!("ARGES_LOG_FORMAT must be journald, json or text, got {other}"),
        }
    }

    fn detect() -> Self {
        if env::var_os("JOURNAL_STREAM").is_some() {
            Self::Journald
        } else {
            Self::Text
        }
    }
}

pub fn init() -> Result<()> {
    let env_filter = EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new(DEFAULT_FILTER))
        .context("failed to configure tracing subscriber")?;

    let format = match env::var("ARGES_LOG_FORMAT") {
        Ok(raw) => LogFormat::parse(&raw)?,
        Err(env::VarError::NotPresent) => LogFormat::detect(),
        Err(e) => return Err(e).context("failed to read ARGES_LOG_FORMAT"),
    };

    let registry = tracing_subscriber::registry().with(env_filter);

    match format {
        LogFormat::Journald => match tracing_journald::layer() {
            Ok(layer) => registry.with(layer).init(),
            Err(e) => {
                registry.with(json_layer()).init();
                tracing::warn!(%e, "journald unavailable, logging json to stdout instead");
            }
        },
        LogFormat::Json => registry.with(json_layer()).init(),
        LogFormat::Text => registry.with(tracing_subscriber::fmt::layer()).init(),
    }

    Ok(())
}

fn json_layer<S>() -> impl tracing_subscriber::Layer<S>
where
    S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
{
    tracing_subscriber::fmt::layer().json()
}
