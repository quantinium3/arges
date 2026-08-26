use anyhow::{Context, Result, bail};
use serde_json::Value;
use tokio::process::Command;

use crate::logging::buffer::AgentLine;

const MAX_TAIL: i64 = 5000;

pub struct JournalReader {
    unit: String,
}

fn level_of(priority: Option<&str>) -> &'static str {
    match priority {
        Some("0") | Some("1") | Some("2") | Some("3") => "ERROR",
        Some("4") => "WARN",
        Some("5") => "INFO",
        Some("6") => "DEBUG",
        _ => "TRACE",
    }
}

fn text_of(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(bytes)) => {
            let raw: Vec<u8> = bytes
                .iter()
                .filter_map(|b| b.as_u64())
                .map(|b| b as u8)
                .collect();
            String::from_utf8_lossy(&raw).into_owned()
        }
        _ => String::new(),
    }
}

fn to_line(entry: &Value) -> Option<AgentLine> {
    let object = entry.as_object()?;

    let message = text_of(object.get("MESSAGE"));
    if message.is_empty() {
        return None;
    }

    let mut fields: Vec<String> = object
        .iter()
        .filter_map(|(key, value)| {
            key.strip_prefix("F_")
                .map(|name| format!("{}={}", name.to_lowercase(), text_of(Some(value))))
        })
        .collect();
    fields.sort();

    let message = if fields.is_empty() {
        message
    } else {
        format!("{message} {}", fields.join(" "))
    };

    let at = object
        .get("__REALTIME_TIMESTAMP")
        .and_then(Value::as_str)
        .and_then(|micros| micros.parse::<i64>().ok())
        .map(|micros| micros / 1000)
        .unwrap_or(0);

    let target = object
        .get("TARGET")
        .and_then(Value::as_str)
        .or_else(|| object.get("SYSLOG_IDENTIFIER").and_then(Value::as_str))
        .unwrap_or("systemd")
        .to_string();

    Some(AgentLine {
        at,
        level: level_of(object.get("PRIORITY").and_then(Value::as_str)).to_string(),
        target,
        message,
    })
}

impl JournalReader {
    pub fn new(unit: impl Into<String>) -> Self {
        Self { unit: unit.into() }
    }

    pub async fn available(&self) -> bool {
        Command::new("journalctl")
            .arg("--version")
            .output()
            .await
            .is_ok_and(|out| out.status.success())
    }

    pub async fn history(&self, tail: i64, since: Option<&str>) -> Result<Vec<AgentLine>> {
        let tail = tail.clamp(1, MAX_TAIL);

        let mut command = Command::new("journalctl");
        command
            .arg("-u")
            .arg(&self.unit)
            .arg("-n")
            .arg(tail.to_string())
            .arg("-o")
            .arg("json")
            .arg("--no-pager");

        if let Some(since) = since {
            command.arg("--since").arg(since);
        }

        let output = command
            .output()
            .await
            .context("failed to run journalctl, is systemd available on this host?")?;

        if !output.status.success() {
            let reason = String::from_utf8_lossy(&output.stderr);
            bail!("journalctl failed: {}", reason.trim());
        }

        Ok(String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .filter_map(|entry| to_line(&entry))
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn structured_fields_are_folded_back_into_the_message() {
        let entry = json!({
            "MESSAGE": "applied proxy config",
            "PRIORITY": "5",
            "TARGET": "arges::infra::proxy::reconciler",
            "F_REVISION": "1",
            "F_ROUTES": "0",
            "__REALTIME_TIMESTAMP": "1787775760800542"
        });

        let line = to_line(&entry).unwrap();

        assert_eq!(line.message, "applied proxy config revision=1 routes=0");
        assert_eq!(line.level, "INFO");
        assert_eq!(line.target, "arges::infra::proxy::reconciler");
        assert_eq!(line.at, 1787775760800);
    }

    #[test]
    fn priorities_map_onto_tracing_levels() {
        assert_eq!(level_of(Some("3")), "ERROR");
        assert_eq!(level_of(Some("4")), "WARN");
        assert_eq!(level_of(Some("5")), "INFO");
        assert_eq!(level_of(Some("6")), "DEBUG");
        assert_eq!(level_of(None), "TRACE");
    }

    #[test]
    fn systemds_own_messages_are_kept_and_labelled() {
        let entry = json!({
            "MESSAGE": "arges.service: Main process exited, code=killed",
            "PRIORITY": "6",
            "SYSLOG_IDENTIFIER": "systemd"
        });

        let line = to_line(&entry).unwrap();

        assert_eq!(line.target, "systemd");
        assert!(line.message.contains("Main process exited"));
    }

    #[test]
    fn a_non_utf8_message_is_recovered_lossily() {
        let entry = json!({ "MESSAGE": [104, 105, 255], "PRIORITY": "5" });

        assert!(to_line(&entry).unwrap().message.starts_with("hi"));
    }

    #[test]
    fn entries_without_a_message_are_skipped() {
        assert!(to_line(&json!({ "PRIORITY": "5" })).is_none());
    }
}
