use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};

use serde::Serialize;
use tokio::sync::broadcast;
use tracing::{
    Event, Subscriber,
    field::{Field, Visit},
};
use tracing_subscriber::{Layer, layer::Context, registry::LookupSpan};

const CAPACITY: usize = 2000;
const CHANNEL_CAPACITY: usize = 256;

#[derive(Debug, Clone, Serialize)]
pub struct AgentLine {
    pub at: i64,
    pub level: String,
    pub target: String,
    pub message: String,
}

pub struct AgentLog {
    recent: Mutex<VecDeque<AgentLine>>,
    sender: broadcast::Sender<AgentLine>,
}

impl AgentLog {
    pub fn new() -> Arc<Self> {
        let (sender, _) = broadcast::channel(CHANNEL_CAPACITY);

        Arc::new(Self {
            recent: Mutex::new(VecDeque::with_capacity(CAPACITY)),
            sender,
        })
    }

    pub fn recent(&self, tail: usize) -> Vec<AgentLine> {
        let Ok(recent) = self.recent.lock() else {
            return Vec::new();
        };

        let skip = recent.len().saturating_sub(tail);
        recent.iter().skip(skip).cloned().collect()
    }

    pub fn subscribe(&self) -> broadcast::Receiver<AgentLine> {
        self.sender.subscribe()
    }

    fn push(&self, line: AgentLine) {
        if let Ok(mut recent) = self.recent.lock() {
            if recent.len() == CAPACITY {
                recent.pop_front();
            }
            recent.push_back(line.clone());
        }

        let _ = self.sender.send(line);
    }
}

#[derive(Default)]
struct MessageVisitor {
    message: String,
    fields: Vec<String>,
}

impl MessageVisitor {
    fn finish(self) -> String {
        if self.fields.is_empty() {
            return self.message;
        }

        let joined = self.fields.join(" ");

        if self.message.is_empty() {
            joined
        } else {
            format!("{} {joined}", self.message)
        }
    }
}

impl Visit for MessageVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.message = format!("{value:?}");
        } else {
            self.fields.push(format!("{}={value:?}", field.name()));
        }
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.message = value.to_string();
        } else {
            self.fields.push(format!("{}={value}", field.name()));
        }
    }
}

pub struct AgentLogLayer {
    log: Arc<AgentLog>,
}

impl AgentLogLayer {
    pub fn new(log: Arc<AgentLog>) -> Self {
        Self { log }
    }
}

impl<S> Layer<S> for AgentLogLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let mut visitor = MessageVisitor::default();
        event.record(&mut visitor);

        let metadata = event.metadata();

        self.log.push(AgentLine {
            at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0),
            level: metadata.level().to_string(),
            target: metadata.target().to_string(),
            message: visitor.finish(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(message: &str) -> AgentLine {
        AgentLine {
            at: 0,
            level: "INFO".to_string(),
            target: "arges".to_string(),
            message: message.to_string(),
        }
    }

    #[test]
    fn the_buffer_keeps_only_the_newest_lines() {
        let log = AgentLog::new();

        for i in 0..(CAPACITY + 50) {
            log.push(line(&format!("line-{i}")));
        }

        let recent = log.recent(CAPACITY * 2);
        assert_eq!(recent.len(), CAPACITY, "the buffer must stay bounded");
        assert_eq!(recent[0].message, format!("line-{}", 50));
        assert_eq!(
            recent[CAPACITY - 1].message,
            format!("line-{}", CAPACITY + 49)
        );
    }

    #[test]
    fn a_tail_returns_the_newest_lines_in_order() {
        let log = AgentLog::new();
        for i in 0..10 {
            log.push(line(&format!("line-{i}")));
        }

        let recent = log.recent(3);
        let messages: Vec<&str> = recent.iter().map(|l| l.message.as_str()).collect();

        assert_eq!(messages, vec!["line-7", "line-8", "line-9"]);
    }

    #[test]
    fn asking_for_more_than_exists_is_fine() {
        let log = AgentLog::new();
        log.push(line("only"));

        assert_eq!(log.recent(500).len(), 1);
    }

    #[tokio::test]
    async fn subscribers_receive_new_lines() {
        let log = AgentLog::new();
        let mut receiver = log.subscribe();

        log.push(line("after-subscribe"));

        let received = receiver.recv().await.unwrap();
        assert_eq!(received.message, "after-subscribe");
    }

    #[tokio::test]
    async fn a_subscriber_does_not_see_lines_from_before_it_joined() {
        let log = AgentLog::new();
        log.push(line("before"));

        let mut receiver = log.subscribe();
        log.push(line("after"));

        assert_eq!(receiver.recv().await.unwrap().message, "after");
    }

    #[test]
    fn fields_are_appended_to_the_message() {
        let mut visitor = MessageVisitor::default();
        visitor.message = "deployment failed".to_string();
        visitor.fields.push("name=web".to_string());

        assert_eq!(visitor.finish(), "deployment failed name=web");
    }
}
