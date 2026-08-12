//! Typed, fail-closed operational health and support-bundle contracts.
//!
//! This module deliberately contains only bounded, local projections. It does not probe a
//! provider, start a process, or perform a network request. Callers must supply evidence for a
//! component status; configuration presence alone is never converted into readiness.

use std::{collections::BTreeMap, fs, path::Path};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use serde::{Deserialize, Serialize};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

use crate::{OperationalEvent, observability::redact_value};

pub const OPERATIONAL_HEALTH_SCHEMA_VERSION: u16 = 1;
const MAX_COMPONENTS: usize = 32;
const MAX_TEXT_BYTES: usize = 512;
const MAX_EVENTS: usize = 64;
const MAX_EVENT_BYTES: usize = 8 * 1024;
const MAX_BUNDLE_BYTES: usize = 512 * 1024;

/// The least permissive status wins when a report is aggregated.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HealthStatus {
    HealthyReady,
    DegradedSafe,
    OptionalUnavailable,
    BlockedUserAction,
    UnhealthyRecoveryRequired,
    UnsafeQuarantine,
}

impl HealthStatus {
    #[must_use]
    pub const fn severity(self) -> u8 {
        match self {
            Self::HealthyReady => 0,
            Self::DegradedSafe => 1,
            Self::OptionalUnavailable => 2,
            Self::BlockedUserAction => 3,
            Self::UnhealthyRecoveryRequired => 4,
            Self::UnsafeQuarantine => 5,
        }
    }

    #[must_use]
    pub const fn safe_to_continue(self) -> bool {
        matches!(
            self,
            Self::HealthyReady
                | Self::DegradedSafe
                | Self::OptionalUnavailable
                | Self::BlockedUserAction
        )
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct HealthComponent {
    pub id: String,
    pub status: HealthStatus,
    pub summary: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remediation: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<String>,
}

impl HealthComponent {
    pub fn new(
        id: impl Into<String>,
        status: HealthStatus,
        summary: impl Into<String>,
        remediation: Option<String>,
    ) -> MedusaResult<Self> {
        let id = bounded_text(id.into(), "component id")?;
        if id.is_empty()
            || !id.chars().all(|character| {
                character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '.')
            })
        {
            return Err(invalid("component id must be non-empty ASCII identifier"));
        }
        Ok(Self {
            id,
            status,
            summary: bounded_text(summary.into(), "component summary")?,
            remediation: remediation
                .map(|value| bounded_text(value, "component remediation"))
                .transpose()?,
            correlation_id: None,
        })
    }

    #[must_use]
    pub fn with_correlation_id(mut self, correlation_id: impl Into<String>) -> Self {
        self.correlation_id = bounded_text(correlation_id.into(), "correlation id").ok();
        self
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct HealthReport {
    pub schema_version: u16,
    pub status: HealthStatus,
    pub safe_to_continue: bool,
    pub components: Vec<HealthComponent>,
}

impl HealthReport {
    pub fn new(mut components: Vec<HealthComponent>) -> MedusaResult<Self> {
        if components.len() > MAX_COMPONENTS {
            return Err(invalid(format!(
                "operational health report exceeds {MAX_COMPONENTS} components"
            )));
        }
        components.sort_by(|left, right| left.id.cmp(&right.id));
        let status = components
            .iter()
            .map(|component| component.status)
            .max_by_key(|candidate| candidate.severity())
            .unwrap_or(HealthStatus::HealthyReady);
        Ok(Self {
            schema_version: OPERATIONAL_HEALTH_SCHEMA_VERSION,
            status,
            safe_to_continue: status.safe_to_continue(),
            components,
        })
    }

    #[must_use]
    pub fn component(&self, id: &str) -> Option<&HealthComponent> {
        self.components.iter().find(|component| component.id == id)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ResourceBudget {
    pub name: String,
    pub max_bytes: u64,
    pub max_entries: u64,
}

impl ResourceBudget {
    pub fn new(name: impl Into<String>, max_bytes: u64, max_entries: u64) -> MedusaResult<Self> {
        let name = bounded_text(name.into(), "resource budget name")?;
        if name.is_empty() || max_bytes == 0 || max_entries == 0 {
            return Err(invalid(
                "resource budget requires a name and non-zero limits",
            ));
        }
        Ok(Self {
            name,
            max_bytes,
            max_entries,
        })
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourcePressure {
    Nominal,
    Warning,
    Critical,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ResourceSnapshot {
    pub budget: ResourceBudget,
    pub bytes: u64,
    pub entries: u64,
    pub pressure: ResourcePressure,
}

impl ResourceSnapshot {
    #[must_use]
    pub fn from_usage(budget: ResourceBudget, bytes: u64, entries: u64) -> Self {
        let byte_ratio = bytes as f64 / budget.max_bytes as f64;
        let entry_ratio = entries as f64 / budget.max_entries as f64;
        let ratio = byte_ratio.max(entry_ratio);
        let pressure = if ratio >= 1.0 {
            ResourcePressure::Critical
        } else if ratio >= 0.8 {
            ResourcePressure::Warning
        } else {
            ResourcePressure::Nominal
        };
        Self {
            budget,
            bytes,
            entries,
            pressure,
        }
    }

    pub fn health_component(&self) -> MedusaResult<HealthComponent> {
        let (status, summary, remediation) = match self.pressure {
            ResourcePressure::Nominal => (
                HealthStatus::HealthyReady,
                "resource usage is within its configured budget",
                None,
            ),
            ResourcePressure::Warning => (
                HealthStatus::DegradedSafe,
                "resource pressure is approaching its configured budget",
                Some(
                    "clean rebuildable caches or reduce queued work before the budget is full"
                        .to_owned(),
                ),
            ),
            ResourcePressure::Critical => (
                HealthStatus::BlockedUserAction,
                "resource budget is full; new non-authoritative work must apply backpressure",
                Some(
                    "free bounded state or increase the configured budget before continuing"
                        .to_owned(),
                ),
            ),
        };
        HealthComponent::new(&self.budget.name, status, summary, remediation)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LivenessDisposition {
    Active,
    SlowButProgressing,
    CancelRecommended,
    RestartRecommended,
    UserInterventionRequired,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LivenessObservation {
    pub id: String,
    pub heartbeat_age_ms: u64,
    pub progress_age_ms: u64,
    pub allowed_idle_ms: u64,
    pub cancellation_supported: bool,
    pub retry_is_idempotent: bool,
}

impl LivenessObservation {
    pub fn new(
        id: impl Into<String>,
        heartbeat_age_ms: u64,
        progress_age_ms: u64,
        allowed_idle_ms: u64,
        cancellation_supported: bool,
        retry_is_idempotent: bool,
    ) -> MedusaResult<Self> {
        let id = bounded_text(id.into(), "liveness id")?;
        if id.is_empty() || allowed_idle_ms == 0 {
            return Err(invalid(
                "liveness observation requires an id and idle bound",
            ));
        }
        Ok(Self {
            id,
            heartbeat_age_ms,
            progress_age_ms,
            allowed_idle_ms,
            cancellation_supported,
            retry_is_idempotent,
        })
    }

    #[must_use]
    pub const fn disposition(&self) -> LivenessDisposition {
        if self.heartbeat_age_ms <= self.allowed_idle_ms
            && self.progress_age_ms <= self.allowed_idle_ms
        {
            LivenessDisposition::Active
        } else if self.progress_age_ms <= self.allowed_idle_ms.saturating_mul(2) {
            LivenessDisposition::SlowButProgressing
        } else if self.cancellation_supported {
            LivenessDisposition::CancelRecommended
        } else if self.retry_is_idempotent {
            LivenessDisposition::RestartRecommended
        } else {
            LivenessDisposition::UserInterventionRequired
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SupportBundle {
    pub schema_version: u16,
    pub generated_at: String,
    pub product: SupportProduct,
    pub resolved_settings: BTreeMap<String, String>,
    pub health: HealthReport,
    pub resources: Vec<ResourceSnapshot>,
    pub events: Vec<SupportEvent>,
    pub excluded: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SupportProduct {
    pub version: String,
    pub commit: Option<String>,
    pub platform: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SupportEvent {
    pub timestamp: String,
    pub level: String,
    pub component: String,
    pub event: String,
    pub correlation_id: String,
    pub fields: BTreeMap<String, serde_json::Value>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SupportBundleManifest {
    pub schema_version: u16,
    pub generated_at: String,
    pub bytes: usize,
    pub component_count: usize,
    pub event_count: usize,
    pub excluded: Vec<String>,
}

impl SupportBundle {
    pub fn new(
        product: SupportProduct,
        resolved_settings: BTreeMap<String, String>,
        health: HealthReport,
        resources: Vec<ResourceSnapshot>,
        events: &[OperationalEvent],
    ) -> MedusaResult<Self> {
        if resources.len() > MAX_COMPONENTS {
            return Err(invalid(
                "support bundle contains too many resource snapshots",
            ));
        }
        let generated_at = OffsetDateTime::now_utc()
            .format(&Rfc3339)
            .map_err(|error| internal(error.to_string()))?;
        let mut bounded_events = events
            .iter()
            .rev()
            .take(MAX_EVENTS)
            .map(sanitize_event)
            .collect::<Vec<_>>();
        bounded_events.reverse();
        Ok(Self {
            schema_version: OPERATIONAL_HEALTH_SCHEMA_VERSION,
            generated_at,
            product: SupportProduct {
                version: bounded_text(product.version, "product version")?,
                commit: product
                    .commit
                    .map(|value| bounded_text(value, "product commit"))
                    .transpose()?,
                platform: bounded_text(product.platform, "product platform")?,
            },
            resolved_settings: sanitize_settings(resolved_settings)?,
            health,
            resources,
            events: bounded_events,
            excluded: vec![
                "credentials, OAuth tokens, and protected environment values".to_owned(),
                "prompts, model responses, hidden reasoning, and raw private content".to_owned(),
                "authoritative journal payloads, repository file contents, and network data"
                    .to_owned(),
            ],
        })
    }

    pub fn write_to(&self, path: &Path) -> MedusaResult<SupportBundleManifest> {
        let bytes = serde_json::to_vec_pretty(self)?;
        if bytes.len() > MAX_BUNDLE_BYTES {
            return Err(invalid(format!(
                "support bundle exceeds {MAX_BUNDLE_BYTES} byte bound"
            )));
        }
        if path
            .extension()
            .is_some_and(|extension| extension != "json")
        {
            return Err(invalid("support bundle path must use the .json extension"));
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, &bytes)?;
        Ok(SupportBundleManifest {
            schema_version: self.schema_version,
            generated_at: self.generated_at.clone(),
            bytes: bytes.len(),
            component_count: self.health.components.len(),
            event_count: self.events.len(),
            excluded: self.excluded.clone(),
        })
    }

    pub fn manifest(&self) -> MedusaResult<SupportBundleManifest> {
        let bytes = serde_json::to_vec(self)?.len();
        Ok(SupportBundleManifest {
            schema_version: self.schema_version,
            generated_at: self.generated_at.clone(),
            bytes,
            component_count: self.health.components.len(),
            event_count: self.events.len(),
            excluded: self.excluded.clone(),
        })
    }
}

fn sanitize_event(event: &OperationalEvent) -> SupportEvent {
    let mut fields = BTreeMap::new();
    for (key, value) in &event.fields {
        let lower = key.to_ascii_lowercase();
        if [
            "prompt",
            "response",
            "reasoning",
            "content",
            "text",
            "message",
            "input",
            "output",
            "token",
            "secret",
            "password",
            "authorization",
            "api_key",
        ]
        .iter()
        .any(|needle| lower.contains(needle))
        {
            continue;
        }
        let mut value = value.clone();
        redact_value(&mut value);
        fields.insert(key.clone(), truncate_json(value));
    }
    SupportEvent {
        timestamp: truncate_lossy(&event.timestamp),
        level: truncate_lossy(&event.level),
        component: truncate_lossy(&event.component),
        event: truncate_lossy(&event.event),
        correlation_id: truncate_lossy(&event.correlation_id),
        fields,
    }
}

fn sanitize_settings(settings: BTreeMap<String, String>) -> MedusaResult<BTreeMap<String, String>> {
    let mut sanitized = BTreeMap::new();
    for (key, value) in settings {
        let key = bounded_text(key, "setting key")?;
        if key.is_empty() {
            continue;
        }
        let lower = key.to_ascii_lowercase();
        let value = if [
            "secret",
            "token",
            "password",
            "authorization",
            "api_key",
            "base_url",
        ]
        .iter()
        .any(|needle| lower.contains(needle))
        {
            "[REDACTED]".to_owned()
        } else {
            bounded_text(value, "setting value")?
        };
        sanitized.insert(key, value);
    }
    Ok(sanitized)
}

fn truncate_json(value: serde_json::Value) -> serde_json::Value {
    let bytes = serde_json::to_vec(&value).unwrap_or_default();
    if bytes.len() <= MAX_EVENT_BYTES {
        value
    } else {
        serde_json::Value::String("[TRUNCATED]".to_owned())
    }
}

fn bounded_text(value: String, field: &str) -> MedusaResult<String> {
    if value.len() > MAX_TEXT_BYTES || value.contains('\0') {
        return Err(invalid(format!(
            "{field} is empty, contains NUL, or exceeds {MAX_TEXT_BYTES} bytes"
        )));
    }
    Ok(value)
}

fn truncate_lossy(value: &str) -> String {
    let mut result = value.chars().take(MAX_TEXT_BYTES).collect::<String>();
    if result.len() < value.len() {
        result.push_str("[TRUNCATED]");
    }
    result
}

fn invalid(message: impl Into<String>) -> MedusaError {
    MedusaError::new(
        ErrorCode::InvalidConfiguration,
        ErrorCategory::Validation,
        message,
    )
}

fn internal(message: impl Into<String>) -> MedusaError {
    MedusaError::new(
        ErrorCode::InternalInvariant,
        ErrorCategory::Internal,
        message,
    )
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use tempfile::tempdir;

    use super::*;

    fn component(id: &str, status: HealthStatus) -> HealthComponent {
        HealthComponent::new(id, status, "evidence", None).expect("component")
    }

    #[test]
    fn aggregation_is_deterministic_and_fail_closed() {
        let report = HealthReport::new(vec![
            component("provider", HealthStatus::DegradedSafe),
            component("journal", HealthStatus::UnsafeQuarantine),
            component("daemon", HealthStatus::HealthyReady),
        ])
        .expect("report");
        assert_eq!(report.status, HealthStatus::UnsafeQuarantine);
        assert!(!report.safe_to_continue);
        assert_eq!(report.components[0].id, "daemon");
    }

    #[test]
    fn resource_pressure_precedes_authoritative_write_failure() {
        let budget = ResourceBudget::new("journal", 100, 10).expect("budget");
        let snapshot = ResourceSnapshot::from_usage(budget, 85, 1);
        assert_eq!(snapshot.pressure, ResourcePressure::Warning);
        assert_eq!(
            snapshot.health_component().expect("health").status,
            HealthStatus::DegradedSafe
        );
        let full = ResourceSnapshot::from_usage(snapshot.budget.clone(), 100, 1);
        assert_eq!(
            full.health_component().expect("health").status,
            HealthStatus::BlockedUserAction
        );
    }

    #[test]
    fn liveness_never_recommends_unsafe_retry() {
        let observation = LivenessObservation::new("tool", 10_000, 10_000, 100, false, false)
            .expect("observation");
        assert_eq!(
            observation.disposition(),
            LivenessDisposition::UserInterventionRequired
        );
        let cancel = LivenessObservation::new("cancel", 10_000, 10_000, 100, true, true)
            .expect("observation");
        assert_eq!(cancel.disposition(), LivenessDisposition::CancelRecommended);
    }

    #[test]
    fn support_bundle_is_bounded_redacted_and_local() {
        let health = HealthReport::new(vec![component(
            "provider",
            HealthStatus::OptionalUnavailable,
        )])
        .expect("health");
        let event = OperationalEvent {
            timestamp: "2026-08-12T00:00:00Z".to_owned(),
            level: "warn".to_owned(),
            component: "provider".to_owned(),
            event: "request_failed".to_owned(),
            correlation_id: "session-1".to_owned(),
            fields: BTreeMap::from([
                ("api_key".to_owned(), serde_json::json!("sk-secret")),
                ("reasoning".to_owned(), serde_json::json!("do not export")),
                ("retry_count".to_owned(), serde_json::json!(2)),
            ]),
        };
        let bundle = SupportBundle::new(
            SupportProduct {
                version: "1.0.0".to_owned(),
                commit: None,
                platform: "test".to_owned(),
            },
            BTreeMap::from([(String::from("provider"), String::from("test"))]),
            health,
            Vec::new(),
            &[event],
        )
        .expect("bundle");
        let directory = tempdir().expect("tempdir");
        let path = directory.path().join("support.json");
        let manifest = bundle.write_to(&path).expect("write");
        let text = fs::read_to_string(path).expect("read");
        assert_eq!(manifest.event_count, 1);
        assert!(!text.contains("sk-secret"));
        assert!(!text.contains("do not export"));
        assert!(text.contains("retry_count"));
        assert!(manifest.bytes <= MAX_BUNDLE_BYTES);
    }
}
