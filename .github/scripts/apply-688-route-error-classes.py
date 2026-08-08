from pathlib import Path


def replace(path: str, old: str, new: str) -> None:
    p = Path(path)
    text = p.read_text()
    if old not in text:
        raise SystemExit(f"missing expected block in {path}")
    p.write_text(text.replace(old, new, 1))

replace(
    "crates/medusa-provider/src/route_latency.rs",
    """    #[serde(default)]\n    pub retry_recoveries: u64,\n}""",
    """    #[serde(default)]\n    pub retry_recoveries: u64,\n    #[serde(default)]\n    pub validation_errors: u64,\n    #[serde(default)]\n    pub policy_errors: u64,\n    #[serde(default)]\n    pub environment_errors: u64,\n    #[serde(default)]\n    pub execution_errors: u64,\n    #[serde(default)]\n    pub transient_errors: u64,\n    #[serde(default)]\n    pub persistence_errors: u64,\n    #[serde(default)]\n    pub internal_errors: u64,\n}""",
)

replace(
    "crates/medusa-provider/src/route_metrics_store.rs",
    """    pub fn record_failure(&self, index: usize, duration_ms: u64) -> MedusaResult<()> {\n        self.update(index, |stats| {\n            stats.samples = stats.samples.saturating_add(1);\n            stats.failures = stats.failures.saturating_add(1);\n            stats.total_duration_ms = stats.total_duration_ms.saturating_add(duration_ms);\n        })\n    }""",
    """    pub fn record_failure(&self, index: usize, duration_ms: u64) -> MedusaResult<()> {\n        self.record_failure_with_category(index, duration_ms, None)\n    }\n\n    pub fn record_failure_with_category(\n        &self,\n        index: usize,\n        duration_ms: u64,\n        category: Option<ErrorCategory>,\n    ) -> MedusaResult<()> {\n        self.update(index, |stats| {\n            stats.samples = stats.samples.saturating_add(1);\n            stats.failures = stats.failures.saturating_add(1);\n            stats.total_duration_ms = stats.total_duration_ms.saturating_add(duration_ms);\n            match category {\n                Some(ErrorCategory::Validation) => {\n                    stats.validation_errors = stats.validation_errors.saturating_add(1);\n                }\n                Some(ErrorCategory::Policy) => {\n                    stats.policy_errors = stats.policy_errors.saturating_add(1);\n                }\n                Some(ErrorCategory::Environment) => {\n                    stats.environment_errors = stats.environment_errors.saturating_add(1);\n                }\n                Some(ErrorCategory::Execution) => {\n                    stats.execution_errors = stats.execution_errors.saturating_add(1);\n                }\n                Some(ErrorCategory::Transient) => {\n                    stats.transient_errors = stats.transient_errors.saturating_add(1);\n                }\n                Some(ErrorCategory::Persistence) => {\n                    stats.persistence_errors = stats.persistence_errors.saturating_add(1);\n                }\n                Some(ErrorCategory::Internal) => {\n                    stats.internal_errors = stats.internal_errors.saturating_add(1);\n                }\n                None => {}\n            }\n        })\n    }""",
)

replace(
    "crates/medusa-provider/src/manager.rs",
    """                        self.latency.record_failure(index, duration_ms)?;\n                        self.record_error(index, &error)?;""",
    """                        self.latency.record_failure_with_category(\n                            index,\n                            duration_ms,\n                            Some(error.category),\n                        )?;\n                        self.record_error(index, &error)?;""",
)

replace(
    "crates/medusa-provider/src/route_metrics_store.rs",
    """        assert_eq!(stats.output_tokens, 0);\n        assert_eq!(stats.generation_total_ms, 0);\n\n        let changed""",
    """        assert_eq!(stats.output_tokens, 0);\n        assert_eq!(stats.generation_total_ms, 0);\n\n        first\n            .record_failure_with_category(0, 25, Some(ErrorCategory::Transient))\n            .expect(\"record categorized failure\");\n        first\n            .record_failure_with_category(0, 30, Some(ErrorCategory::Environment))\n            .expect(\"record environment failure\");\n        let categorized = second.stats().expect(\"categorized stats\")[0];\n        assert_eq!(categorized.failures, 2);\n        assert_eq!(categorized.transient_errors, 1);\n        assert_eq!(categorized.environment_errors, 1);\n        assert_eq!(categorized.execution_errors, 0);\n\n        let changed""",
)
