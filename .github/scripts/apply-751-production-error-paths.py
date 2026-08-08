from pathlib import Path


def replace(path: str, old: str, new: str, count: int = 1) -> None:
    p = Path(path)
    text = p.read_text()
    actual = text.count(old)
    if actual < count:
        raise SystemExit(f"expected at least {count} matches in {path}, found {actual}")
    p.write_text(text.replace(old, new, count))

manager = "crates/medusa-provider/src/manager.rs"

replace(
    manager,
    """                    self.latency\n                        .record_failure(candidate.index, candidate.duration_ms)?;\n                    self.record_error(candidate.index, error)?;""",
    """                    self.latency.record_failure_with_category(\n                        candidate.index,\n                        candidate.duration_ms,\n                        Some(error.category),\n                    )?;\n                    self.record_error(candidate.index, error)?;""",
    1,
)

replace(
    manager,
    """            self.latency\n                .record_failure(candidate.index, candidate.duration_ms)?;\n            self.record_error(candidate.index, error)?;""",
    """            self.latency.record_failure_with_category(\n                candidate.index,\n                candidate.duration_ms,\n                Some(error.category),\n            )?;\n            self.record_error(candidate.index, error)?;""",
    1,
)

replace(
    manager,
    """                    Err(error) => {\n                        if route_stream_started {\n                            return Err(error);\n                        }\n                        let duration_ms = elapsed_ms(started);\n                        self.latency.record_failure_with_category(\n                            index,\n                            duration_ms,\n                            Some(error.category),\n                        )?;\n                        self.record_error(index, &error)?;""",
    """                    Err(error) => {\n                        let duration_ms = elapsed_ms(started);\n                        self.latency.record_failure_with_category(\n                            index,\n                            duration_ms,\n                            Some(error.category),\n                        )?;\n                        self.record_error(index, &error)?;\n                        if route_stream_started {\n                            return Err(error);\n                        }""",
    1,
)

replace(
    manager,
    """        assert_eq!(manager.route_latency()[0].failures, 2);\n        assert_eq!(manager.route_latency()[0].retry_attempts, 1);""",
    """        assert_eq!(manager.route_latency()[0].failures, 2);\n        assert_eq!(manager.route_latency()[0].transient_errors, 2);\n        assert_eq!(manager.route_latency()[0].retry_attempts, 1);""",
    1,
)

replace(
    manager,
    """        assert_eq!(manager.last_completed_provider(), None);\n        assert_eq!(manager.execution_status(), None);\n    }\n\n    #[test]\n    fn environment_failure_fails_over_without_retry()""",
    """        assert_eq!(manager.last_completed_provider(), None);\n        assert_eq!(manager.execution_status(), None);\n        assert_eq!(manager.route_latency()[0].validation_errors, 1);\n    }\n\n    #[test]\n    fn environment_failure_fails_over_without_retry()""",
    1,
)

replace(
    manager,
    """        assert_eq!(manager.health()[0].failovers, 1);\n        assert_eq!(manager.last_completed_provider(), Some(1));\n        let status = manager.execution_status().expect(\"execution status\");""",
    """        assert_eq!(manager.health()[0].failovers, 1);\n        assert_eq!(manager.last_completed_provider(), Some(1));\n        assert_eq!(manager.route_latency()[0].environment_errors, 1);\n        let status = manager.execution_status().expect(\"execution status\");""",
    1,
)
