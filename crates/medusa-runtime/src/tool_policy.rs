use std::collections::BTreeSet;

use medusa_tool_control::{VerificationPlan, VerificationRequirement};
use medusa_tool_policy::{
    OutputMode, ToolBudgets, ToolEffect, ToolEstimate, ToolMetadata, ToolRecommendation,
    ToolRegistry, VerificationLevel,
};

use crate::prompt::PromptDraft;

/// Explicit verification level of the preferred tool for a draft, if any
/// tool is eligible. `None` means no registered tool claimed the draft, so
/// there is no explicit metadata to apply.
pub(crate) fn preferred_verification_level(draft: &PromptDraft) -> Option<VerificationLevel> {
    let (registry, selected) = select_preferred(draft).ok()?;
    registry
        .get(&selected.tool)
        .map(|metadata| metadata.verification)
}

/// Resolves the effective verification plan when explicit tool metadata
/// disagrees with the keyword-derived plan.
///
/// Precedence rule: explicit metadata wins. A `Some(level)` selects exactly
/// the requirement stages implied by that level, superseding the
/// keyword-derived plan when they disagree; `None` (no eligible tool) keeps
/// the keyword plan unchanged. Each [`VerificationLevel`] implies its stage
/// plus every narrower stage, so escalation is monotonic and suppression is
/// always traceable in the returned rationale.
pub(crate) fn resolve_verification(
    explicit: Option<VerificationLevel>,
    keyword: &VerificationPlan,
) -> VerificationPlan {
    let Some(level) = explicit else {
        return keyword.clone();
    };
    let requirements = requirements_for_level(level);
    let mut rationale = vec![format!(
        "explicit tool metadata requires {level:?} verification; keyword-derived plan superseded"
    )];
    if keyword.requirements == requirements {
        rationale.push("keyword-derived plan already matches the explicit level".to_owned());
    }
    VerificationPlan {
        requirements,
        rationale,
    }
}

fn requirements_for_level(level: VerificationLevel) -> Vec<VerificationRequirement> {
    match level {
        VerificationLevel::None => Vec::new(),
        VerificationLevel::Syntax => vec![VerificationRequirement::Syntax],
        VerificationLevel::Focused => vec![
            VerificationRequirement::Syntax,
            VerificationRequirement::Focused,
        ],
        VerificationLevel::Package => vec![
            VerificationRequirement::Syntax,
            VerificationRequirement::Focused,
            VerificationRequirement::Package,
        ],
        VerificationLevel::Workspace => vec![
            VerificationRequirement::Syntax,
            VerificationRequirement::Focused,
            VerificationRequirement::Package,
            VerificationRequirement::Workspace,
        ],
    }
}

fn select_preferred(
    draft: &PromptDraft,
) -> Result<(ToolRegistry, ToolRecommendation), &'static str> {
    let intent = classify_intent(&draft.text);
    let mut registry = ToolRegistry::default();
    for metadata in default_tools() {
        registry.register(metadata)?;
    }

    let recommendations = registry.recommend(
        intent,
        &default_budgets(),
        true,
        true,
        OutputMode::Compact,
        &BTreeSet::new(),
    );
    let selected = recommendations
        .first()
        .ok_or("runtime tool policy produced no eligible recommendation")?
        .clone();
    Ok((registry, selected))
}

pub(crate) fn runtime_context(draft: &PromptDraft) -> Result<String, &'static str> {
    let intent = classify_intent(&draft.text);
    let (_, selected) = select_preferred(draft)?;

    Ok(format!(
        "Tool orchestration policy: intent={intent}; preferred={}; output_mode={:?}; alternatives={:?}. Use the narrowest eligible tool first, preserve complete failure evidence, expand compact output only when necessary, and reserve capacity for verification and the final answer.",
        selected.tool, selected.output_mode, selected.alternatives
    ))
}

fn classify_intent(text: &str) -> &'static str {
    let lower = text.to_ascii_lowercase();
    if ["test", "verify", "ci", "lint", "clippy"]
        .iter()
        .any(|marker| lower.contains(marker))
    {
        "verify"
    } else if ["implement", "fix", "edit", "refactor", "change"]
        .iter()
        .any(|marker| lower.contains(marker))
    {
        "modify"
    } else {
        "inspect"
    }
}

fn default_tools() -> Vec<ToolMetadata> {
    vec![
        metadata(
            "focused-inspection",
            ["inspect"],
            ToolEffect::ReadOnly,
            40,
            120,
            VerificationLevel::None,
        ),
        metadata(
            "repository-search",
            ["inspect", "modify"],
            ToolEffect::ReadOnly,
            80,
            220,
            VerificationLevel::None,
        ),
        metadata(
            "scoped-edit",
            ["modify"],
            ToolEffect::Mutating,
            120,
            180,
            VerificationLevel::Focused,
        ),
        metadata(
            "focused-verification",
            ["verify", "modify"],
            ToolEffect::ReadOnly,
            300,
            300,
            VerificationLevel::Focused,
        ),
        metadata(
            "workspace-verification",
            ["verify"],
            ToolEffect::ReadOnly,
            1_500,
            600,
            VerificationLevel::Workspace,
        ),
    ]
}

fn metadata<const N: usize>(
    name: &str,
    intents: [&str; N],
    effect: ToolEffect,
    latency_ms: u64,
    output_tokens: u64,
    verification: VerificationLevel,
) -> ToolMetadata {
    ToolMetadata {
        name: name.to_owned(),
        intents: intents.into_iter().map(str::to_owned).collect(),
        effect,
        estimate: ToolEstimate {
            latency_ms,
            input_tokens: 40,
            output_tokens,
            retained_tokens: output_tokens,
            monetary_microunits: 0,
            success_probability: 0.9,
        },
        requires_network: false,
        cancellable: true,
        idempotent: effect == ToolEffect::ReadOnly,
        concurrency_group: None,
        max_output_bytes: 64 * 1024,
        output_modes: BTreeSet::from([OutputMode::Compact, OutputMode::Normal]),
        fallback_tools: Vec::new(),
        cacheable: effect == ToolEffect::ReadOnly,
        verification,
    }
}

fn default_budgets() -> ToolBudgets {
    ToolBudgets {
        remaining_calls: 16,
        remaining_time_ms: 120_000,
        remaining_input_tokens: 32_000,
        remaining_output_tokens: 16_000,
        remaining_monetary_microunits: u64::MAX,
        reserved_verification_calls: 2,
        reserved_final_tokens: 1_000,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modification_prompts_receive_scoped_policy_context() {
        let draft = PromptDraft {
            text: "Fix the runtime implementation and run tests".to_owned(),
            ..PromptDraft::default()
        };
        let context = runtime_context(&draft).expect("tool policy context");
        assert!(context.contains("intent=verify"));
        assert!(context.contains("focused-verification"));
        assert!(context.contains("reserve capacity for verification"));
    }

    #[test]
    fn explicit_workspace_metadata_escalates_keyword_plan() {
        use medusa_tool_control::verification_plan;
        // "explain" derives a syntax-only keyword plan, but explicit
        // workspace metadata wins.
        let keyword = verification_plan("Explain how this module works");
        assert!(
            !keyword
                .requirements
                .contains(&medusa_tool_control::VerificationRequirement::Workspace)
        );
        let resolved = resolve_verification(Some(VerificationLevel::Workspace), &keyword);
        assert!(
            resolved
                .requirements
                .contains(&medusa_tool_control::VerificationRequirement::Workspace)
        );
        assert!(
            resolved
                .rationale
                .iter()
                .any(|entry| entry.contains("explicit"))
        );
    }

    #[test]
    fn explicit_none_metadata_supersedes_keyword_plan() {
        use medusa_tool_control::verification_plan;
        let keyword = verification_plan("Fix the workspace release and run all CI tests");
        assert!(!keyword.requirements.is_empty());
        let resolved = resolve_verification(Some(VerificationLevel::None), &keyword);
        assert!(resolved.requirements.is_empty());
    }

    #[test]
    fn missing_metadata_keeps_keyword_plan() {
        use medusa_tool_control::verification_plan;
        let keyword = verification_plan("Fix the implementation");
        let resolved = resolve_verification(None, &keyword);
        assert_eq!(resolved.requirements, keyword.requirements);
    }

    #[test]
    fn preferred_tool_level_flows_into_resolution() {
        let draft = PromptDraft {
            text: "Verify the failing test suite".to_owned(),
            ..PromptDraft::default()
        };
        // Verify-intent drafts prefer the focused-verification tool, whose
        // explicit metadata level is Focused.
        assert_eq!(
            preferred_verification_level(&draft),
            Some(VerificationLevel::Focused)
        );
    }

    #[test]
    fn inspection_prompts_prefer_narrow_inspection() {
        let draft = PromptDraft {
            text: "Explain how this module works".to_owned(),
            ..PromptDraft::default()
        };
        let context = runtime_context(&draft).expect("tool policy context");
        assert!(context.contains("intent=inspect"));
        assert!(context.contains("focused-inspection"));
    }
}
