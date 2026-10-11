//! In-process event bus: a bounded log ring buffer plus a broadcast channel
//! that feeds the `/api/events` SSE stream.
//!
//! Everything that should be visible in the GUI goes through `tracing`; the
//! [`BusLayer`] copies each event into the bus. A `source` field on the event
//! (an account id, `login:<id>`, ...) tags the entry so the UI can filter it.

use std::collections::VecDeque;
use std::fmt::Write as _;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use tokio::sync::broadcast;
use tracing::field::{Field, Visit};
use tracing_subscriber::layer::Context;
use tracing_subscriber::Layer;

use crate::api::{AccountView, LoginView};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Debug,
    Info,
    Warn,
    Error,
}

impl From<&tracing::Level> for Level {
    fn from(l: &tracing::Level) -> Self {
        match *l {
            tracing::Level::ERROR => Level::Error,
            tracing::Level::WARN => Level::Warn,
            tracing::Level::INFO => Level::Info,
            _ => Level::Debug,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct LogEntry {
    pub seq: u64,
    /// Unix time in milliseconds.
    pub ts: i64,
    pub source: String,
    pub level: Level,
    pub msg: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Event {
    Log(LogEntry),
    Account { account: AccountView },
    AccountRemoved { id: String },
    Login { login: LoginView },
}

pub struct EventBus {
    tx: broadcast::Sender<Event>,
    logs: Mutex<VecDeque<LogEntry>>,
    seq: AtomicU64,
    capacity: usize,
}

impl EventBus {
    pub fn new(capacity: usize) -> Arc<Self> {
        let (tx, _) = broadcast::channel(1024);
        Arc::new(Self {
            tx,
            logs: Mutex::new(VecDeque::with_capacity(capacity)),
            seq: AtomicU64::new(0),
            capacity,
        })
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.tx.subscribe()
    }

    pub fn emit(&self, event: Event) {
        // No receivers is fine: nobody has the GUI open.
        let _ = self.tx.send(event);
    }

    pub fn push_log(&self, source: &str, level: Level, msg: String) {
        let entry = LogEntry {
            seq: self.seq.fetch_add(1, Ordering::Relaxed) + 1,
            ts: now_millis(),
            source: source.to_string(),
            level,
            msg,
        };
        {
            let mut logs = self.logs.lock().unwrap();
            if logs.len() >= self.capacity {
                logs.pop_front();
            }
            logs.push_back(entry.clone());
        }
        self.emit(Event::Log(entry));
    }

    /// Returns up to `limit` of the newest buffered entries, oldest first.
    /// `source` filters per [`source_matches`]; `after` skips `seq <= after`.
    pub fn recent_logs(&self, source: Option<&str>, after: Option<u64>, limit: usize) -> Vec<LogEntry> {
        let logs = self.logs.lock().unwrap();
        let mut out: Vec<LogEntry> = logs
            .iter()
            .rev()
            .filter(|e| after.map_or(true, |a| e.seq > a))
            .filter(|e| source.map_or(true, |s| source_matches(&e.source, s)))
            .take(limit)
            .cloned()
            .collect();
        out.reverse();
        out
    }
}

/// `filter` matches a source exactly, or matches any source that ends with
/// `:<filter>` (so filtering by an account id also shows its login logs).
pub fn source_matches(source: &str, filter: &str) -> bool {
    source == filter || source.strip_suffix(filter).is_some_and(|p| p.ends_with(':'))
}

pub fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or_default()
}

/// `tracing` layer that mirrors events into an [`EventBus`].
pub struct BusLayer {
    bus: Arc<EventBus>,
}

impl BusLayer {
    pub fn new(bus: Arc<EventBus>) -> Self {
        Self { bus }
    }
}

#[derive(Default)]
struct EventVisitor {
    message: String,
    source: Option<String>,
    fields: String,
}

impl Visit for EventVisitor {
    fn record_str(&mut self, field: &Field, value: &str) {
        match field.name() {
            "message" => self.message.push_str(value),
            "source" => self.source = Some(value.to_string()),
            name => {
                let _ = write!(self.fields, " {name}={value}");
            }
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        match field.name() {
            "message" => {
                let _ = write!(self.message, "{value:?}");
            }
            "source" => self.source = Some(format!("{value:?}")),
            name => {
                let _ = write!(self.fields, " {name}={value:?}");
            }
        }
    }
}

impl<S: tracing::Subscriber> Layer<S> for BusLayer {
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
        let mut v = EventVisitor::default();
        event.record(&mut v);
        let mut msg = v.message;
        msg.push_str(&v.fields);
        let source = v.source.unwrap_or_else(|| "agent".to_string());
        self.bus.push_log(&source, event.metadata().level().into(), msg);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_buffer_is_bounded_and_filterable() {
        let bus = EventBus::new(3);
        bus.push_log("agent", Level::Info, "a".into());
        bus.push_log("acc1", Level::Info, "b".into());
        bus.push_log("login:acc1", Level::Warn, "c".into());
        bus.push_log("acc2", Level::Error, "d".into());

        let all = bus.recent_logs(None, None, 10);
        assert_eq!(all.iter().map(|e| e.msg.as_str()).collect::<Vec<_>>(), ["b", "c", "d"]);

        let acc1 = bus.recent_logs(Some("acc1"), None, 10);
        assert_eq!(acc1.len(), 2);

        let after = bus.recent_logs(None, Some(all[1].seq), 10);
        assert_eq!(after.len(), 1);
        assert_eq!(after[0].msg, "d");
    }

    #[test]
    fn source_filter_rules() {
        assert!(source_matches("abc", "abc"));
        assert!(source_matches("login:abc", "abc"));
        assert!(!source_matches("xabc", "abc"));
        assert!(!source_matches("abcd", "abc"));
    }
}
