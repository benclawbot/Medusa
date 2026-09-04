use sha2::{Digest, Sha256};

#[path = "inner.rs"]
mod inner;

pub use inner::*;
pub mod mutation_dag;
pub mod speculation;

// These clauses can protect paths adjacent to an affirmative coding request. Normalize them
// before the core planner classifies intent so "create X; do not modify any other file" does not
// accidentally turn the whole request into a read-only plan.
const SCOPED_READ_ONLY_PHRASES: &[&str] = &[
    "without changing",
    "without modifying",
    "do not change",
    "do not modify",
    "don't change",
    "don't modify",
];
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
const TERMINAL_PROSE_PATH_DELIMITERS: &[char] = &['.', ',', ';', ':', ')', ']', '}', '!', '?'];

/// Plans through the public scheduler boundary while preserving scoped protection clauses.
///
/// The core planner intentionally fails closed when it sees read-only language. A coding objective
/// can also use that language to protect specific files, for example "repair X without modifying
/// verify.py". In that mixed case, neutralize only the scoped protection phrase for intent
/// classification. Repository-aware path cleanup also removes terminal prose punctuation only when
/// the exact token does not resolve and a stripped candidate does. The caller's accepted objective
/// is restored before returning.
pub fn plan_typed(mut input: PlannerInput) -> Result<PlanningResult, &'static str> {
    let original_objective = input.objective.trim().to_owned();
    input.objective = original_objective.clone();

    if has_affirmative_mutation_request(&input.objective) {
        input.objective = neutralize_scoped_read_only_phrases(&input.objective);
    }
    input.objective = resolve_prose_delimited_paths(&input.objective, &input.repository_paths);

    let mut result = inner::plan_typed(input)?;
    if result.requested_outcomes == [original_objective.clone()] {
        return Ok(result);
    }

    if result.requested_outcomes.len() != 1 {
        return Err("typed planning result changed the accepted objective shape");
    }
    result.requested_outcomes[0] = original_objective;
    result.fingerprint = planning_fingerprint(&result)?;
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

fn resolve_prose_delimited_paths(value: &str, repository_paths: &[String]) -> String {
    value
        .split_whitespace()
        .map(|token| resolve_prose_delimited_path_token(token, repository_paths))
        .collect::<Vec<_>>()
        .join(" ")
}

fn resolve_prose_delimited_path_token(token: &str, repository_paths: &[String]) -> String {
    let candidate = token.trim_matches(|character: char| {
        !character.is_ascii_alphanumeric() && !matches!(character, '/' | '\\' | '.' | '-' | '_')
    });
    if candidate.is_empty()
        || (!candidate.contains('/') && !candidate.contains('\\') && !candidate.contains('.'))
        || repository_path_exists(candidate, repository_paths)
    {
        return token.to_owned();
    }

    let mut stripped = candidate;
    while let Some(last) = stripped.chars().last() {
        if !TERMINAL_PROSE_PATH_DELIMITERS.contains(&last) {
            break;
        }
        stripped = &stripped[..stripped.len() - last.len_utf8()];
        if stripped.is_empty() {
            break;
        }
        if repository_path_exists(stripped, repository_paths) {
            return token.replacen(candidate, stripped, 1);
        }
    }

    token.to_owned()
}

fn repository_path_exists(candidate: &str, repository_paths: &[String]) -> bool {
    let candidate = candidate.replace('\\', "/");
    repository_paths.iter().any(|path| {
        let path = path.replace('\\', "/");
        path == candidate
            || path
                .strip_prefix(&candidate)
                .is_some_and(|remainder| remainder.starts_with('/'))
    })
}

fn planning_fingerprint(result: &PlanningResult) -> Result<String, &'static str> {
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
    .map_err(|_| "failed to serialize planning result")?;
    Ok(hex::encode(Sha256::digest(bytes)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scoped_do_not_modify_clause_preserves_mutation_intent() {
        let planned = plan_typed(PlannerInput {
            objective:
                "Create a simple webpage in index.html with fs_write; do not modify any other file"
                    .to_owned(),
            attachment_count: 0,
            repository_paths: Vec::new(),
        })
        .expect("scoped protection clause should still permit the requested artifact");

        assert_eq!(planned.strategy, ExecutionStrategy::CoordinatedMutation);
        assert_eq!(planned.scope.effective, vec!["index.html".to_owned()]);
    }

    #[test]
    fn unqualified_do_not_modify_request_remains_read_only() {
        let planned = plan_typed(PlannerInput {
            objective: "Inspect the repository and do not modify any files".to_owned(),
            attachment_count: 0,
            repository_paths: vec!["src/lib.rs".to_owned()],
        })
        .expect("read-only request should still be plannable");

        assert_eq!(planned.strategy, ExecutionStrategy::CoordinatedReadOnly);
        assert_eq!(planned.scope.resolution, ScopeResolution::NotRequested);
    }
}
