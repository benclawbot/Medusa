use std::collections::{BTreeMap, BTreeSet};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct ExecutionBudget {
    pub max_tool_calls: usize,
    pub max_parallel_reads: usize,
    pub max_retained_output_bytes: usize,
    pub max_identical_calls: usize,
    pub reserve_verification_calls: usize,
    pub used_tool_calls: usize,
    pub retained_output_bytes: usize,
    pub verification_calls: usize,
    seen_calls: BTreeMap<String, usize>,
    pending_mutations: BTreeSet<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct SchedulingDecision {
    pub batch_size: usize,
    pub parallel: bool,
    pub reason: String,
    pub remaining_tool_calls: usize,
    pub remaining_output_bytes: usize,
    pub verification_reserved: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct VerificationDecision {
    pub status: String,
    pub mutation: String,
    pub required: bool,
    pub reason: String,
}

impl ExecutionBudget {
    pub(crate) fn for_turn(configured_workers: u16) -> Self {
        let max_parallel_reads = usize::from(configured_workers).clamp(1, 8);
        Self {
            max_tool_calls: 32,
            max_parallel_reads,
            max_retained_output_bytes: 512 * 1024,
            max_identical_calls: 1,
            reserve_verification_calls: 2,
            used_tool_calls: 0,
            retained_output_bytes: 0,
            verification_calls: 0,
            seen_calls: BTreeMap::new(),
            pending_mutations: BTreeSet::new(),
        }
    }

    pub(crate) fn schedule_batch(
        &self,
        remaining_calls: usize,
        leading_parallel_safe: usize,
    ) -> MedusaResult<SchedulingDecision> {
        let remaining_budget = self.max_tool_calls.saturating_sub(self.used_tool_calls);
        let executable_budget = remaining_budget.saturating_sub(self.reserve_verification_calls);
        if executable_budget == 0 {
            return Err(MedusaError::new(
                ErrorCode::ToolExecutionFailed,
                ErrorCategory::Execution,
                "tool-call budget exhausted with verification capacity reserved",
            ));
        }
        let parallel = leading_parallel_safe > 1;
        let requested = if parallel {
            leading_parallel_safe.min(self.max_parallel_reads)
        } else {
            1
        };
        let batch_size = requested.min(remaining_calls).min(executable_budget).max(1);
        Ok(SchedulingDecision {
            batch_size,
            parallel: parallel && batch_size > 1,
            reason: if parallel && batch_size > 1 {
                "independent read-only calls share a dependency frontier".into()
            } else {
                "mutation, unsafe call, or dependency boundary requires serialization".into()
            },
            remaining_tool_calls: remaining_budget,
            remaining_output_bytes: self
                .max_retained_output_bytes
                .saturating_sub(self.retained_output_bytes),
            verification_reserved: self.reserve_verification_calls,
        })
    }

    pub(crate) fn before_call(
        &mut self,
        tool: &str,
        input: &serde_json::Value,
    ) -> MedusaResult<String> {
        if self.used_tool_calls >= self.max_tool_calls {
            return Err(MedusaError::new(
                ErrorCode::ToolExecutionFailed,
                ErrorCategory::Execution,
                "tool-call budget exhausted",
            ));
        }
        let digest = call_digest(tool, input)?;
        let count = self.seen_calls.entry(digest.clone()).or_default();
        if *count >= self.max_identical_calls {
            return Err(MedusaError::new(
                ErrorCode::ToolExecutionFailed,
                ErrorCategory::Execution,
                format!("unchanged repeated tool call blocked by loop guard: {tool}"),
            ));
        }
        *count += 1;
        self.used_tool_calls += 1;
        if is_mutation(tool) {
            self.pending_mutations.insert(digest.clone());
        }
        Ok(digest)
    }

    pub(crate) fn record_output(&mut self, output: &str) -> MedusaResult<()> {
        self.retained_output_bytes = self.retained_output_bytes.saturating_add(output.len());
        if self.retained_output_bytes > self.max_retained_output_bytes {
            return Err(MedusaError::new(
                ErrorCode::ToolExecutionFailed,
                ErrorCategory::Execution,
                "combined retained tool-output budget exhausted",
            ));
        }
        Ok(())
    }

    pub(crate) fn verification_for(
        &mut self,
        tool: &str,
        digest: &str,
        succeeded: bool,
    ) -> VerificationDecision {
        let required = is_mutation(tool) && succeeded;
        if required {
            self.verification_calls = self.verification_calls.saturating_add(1);
        }
        let status = if required {
            self.pending_mutations.remove(digest);
            "required"
        } else if is_mutation(tool) {
            "skipped"
        } else {
            "not_applicable"
        };
        VerificationDecision {
            status: status.into(),
            mutation: tool.into(),
            required,
            reason: if required {
                "successful repository mutation requires proportional targeted verification".into()
            } else if is_mutation(tool) {
                "failed mutation produced no new repository state to verify".into()
            } else {
                "read-only call does not create verification debt".into()
            },
        }
    }

    pub(crate) fn format_schedule(decision: &SchedulingDecision) -> String {
        format!(
            "[tool-scheduler batch_size={}; parallel={}; remaining_calls={}; remaining_output_bytes={}; verification_reserved={}; reason={}]",
            decision.batch_size,
            decision.parallel,
            decision.remaining_tool_calls,
            decision.remaining_output_bytes,
            decision.verification_reserved,
            decision.reason
        )
    }

    pub(crate) fn format_verification(decision: &VerificationDecision) -> String {
        format!(
            "[tool-verification status={}; mutation={}; required={}; reason={}]",
            decision.status, decision.mutation, decision.required, decision.reason
        )
    }
}

fn is_mutation(tool: &str) -> bool {
    matches!(
        tool,
        "fs_write" | "fs_create_dir" | "patch_apply" | "symbol_rename"
    )
}

fn call_digest(tool: &str, input: &serde_json::Value) -> MedusaResult<String> {
    let mut hasher = Sha256::new();
    hasher.update(tool.as_bytes());
    hasher.update([0]);
    hasher.update(serde_json::to_vec(input)?);
    Ok(hex::encode(&hasher.finalize()[..8]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn independent_reads_share_one_bounded_batch() {
        let budget = ExecutionBudget::for_turn(4);
        let decision = budget.schedule_batch(5, 3).expect("schedule reads");
        assert_eq!(decision.batch_size, 3);
        assert!(decision.parallel);
    }

    #[test]
    fn mutations_are_serialized_and_require_verification() {
        let mut budget = ExecutionBudget::for_turn(4);
        let decision = budget.schedule_batch(2, 0).expect("serialize mutation");
        assert_eq!(decision.batch_size, 1);
        assert!(!decision.parallel);
        let digest = budget
            .before_call("fs_write", &json!({"path":"value.txt","content":"42"}))
            .expect("first mutation");
        let verification = budget.verification_for("fs_write", &digest, true);
        assert!(verification.required);
        assert_eq!(verification.status, "required");
    }

    #[test]
    fn identical_calls_are_blocked_without_new_evidence() {
        let mut budget = ExecutionBudget::for_turn(1);
        let input = json!({"path":"Cargo.toml"});
        budget.before_call("fs_read", &input).expect("first read");
        let error = budget
            .before_call("fs_read", &input)
            .expect_err("duplicate must fail");
        assert!(error.to_string().contains("loop guard"));
    }
}
