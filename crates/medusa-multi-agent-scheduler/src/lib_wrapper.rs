use sha2::{Digest, Sha256};

#[path = "lib.rs"]
mod core;

pub use core::*;
pub mod mutation_dag;
pub mod speculation;

const SCOPED_READ_ONLY_PHRASES: &[&str] = &["without changing", "without modifying"];
const ADVISORY_MUTATION_PHRASES: &[&str] = &[
    "analyze how to",
    "describe how to",
    "explain how to",
    "inspect how to",
    "review how to",
    "show how to",
    "tell me how to",
];
const MUTATION_VERBS: &[&str] = &[
    "add",
    "build",
    "change",
    "correct",
    "create",
    "delete",
    "edit",
    "fix",
    "implement",
    "make",
    "migrate",
    "modify",
    "patch",
    "refactor",
    "remove",
    "rename",
    "repair",
    "replace",
    "rewrite",
    "update",
    "upgrade",
    "write",
];

/// Plans through the public scheduler boundary while preserving scoped protection clauses.
///
/// The core planner intentionally fails closed when it sees read-only language. A coding objective
/// can also use that language to protect specific files, for example "repair X without modifying
/// verify.py". In that mixed case, neutralize only the scoped protection phrase for intent
/// classification. The protected path remains present in the objective and therefore in the
/// repository evidence; the caller's accepted objective is restored before returning.
pub fn plan_typed(mut input: PlannerInput) -> Result<PlanningResult, &'static str> {
    let original_objective = input.objective.trim().to_owned();
    input.objective = original_objective.clone();

    if has_affirmative_mutation_request(&input.objective) {
        input.objective = neutralize_scoped_read_only_phrases(&input.objective);
    }

    let mut result = core::plan_typed(input)?;
    if result.requested_outcomes == [original_objective.clone()] {
        return Ok(result);
    }

    if result.requested_outcomes.len() != 1 {
        return Err("typed planning result changed the accepted objective shape");
    }
    result.requested_outcomes[0] = original_objective;
    result.fingerprint = planning_fingerprint(&result);
    result.validate()?;
    Ok(result)
}

fn has_affirmative_mutation_request(objective: &str) -> bool {
    let lower = objective.to_ascii_lowercase();
    if !SCOPED_READ_ONLY_PHRASES
        .iter()
        .any(|phrase| lower.contains(phrase))
        || ADVISORY_MUTATION_PHRASES
            .iter()
            .any(|phrase| lower.contains(phrase))
    {
        return false;
    }

    let neutralized = neutralize_scoped_read_only_phrases(&lower);
    neutralized
        .split(|character: char| !character.is_ascii_alphanumeric() && character != '-')
        .filter(|word| !word.is_empty())
        .any(|word| MUTATION_VERBS.contains(&word))
}

fn neutralize_scoped_read_only_phrases(value: &str) -> String {
    let mut normalized = value.to_owned();
    for phrase in SCOPED_READ_ONLY_PHRASES {
        loop {
            let lower = normalized.to_ascii_lowercase();
            let Some(start) = lower.find(phrase) else {
                break;
            };
            let end = start + phrase.len();
            normalized.replace_range(start..end, "while preserving");
        }
    }
    normalized
}

fn planning_fingerprint(result: &PlanningResult) -> String {
    let bytes = serde_json::to_vec(&(
        result.intent,
        &result.requested_outcomes,
        &result.affected_components,
        &result.scope,
        result.risk,
        result.confidence_milli,
        &result.required_capabilities,
        result.strategy,
        result.lane,
        &result.lane_rationale,
        result.model_turn_budget,
        &result.tasks,
    ))
    .unwrap_or_default();
    hex::encode(Sha256::digest(bytes))
}
