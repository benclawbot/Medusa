//! Threshold-driven operational-health alerting with a Telegram render path.
//!
//! Evaluation lives in `medusa_hardening::evaluate_alerts` (what fires); this
//! module owns the daemon side: deduplicating repeats across polls, emitting a
//! process log, and rendering Telegram-ready message text that the Telegram
//! service layer can forward to operators.

use std::collections::BTreeSet;

use medusa_hardening::{
    AlertSeverity, AlertThresholds, HealthReport, OperationalAlert, ResourceSnapshot,
    evaluate_alerts,
};

use crate::telegram::telegram_markdown_v2;

/// Daemon alert dispatcher: polls health state, deduplicates repeats, and
/// renders newly-firing alerts as Telegram message text.
pub struct OperationalAlertDispatcher {
    thresholds: AlertThresholds,
    delivered: BTreeSet<String>,
}

impl OperationalAlertDispatcher {
    #[must_use]
    pub fn new(thresholds: AlertThresholds) -> Self {
        Self {
            thresholds,
            delivered: BTreeSet::new(),
        }
    }

    #[must_use]
    pub fn with_env_thresholds() -> Self {
        Self::new(AlertThresholds::from_env())
    }

    /// Evaluates health plus capacity pressure and returns newly-firing alerts.
    /// Repeats of an already-delivered alert are suppressed until the alert key
    /// set changes (a clear followed by a re-fire notifies again).
    pub fn poll(
        &mut self,
        report: &HealthReport,
        resources: &[ResourceSnapshot],
    ) -> Vec<OperationalAlert> {
        let fired = evaluate_alerts(report, resources, &self.thresholds);
        let mut fresh = Vec::new();
        for alert in &fired {
            if self.delivered.insert(alert_key(alert)) {
                fresh.push(alert.clone());
            }
        }
        if fired.is_empty() {
            self.delivered.clear();
        }
        for alert in &fresh {
            match alert.severity {
                AlertSeverity::Warning => {
                    tracing::warn!(scope = %alert.scope, message = %alert.message, "operational alert")
                }
                AlertSeverity::Critical => {
                    tracing::error!(scope = %alert.scope, message = %alert.message, "operational alert")
                }
            }
        }
        fresh
    }

    /// Renders newly-firing alerts as Telegram-ready message text.
    pub fn poll_telegram(
        &mut self,
        report: &HealthReport,
        resources: &[ResourceSnapshot],
    ) -> Vec<String> {
        self.poll(report, resources)
            .iter()
            .map(render_telegram_alert)
            .collect()
    }
}

fn alert_key(alert: &OperationalAlert) -> String {
    format!("{:?}:{}:{}", alert.severity, alert.scope, alert.message)
}

/// Renders one alert as Telegram MarkdownV2 text for the operator channel.
#[must_use]
pub fn render_telegram_alert(alert: &OperationalAlert) -> String {
    let header = match alert.severity {
        AlertSeverity::Warning => "⚠️ Medusa operational warning",
        AlertSeverity::Critical => "🚨 Medusa operational alert",
    };
    let body = format!("{header}\nScope: `{}`\n{}", alert.scope, alert.message);
    telegram_markdown_v2(&body)
}

#[cfg(test)]
mod tests {
    use medusa_hardening::{HealthComponent, HealthStatus, ResourceBudget, ResourcePressure};

    use super::*;

    fn report() -> HealthReport {
        HealthReport::new(vec![
            HealthComponent::new("journal", HealthStatus::DegradedSafe, "slow journal", None)
                .expect("component"),
        ])
        .expect("report")
    }

    #[test]
    fn warning_pressure_fires_once_then_dedupes() {
        let mut dispatcher = OperationalAlertDispatcher::new(AlertThresholds::default());
        let budget = ResourceBudget::new("journal", 100, 10).expect("budget");
        let warning = ResourceSnapshot {
            budget,
            bytes: 85,
            entries: 1,
            pressure: ResourcePressure::Warning,
        };
        let first = dispatcher.poll_telegram(&report(), &[warning.clone()]);
        assert_eq!(first.len(), 2);
        assert!(first.iter().all(|message| message.contains("journal")));
        // Re-polling the same state stays silent.
        assert!(dispatcher.poll_telegram(&report(), &[warning]).is_empty());
    }

    #[test]
    fn cleared_alerts_refire_on_return() {
        let mut dispatcher = OperationalAlertDispatcher::new(AlertThresholds::default());
        let healthy = HealthReport::new(vec![
            HealthComponent::new("journal", HealthStatus::HealthyReady, "ok", None)
                .expect("component"),
        ])
        .expect("report");
        assert_eq!(dispatcher.poll(&report(), &[]).len(), 1);
        assert!(dispatcher.poll(&healthy, &[]).is_empty());
        assert_eq!(dispatcher.poll(&report(), &[]).len(), 1);
    }
}
