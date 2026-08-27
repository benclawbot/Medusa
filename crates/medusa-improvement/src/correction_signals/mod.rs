use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConversationRole {
    User,
    Assistant,
    Tool,
    System,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ConversationTurn {
    pub id: String,
    pub role: ConversationRole,
    pub content: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LearningSignalKind {
    ExplicitCorrection,
    Dissatisfaction,
    RepeatedInstruction,
    Preference,
    Omission,
    UnjustifiedClaim,
    WorkflowFailure,
    ReusableSuccess,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CandidateScope {
    Task,
    Repository,
    User,
    Unresolved,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RedactionStatus {
    NotRequired,
    Redacted,
    Blocked,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct EvidenceReference {
    pub turn_id: String,
    pub excerpt_digest: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LearningSignal {
    pub id: String,
    pub kind: LearningSignalKind,
    pub source_turns: Vec<String>,
    pub task_id: Option<String>,
    pub observed_behavior: String,
    pub user_correction: Option<String>,
    pub requested_outcome: Option<String>,
    pub candidate_scope: CandidateScope,
    pub confidence_milli: u16,
    pub ambiguity: Vec<String>,
    pub evidence: Vec<EvidenceReference>,
    pub redaction_status: RedactionStatus,
    pub contradicted_by: Vec<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct LearningSignalBatch {
    pub signals: Vec<LearningSignal>,
    pub blocked_turns: Vec<String>,
}

#[derive(Clone, Debug, Default)]
pub struct CorrectionSignalDetector;

impl CorrectionSignalDetector {
    #[must_use]
    pub fn detect(&self, turns: &[ConversationTurn], task_id: Option<&str>) -> LearningSignalBatch {
        let mut grouped = BTreeMap::<String, LearningSignal>::new();
        let mut blocked_turns = Vec::new();
        let mut previous_assistant = None::<&ConversationTurn>;

        for turn in turns {
            match turn.role {
                ConversationRole::Assistant => previous_assistant = Some(turn),
                ConversationRole::User => {
                    let redaction = redact(&turn.content);
                    if redaction.status == RedactionStatus::Blocked {
                        blocked_turns.push(turn.id.clone());
                        continue;
                    }
                    let Some(classification) = classify(&redaction.text) else {
                        continue;
                    };
                    let normalized = normalize(&redaction.text);
                    let key = semantic_key(classification.kind, &normalized);
                    let observed_behavior = previous_assistant
                        .map(|assistant| summarize_observed(&assistant.content))
                        .unwrap_or_else(|| {
                            "user identified a correction without an assistant turn".to_owned()
                        });
                    let evidence = EvidenceReference {
                        turn_id: turn.id.clone(),
                        excerpt_digest: digest(&normalized),
                    };
                    let entry = grouped
                        .entry(key.clone())
                        .or_insert_with(|| LearningSignal {
                            id: format!("signal-{}", &digest(&key)[..16]),
                            kind: classification.kind,
                            source_turns: Vec::new(),
                            task_id: task_id.map(ToOwned::to_owned),
                            observed_behavior,
                            user_correction: Some(redaction.text.clone()),
                            requested_outcome: classification.requested_outcome.clone(),
                            candidate_scope: classification.scope,
                            confidence_milli: classification.confidence_milli,
                            ambiguity: classification.ambiguity.clone(),
                            evidence: Vec::new(),
                            redaction_status: redaction.status,
                            contradicted_by: Vec::new(),
                        });
                    push_unique(&mut entry.source_turns, turn.id.clone());
                    if !entry.evidence.iter().any(|item| item.turn_id == turn.id) {
                        entry.evidence.push(evidence);
                    }
                    entry.confidence_milli = entry.confidence_milli.saturating_add(50).min(1_000);
                    if redaction.status == RedactionStatus::Redacted {
                        entry.redaction_status = RedactionStatus::Redacted;
                    }
                }
                ConversationRole::Tool | ConversationRole::System => {}
            }
        }

        let mut signals = grouped.into_values().collect::<Vec<_>>();
        mark_contradictions(&mut signals);
        signals.sort_by(|left, right| left.id.cmp(&right.id));
        LearningSignalBatch {
            signals,
            blocked_turns,
        }
    }
}

struct Classification {
    kind: LearningSignalKind,
    scope: CandidateScope,
    confidence_milli: u16,
    requested_outcome: Option<String>,
    ambiguity: Vec<String>,
}

fn classify(text: &str) -> Option<Classification> {
    let lower = text.to_ascii_lowercase();
    let explicit = contains_any(
        &lower,
        &[
            "that's wrong",
            "that is wrong",
            "you missed",
            "you forgot",
            "not what i asked",
            "the question was different",
            "no because",
        ],
    );
    let omission = contains_any(
        &lower,
        &[
            "you missed",
            "you forgot",
            "didn't include",
            "not complete",
            "incomplete",
        ],
    );
    let unjustified = contains_any(
        &lower,
        &[
            "don't claim",
            "do not claim",
            "are you sure",
            "without checking",
            "without verifying",
        ],
    );
    let workflow = contains_any(
        &lower,
        &[
            "why did you",
            "should have",
            "start by",
            "before you",
            "don't talk about follow-ups",
        ],
    );
    let preference = contains_any(
        &lower,
        &["always ", "never ", "i prefer", "for me,", "remember that"],
    );
    let dissatisfaction = contains_any(
        &lower,
        &[
            "not good enough",
            "this is generic",
            "that's generic",
            "not just that",
        ],
    );
    let reusable_success = contains_any(
        &lower,
        &[
            "do this in the future",
            "use this approach again",
            "this worked well",
        ],
    );

    let kind = if omission {
        LearningSignalKind::Omission
    } else if unjustified {
        LearningSignalKind::UnjustifiedClaim
    } else if workflow {
        LearningSignalKind::WorkflowFailure
    } else if preference {
        LearningSignalKind::Preference
    } else if reusable_success {
        LearningSignalKind::ReusableSuccess
    } else if explicit {
        LearningSignalKind::ExplicitCorrection
    } else if dissatisfaction {
        LearningSignalKind::Dissatisfaction
    } else {
        return None;
    };

    let scope = if preference
        || lower.contains("in the future")
        || lower.contains("always")
        || lower.contains("never")
    {
        CandidateScope::User
    } else if lower.contains("this repo") || lower.contains("medusa") {
        CandidateScope::Repository
    } else if explicit || dissatisfaction {
        // A concrete omission is interpreted by the lesson engine into an explicit
        // completeness procedure. Keep generic dissatisfaction unresolved, but do not
        // discard a correction whose failure class is already typed as an omission.
        if omission {
            CandidateScope::Task
        } else {
            CandidateScope::Unresolved
        }
    } else {
        CandidateScope::Task
    };
    let confidence_milli = match kind {
        LearningSignalKind::ExplicitCorrection | LearningSignalKind::Omission => 850,
        LearningSignalKind::UnjustifiedClaim | LearningSignalKind::WorkflowFailure => 800,
        LearningSignalKind::Preference => 750,
        LearningSignalKind::Dissatisfaction => 600,
        LearningSignalKind::RepeatedInstruction => 700,
        LearningSignalKind::ReusableSuccess => 700,
    };
    let requested_outcome = requested_clause(text);
    let ambiguity = if scope == CandidateScope::Unresolved {
        vec!["durable scope cannot be inferred safely from this turn alone".to_owned()]
    } else {
        Vec::new()
    };

    Some(Classification {
        kind,
        scope,
        confidence_milli,
        requested_outcome,
        ambiguity,
    })
}

fn mark_contradictions(signals: &mut [LearningSignal]) {
    for index in 0..signals.len() {
        let Some(left) = signals[index].user_correction.clone() else {
            continue;
        };
        for other in (index + 1)..signals.len() {
            let Some(right) = signals[other].user_correction.clone() else {
                continue;
            };
            if contradictory(&left, &right) {
                let left_id = signals[index].id.clone();
                let right_id = signals[other].id.clone();
                push_unique(&mut signals[index].contradicted_by, right_id);
                push_unique(&mut signals[other].contradicted_by, left_id);
                push_unique(
                    &mut signals[index].ambiguity,
                    "conflicting user feedback requires resolution".to_owned(),
                );
                push_unique(
                    &mut signals[other].ambiguity,
                    "conflicting user feedback requires resolution".to_owned(),
                );
                signals[index].candidate_scope = CandidateScope::Unresolved;
                signals[other].candidate_scope = CandidateScope::Unresolved;
            }
        }
    }
}

fn contradictory(left: &str, right: &str) -> bool {
    let left = normalize(left);
    let right = normalize(right);
    let pairs = [
        ("always ask", "never ask"),
        ("always browse", "never browse"),
        ("always confirm", "never confirm"),
        ("be concise", "be detailed"),
    ];
    pairs.iter().any(|(positive, negative)| {
        (left.contains(positive) && right.contains(negative))
            || (left.contains(negative) && right.contains(positive))
    })
}

struct Redaction {
    text: String,
    status: RedactionStatus,
}

fn redact(text: &str) -> Redaction {
    let lower = text.to_ascii_lowercase();
    if contains_any(&lower, &["private key", "recovery phrase", "seed phrase"]) {
        return Redaction {
            text: String::new(),
            status: RedactionStatus::Blocked,
        };
    }
    let mut output = Vec::new();
    let mut redacted = false;
    for token in text.split_whitespace() {
        let looks_secret = token.starts_with("sk-")
            || token.starts_with("ghp_")
            || token.starts_with("github_pat_")
            || token.contains("Bearer=")
            || token.contains("password=")
            || token.contains("token=");
        if looks_secret {
            output.push("[REDACTED]");
            redacted = true;
        } else {
            output.push(token);
        }
    }
    Redaction {
        text: output.join(" "),
        status: if redacted {
            RedactionStatus::Redacted
        } else {
            RedactionStatus::NotRequired
        },
    }
}

fn requested_clause(text: &str) -> Option<String> {
    let lower = text.to_ascii_lowercase();
    for marker in ["instead", "should", "must", "always", "never", "before"] {
        if let Some(index) = lower.find(marker) {
            let clause = text[index..].trim();
            if !clause.is_empty() {
                return Some(clause.chars().take(240).collect());
            }
        }
    }
    None
}

fn summarize_observed(text: &str) -> String {
    let normalized = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.is_empty() {
        "assistant produced an empty response".to_owned()
    } else {
        format!(
            "assistant response: {}",
            normalized.chars().take(180).collect::<String>()
        )
    }
}

fn normalize(text: &str) -> String {
    text.to_ascii_lowercase()
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn semantic_key(kind: LearningSignalKind, normalized: &str) -> String {
    let mut terms = normalized
        .split_whitespace()
        .filter(|term| !is_stop_word(term))
        .take(12)
        .collect::<Vec<_>>();
    terms.sort_unstable();
    terms.dedup();
    format!("{kind:?}:{}", terms.join("-"))
}

fn digest(value: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(value.as_bytes());
    format!("{:x}", hasher.finalize())
}

fn contains_any(value: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| value.contains(needle))
}

fn push_unique(values: &mut Vec<String>, value: String) {
    if !values.contains(&value) {
        values.push(value);
    }
}

fn is_stop_word(term: &str) -> bool {
    matches!(
        term,
        "a" | "an"
            | "and"
            | "be"
            | "because"
            | "did"
            | "do"
            | "i"
            | "is"
            | "it"
            | "of"
            | "that"
            | "the"
            | "this"
            | "to"
            | "you"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn turn(id: &str, role: ConversationRole, content: &str) -> ConversationTurn {
        ConversationTurn {
            id: id.to_owned(),
            role,
            content: content.to_owned(),
        }
    }

    #[test]
    fn detects_explicit_omission_with_turn_provenance() {
        let batch = CorrectionSignalDetector.detect(
            &[
                turn(
                    "a1",
                    ConversationRole::Assistant,
                    "The plan covers desktop and TUI.",
                ),
                turn(
                    "u1",
                    ConversationRole::User,
                    "You missed orchestration and sub-agents.",
                ),
            ],
            Some("task-1"),
        );
        assert_eq!(batch.signals.len(), 1);
        let signal = &batch.signals[0];
        assert_eq!(signal.kind, LearningSignalKind::Omission);
        assert_eq!(signal.source_turns, vec!["u1"]);
        assert_eq!(signal.task_id.as_deref(), Some("task-1"));
        assert!(signal.observed_behavior.contains("plan covers"));
        assert_eq!(signal.redaction_status, RedactionStatus::NotRequired);
    }

    #[test]
    fn consolidates_repeated_equivalent_corrections() {
        let batch = CorrectionSignalDetector.detect(
            &[
                turn(
                    "u1",
                    ConversationRole::User,
                    "You missed commit history coverage.",
                ),
                turn(
                    "u2",
                    ConversationRole::User,
                    "You missed coverage of commit history.",
                ),
            ],
            None,
        );
        assert_eq!(batch.signals.len(), 1);
        assert_eq!(batch.signals[0].source_turns.len(), 2);
        assert!(batch.signals[0].confidence_milli > 850);
    }

    #[test]
    fn keeps_ambiguous_dissatisfaction_unresolved() {
        let batch = CorrectionSignalDetector.detect(
            &[turn(
                "u1",
                ConversationRole::User,
                "That's generic and not good enough.",
            )],
            None,
        );
        assert_eq!(batch.signals[0].kind, LearningSignalKind::Dissatisfaction);
        assert_eq!(batch.signals[0].candidate_scope, CandidateScope::Unresolved);
        assert!(!batch.signals[0].ambiguity.is_empty());
    }

    #[test]
    fn marks_contradictory_preferences_for_resolution() {
        let batch = CorrectionSignalDetector.detect(
            &[
                turn(
                    "u1",
                    ConversationRole::User,
                    "Always ask before making changes.",
                ),
                turn(
                    "u2",
                    ConversationRole::User,
                    "Never ask before making changes.",
                ),
            ],
            None,
        );
        assert_eq!(batch.signals.len(), 2);
        assert!(
            batch
                .signals
                .iter()
                .all(|signal| !signal.contradicted_by.is_empty())
        );
        assert!(
            batch
                .signals
                .iter()
                .all(|signal| signal.candidate_scope == CandidateScope::Unresolved)
        );
    }

    #[test]
    fn redacts_tokens_and_blocks_high_risk_secret_material() {
        let redacted = CorrectionSignalDetector.detect(
            &[turn(
                "u1",
                ConversationRole::User,
                "Never log token=abc123 when reporting errors.",
            )],
            None,
        );
        assert_eq!(
            redacted.signals[0].redaction_status,
            RedactionStatus::Redacted
        );
        assert!(
            !redacted.signals[0]
                .user_correction
                .as_deref()
                .unwrap_or_default()
                .contains("abc123")
        );

        let blocked = CorrectionSignalDetector.detect(
            &[turn(
                "u2",
                ConversationRole::User,
                "Remember my seed phrase is alpha beta gamma.",
            )],
            None,
        );
        assert!(blocked.signals.is_empty());
        assert_eq!(blocked.blocked_turns, vec!["u2"]);
    }

    #[test]
    fn ignores_ordinary_task_instruction() {
        let batch = CorrectionSignalDetector.detect(
            &[turn(
                "u1",
                ConversationRole::User,
                "Add a parser module and tests.",
            )],
            None,
        );
        assert!(batch.signals.is_empty());
    }
}
