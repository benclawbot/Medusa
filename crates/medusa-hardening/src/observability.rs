use std::{
    collections::BTreeMap,
    fs,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Instant,
};

use medusa_core::MedusaResult;
use serde::{Deserialize, Serialize};

use crate::support::{append_atomic, internal, invalid};

/// Append-only JSONL operational event.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct OperationalEvent {
    pub timestamp: String,
    pub level: String,
    pub component: String,
    pub event: String,
    pub correlation_id: String,
    pub fields: BTreeMap<String, serde_json::Value>,
}

/// Thread-safe metrics and structured event recorder.
#[derive(Clone)]
pub struct Observability {
    root: PathBuf,
    counters: Arc<Mutex<BTreeMap<String, u64>>>,
    durations_ms: Arc<Mutex<BTreeMap<String, Vec<u128>>>>,
}

impl Observability {
    pub fn new(root: impl Into<PathBuf>) -> MedusaResult<Self> {
        let root = root.into();
        fs::create_dir_all(&root)?;
        Ok(Self {
            root,
            counters: Arc::new(Mutex::new(BTreeMap::new())),
            durations_ms: Arc::new(Mutex::new(BTreeMap::new())),
        })
    }

    pub fn increment(&self, name: &str, value: u64) -> MedusaResult<()> {
        validate_metric_name(name)?;
        let mut counters = self
            .counters
            .lock()
            .map_err(|_| internal("counter lock poisoned"))?;
        *counters.entry(name.to_owned()).or_default() += value;
        Ok(())
    }

    pub fn record_duration(&self, name: &str, started: Instant) -> MedusaResult<()> {
        validate_metric_name(name)?;
        self.durations_ms
            .lock()
            .map_err(|_| internal("duration lock poisoned"))?
            .entry(name.to_owned())
            .or_default()
            .push(started.elapsed().as_millis());
        Ok(())
    }

    pub fn emit(&self, mut event: OperationalEvent) -> MedusaResult<()> {
        redact_value_map(&mut event.fields);
        let path = self.root.join("events.jsonl");
        let mut line = serde_json::to_vec(&event)?;
        line.push(b'\n');
        append_atomic(&path, &line)
    }

    pub fn snapshot(&self) -> MedusaResult<serde_json::Value> {
        let counters = self
            .counters
            .lock()
            .map_err(|_| internal("counter lock poisoned"))?
            .clone();
        let durations = self
            .durations_ms
            .lock()
            .map_err(|_| internal("duration lock poisoned"))?
            .clone();
        Ok(serde_json::json!({
            "counters": counters,
            "durations_ms": durations,
        }))
    }
}

fn validate_metric_name(name: &str) -> MedusaResult<()> {
    if !name.is_empty()
        && name
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '.'))
    {
        Ok(())
    } else {
        Err(invalid(format!("invalid metric name: {name}")))
    }
}

fn redact_value_map(fields: &mut BTreeMap<String, serde_json::Value>) {
    for (key, value) in fields {
        let sensitive_key = ["secret", "token", "password", "authorization", "api_key"]
            .iter()
            .any(|needle| key.to_ascii_lowercase().contains(needle));
        if sensitive_key {
            *value = serde_json::Value::String("[REDACTED]".into());
        } else {
            redact_value(value);
        }
    }
}

pub(crate) fn redact_value(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::String(text) => {
            for marker in ["ghp_", "sk-", "Bearer "] {
                if text.contains(marker) {
                    *text = "[REDACTED]".into();
                    break;
                }
            }
        }
        serde_json::Value::Array(values) => values.iter_mut().for_each(redact_value),
        serde_json::Value::Object(values) => values.values_mut().for_each(redact_value),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, time::Instant};

    use super::*;

    #[test]
    fn metrics_snapshot_tracks_counters_and_durations() {
        let directory = tempfile::tempdir().expect("tempdir");
        let observability = Observability::new(directory.path()).expect("observability");
        observability
            .increment("agent.turns", 2)
            .expect("increment");
        observability
            .increment("agent.turns", 3)
            .expect("increment");
        observability
            .record_duration("agent.latency_ms", Instant::now())
            .expect("duration");

        let snapshot = observability.snapshot().expect("snapshot");
        assert_eq!(snapshot["counters"]["agent.turns"], 5);
        assert_eq!(
            snapshot["durations_ms"]["agent.latency_ms"]
                .as_array()
                .expect("durations")
                .len(),
            1
        );
    }

    #[test]
    fn invalid_metric_names_are_rejected() {
        let directory = tempfile::tempdir().expect("tempdir");
        let observability = Observability::new(directory.path()).expect("observability");
        assert!(observability.increment("", 1).is_err());
        assert!(observability.increment("agent turns", 1).is_err());
        assert!(
            observability
                .record_duration("agent/latency", Instant::now())
                .is_err()
        );
    }

    #[test]
    fn nested_events_are_redacted_and_appended() {
        let directory = tempfile::tempdir().expect("tempdir");
        let observability = Observability::new(directory.path()).expect("observability");
        for correlation_id in ["cor-1", "cor-2"] {
            observability
                .emit(OperationalEvent {
                    timestamp: "2026-07-12T00:00:00Z".into(),
                    level: "info".into(),
                    component: "test".into(),
                    event: "nested".into(),
                    correlation_id: correlation_id.into(),
                    fields: BTreeMap::from([
                        ("token_value".into(), serde_json::json!("plain-secret")),
                        (
                            "payload".into(),
                            serde_json::json!({"items": ["safe", "ghp_hidden"]}),
                        ),
                    ]),
                })
                .expect("emit");
        }
        let text = fs::read_to_string(directory.path().join("events.jsonl")).expect("events");
        assert_eq!(text.lines().count(), 2);
        assert!(!text.contains("plain-secret"));
        assert!(!text.contains("ghp_hidden"));
        assert!(text.contains("[REDACTED]"));
    }
}
