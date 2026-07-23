mod session {
    pub use medusa_agent::AgentPlanStep;
}

#[path = "../src/approval.rs"]
mod approval;

use approval::{ApprovalDecision, ApprovalGrant};
use medusa_agent::{AgentPlanStep, AgentPlanStepStatus};
use serde_json::json;
use time::macros::datetime;

fn plan(title: &str) -> Vec<AgentPlanStep> {
    vec![AgentPlanStep {
        title: title.to_owned(),
        status: AgentPlanStepStatus::InProgress,
    }]
}

#[test]
fn exact_grant_rejects_payload_substitution() {
    let now = datetime!(2026-07-23 06:00 UTC);
    let original = json!({"path": "src/lib.rs", "content": "safe"});
    let grant = ApprovalGrant::exact_action("fs_write", &original, &plan("Apply fix"), now);
    assert_eq!(
        grant.authorizes(
            "fs_write",
            &json!({"path": "src/lib.rs", "content": "substituted"}),
            &plan("Apply fix"),
            now,
        ),
        ApprovalDecision::Denied
    );
}

#[test]
fn changed_plan_invalidates_approval() {
    let now = datetime!(2026-07-23 06:00 UTC);
    let input = json!({"path": "src/lib.rs", "content": "safe"});
    let grant = ApprovalGrant::exact_action("fs_write", &input, &plan("Apply fix"), now);
    assert_eq!(
        grant.authorizes("fs_write", &input, &plan("Rewrite subsystem"), now),
        ApprovalDecision::Invalidated
    );
}
