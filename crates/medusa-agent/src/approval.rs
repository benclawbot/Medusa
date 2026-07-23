use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::session::AgentPlanStep;

const DEFAULT_GRANT_TTL_SECONDS: i64 = 300;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ApprovalScope {
    pub tool: String,
    pub action_fingerprint: String,
    pub plan_fingerprint: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ApprovalGrant {
    pub scope: ApprovalScope,
    #[serde(with = "time::serde::rfc3339")]
    pub approved_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub expires_at: OffsetDateTime,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalDecision {
    Approved,
    Denied,
    Expired,
    Invalidated,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ApprovalReceipt {
    pub decision: ApprovalDecision,
    pub scope: ApprovalScope,
    #[serde(with = "time::serde::rfc3339")]
    pub recorded_at: OffsetDateTime,
    pub reason: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RollbackOutcome {
    NotRequired,
    Completed,
    Failed,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RollbackReceipt {
    pub checkpoint: String,
    pub affected_files: Vec<String>,
    pub outcome: RollbackOutcome,
    #[serde(with = "time::serde::rfc3339")]
    pub recorded_at: OffsetDateTime,
    pub detail: String,
}

impl ApprovalGrant {
    #[must_use]
    pub fn exact_action(
        tool: &str,
        input: &serde_json::Value,
        plan: &[AgentPlanStep],
        now: OffsetDateTime,
    ) -> Self {
        Self {
            scope: ApprovalScope {
                tool: tool.to_owned(),
                action_fingerprint: action_fingerprint(tool, input),
                plan_fingerprint: plan_fingerprint(plan),
            },
            approved_at: now,
            expires_at: now + time::Duration::seconds(DEFAULT_GRANT_TTL_SECONDS),
        }
    }

    #[must_use]
    pub fn authorizes(
        &self,
        tool: &str,
        input: &serde_json::Value,
        plan: &[AgentPlanStep],
        now: OffsetDateTime,
    ) -> ApprovalDecision {
        if now > self.expires_at {
            return ApprovalDecision::Expired;
        }
        if self.scope.plan_fingerprint != plan_fingerprint(plan) {
            return ApprovalDecision::Invalidated;
        }
        if self.scope.tool != tool
            || self.scope.action_fingerprint != action_fingerprint(tool, input)
        {
            return ApprovalDecision::Denied;
        }
        ApprovalDecision::Approved
    }
}

#[must_use]
pub fn action_fingerprint(tool: &str, input: &serde_json::Value) -> String {
    stable_fingerprint(&format!("{tool}\n{}", canonical_json(input)))
}

#[must_use]
pub fn plan_fingerprint(plan: &[AgentPlanStep]) -> String {
    let encoded = serde_json::to_string(plan).unwrap_or_else(|_| "[]".to_owned());
    stable_fingerprint(&encoded)
}

fn canonical_json(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Object(map) => {
            let mut entries = map.iter().collect::<Vec<_>>();
            entries.sort_by(|left, right| left.0.cmp(right.0));
            let body = entries
                .into_iter()
                .map(|(key, value)| {
                    format!(
                        "{}:{}",
                        serde_json::to_string(key).unwrap_or_default(),
                        canonical_json(value)
                    )
                })
                .collect::<Vec<_>>()
                .join(",");
            format!("{{{body}}}")
        }
        serde_json::Value::Array(values) => {
            let body = values
                .iter()
                .map(canonical_json)
                .collect::<Vec<_>>()
                .join(",");
            format!("[{body}]")
        }
        _ => serde_json::to_string(value).unwrap_or_default(),
    }
}

fn stable_fingerprint(value: &str) -> String {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in value.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use time::macros::datetime;

    use super::*;
    use crate::session::AgentPlanStepStatus;

    fn plan(title: &str) -> Vec<AgentPlanStep> {
        vec![AgentPlanStep {
            title: title.to_owned(),
            status: AgentPlanStepStatus::InProgress,
        }]
    }

    #[test]
    fn object_key_order_does_not_change_action_fingerprint() {
        let left = json!({"path": "src/lib.rs", "content": "fixed"});
        let right = json!({"content": "fixed", "path": "src/lib.rs"});
        assert_eq!(
            action_fingerprint("fs_write", &left),
            action_fingerprint("fs_write", &right)
        );
    }

    #[test]
    fn grant_is_bound_to_exact_tool_payload_and_plan() {
        let now = datetime!(2026-07-23 06:00 UTC);
        let input = json!({"path": "src/lib.rs", "content": "fixed"});
        let current_plan = plan("Apply the fix");
        let grant = ApprovalGrant::exact_action("fs_write", &input, &current_plan, now);

        assert_eq!(
            grant.authorizes("fs_write", &input, &current_plan, now),
            ApprovalDecision::Approved
        );
        assert_eq!(
            grant.authorizes(
                "fs_write",
                &json!({"path": "src/lib.rs", "content": "substituted"}),
                &current_plan,
                now
            ),
            ApprovalDecision::Denied
        );
        assert_eq!(
            grant.authorizes("shell_run", &input, &current_plan, now),
            ApprovalDecision::Denied
        );
    }

    #[test]
    fn modified_plan_invalidates_prior_grant() {
        let now = datetime!(2026-07-23 06:00 UTC);
        let input = json!({"path": "src/lib.rs", "content": "fixed"});
        let grant = ApprovalGrant::exact_action("fs_write", &input, &plan("Apply the fix"), now);

        assert_eq!(
            grant.authorizes("fs_write", &input, &plan("Rewrite the subsystem"), now),
            ApprovalDecision::Invalidated
        );
    }

    #[test]
    fn expired_grant_cannot_authorize_action() {
        let now = datetime!(2026-07-23 06:00 UTC);
        let input = json!({"path": "src/lib.rs", "content": "fixed"});
        let current_plan = plan("Apply the fix");
        let grant = ApprovalGrant::exact_action("fs_write", &input, &current_plan, now);

        assert_eq!(
            grant.authorizes(
                "fs_write",
                &input,
                &current_plan,
                now + time::Duration::seconds(DEFAULT_GRANT_TTL_SECONDS + 1)
            ),
            ApprovalDecision::Expired
        );
    }
}
