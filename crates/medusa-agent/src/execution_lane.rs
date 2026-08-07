use serde::{Deserialize, Serialize};

/// Authoritative execution lane selected before mutation begins.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionLane {
    Instant,
    FastMutation,
    StandardMutation,
    FullOrchestration,
}

/// Deterministic inputs used to select an execution lane.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct ExecutionLaneInput {
    pub mutating: bool,
    pub scope_resolved: bool,
    pub changed_path_count: usize,
    pub package_count: usize,
    pub security_sensitive: bool,
    pub migration_or_release: bool,
    pub public_api_risk: bool,
    pub dependency_change: bool,
    pub generated_file_risk: bool,
    pub repository_wide: bool,
    pub ambiguous: bool,
    pub historical_failures: u32,
    pub confidence_milli: u16,
    pub repository_forces_full: bool,
}

/// Durable evidence attached to the selected lane.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ExecutionLaneDecision {
    pub lane: ExecutionLane,
    pub rationale: Vec<String>,
    pub confidence_milli: u16,
    pub max_model_requests_before_first_edit: u8,
    pub max_model_requests_success_path: u8,
}

impl ExecutionLaneDecision {
    #[must_use]
    pub fn requires_model_review(&self) -> bool {
        matches!(
            self.lane,
            ExecutionLane::StandardMutation | ExecutionLane::FullOrchestration
        )
    }
}

/// Select the narrowest lane allowed by deterministic safety/risk evidence.
#[must_use]
pub fn select_execution_lane(input: &ExecutionLaneInput) -> ExecutionLaneDecision {
    let confidence = input.confidence_milli.min(1000);
    if !input.mutating {
        return ExecutionLaneDecision {
            lane: ExecutionLane::Instant,
            rationale: vec!["objective is non-mutating".to_owned()],
            confidence_milli: confidence,
            max_model_requests_before_first_edit: 0,
            max_model_requests_success_path: 1,
        };
    }

    let full_reasons = [
        (
            input.repository_forces_full,
            "repository policy requires full orchestration",
        ),
        (
            !input.scope_resolved || input.ambiguous,
            "write scope is unresolved or ambiguous",
        ),
        (input.security_sensitive, "security-sensitive change"),
        (input.migration_or_release, "migration or release change"),
        (input.repository_wide, "repository-wide change"),
        (input.public_api_risk, "public API risk"),
        (input.dependency_change, "dependency graph mutation"),
        (input.generated_file_risk, "generated-file risk"),
        (input.package_count > 1, "multi-package change"),
        (input.changed_path_count > 8, "broad changed-path scope"),
        (
            input.historical_failures >= 2,
            "repeated historical failures",
        ),
        (confidence < 700, "insufficient confidence for a fast lane"),
    ];
    let rationale = full_reasons
        .into_iter()
        .filter_map(|(active, reason)| active.then(|| reason.to_owned()))
        .collect::<Vec<_>>();
    if !rationale.is_empty() {
        return ExecutionLaneDecision {
            lane: ExecutionLane::FullOrchestration,
            rationale,
            confidence_milli: confidence,
            max_model_requests_before_first_edit: 1,
            max_model_requests_success_path: 4,
        };
    }

    if input.changed_path_count <= 1 && input.package_count <= 1 && confidence >= 900 {
        return ExecutionLaneDecision {
            lane: ExecutionLane::FastMutation,
            rationale: vec!["localized resolved-scope low-risk mutation".to_owned()],
            confidence_milli: confidence,
            max_model_requests_before_first_edit: 1,
            max_model_requests_success_path: 2,
        };
    }

    ExecutionLaneDecision {
        lane: ExecutionLane::StandardMutation,
        rationale: vec!["resolved medium-risk mutation".to_owned()],
        confidence_milli: confidence,
        max_model_requests_before_first_edit: 1,
        max_model_requests_success_path: 3,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_mutating_work_selects_instant() {
        let decision = select_execution_lane(&ExecutionLaneInput {
            confidence_milli: 1000,
            ..ExecutionLaneInput::default()
        });
        assert_eq!(decision.lane, ExecutionLane::Instant);
        assert_eq!(decision.max_model_requests_before_first_edit, 0);
    }

    #[test]
    fn localized_low_risk_mutation_selects_fast_lane() {
        let decision = select_execution_lane(&ExecutionLaneInput {
            mutating: true,
            scope_resolved: true,
            changed_path_count: 1,
            package_count: 1,
            confidence_milli: 950,
            ..ExecutionLaneInput::default()
        });
        assert_eq!(decision.lane, ExecutionLane::FastMutation);
        assert_eq!(decision.max_model_requests_before_first_edit, 1);
        assert_eq!(decision.max_model_requests_success_path, 2);
        assert!(!decision.requires_model_review());
    }

    #[test]
    fn medium_risk_related_file_work_selects_standard_lane() {
        let decision = select_execution_lane(&ExecutionLaneInput {
            mutating: true,
            scope_resolved: true,
            changed_path_count: 3,
            package_count: 1,
            confidence_milli: 850,
            ..ExecutionLaneInput::default()
        });
        assert_eq!(decision.lane, ExecutionLane::StandardMutation);
        assert!(decision.requires_model_review());
    }

    #[test]
    fn high_risk_or_ambiguous_work_selects_full_orchestration() {
        for input in [
            ExecutionLaneInput {
                mutating: true,
                scope_resolved: false,
                confidence_milli: 1000,
                ..ExecutionLaneInput::default()
            },
            ExecutionLaneInput {
                mutating: true,
                scope_resolved: true,
                security_sensitive: true,
                confidence_milli: 1000,
                ..ExecutionLaneInput::default()
            },
            ExecutionLaneInput {
                mutating: true,
                scope_resolved: true,
                migration_or_release: true,
                confidence_milli: 1000,
                ..ExecutionLaneInput::default()
            },
            ExecutionLaneInput {
                mutating: true,
                scope_resolved: true,
                package_count: 2,
                confidence_milli: 1000,
                ..ExecutionLaneInput::default()
            },
        ] {
            assert_eq!(
                select_execution_lane(&input).lane,
                ExecutionLane::FullOrchestration
            );
        }
    }

    #[test]
    fn low_confidence_never_selects_fast_lane() {
        let decision = select_execution_lane(&ExecutionLaneInput {
            mutating: true,
            scope_resolved: true,
            changed_path_count: 1,
            package_count: 1,
            confidence_milli: 699,
            ..ExecutionLaneInput::default()
        });
        assert_eq!(decision.lane, ExecutionLane::FullOrchestration);
    }
}
