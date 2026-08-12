#![forbid(unsafe_code)]

use serde::Serialize;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TextKind {
    Procedural,
    Descriptive,
}

impl TextKind {
    pub const fn default_word_limit(self) -> usize {
        match self {
            Self::Procedural => 20,
            Self::Descriptive => 25,
        }
    }
}

#[derive(Clone, Debug)]
pub struct ClarityConfig {
    pub kind: TextKind,
    pub max_words: Option<usize>,
}

impl Default for ClarityConfig {
    fn default() -> Self {
        Self {
            kind: TextKind::Descriptive,
            max_words: None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Warning,
    Error,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub enum RuleId {
    #[serde(rename = "MEDUSA-CLARITY-001")]
    LongSentence,
    #[serde(rename = "MEDUSA-CLARITY-002")]
    AmbiguousStatus,
    #[serde(rename = "MEDUSA-CLARITY-003")]
    InstructionInNote,
    #[serde(rename = "MEDUSA-CLARITY-004")]
    PassiveVoice,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Finding {
    pub rule: RuleId,
    pub severity: Severity,
    pub line: usize,
    pub message: String,
    pub excerpt: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Report {
    pub word_limit: usize,
    pub findings: Vec<Finding>,
}

impl Report {
    pub fn passed(&self) -> bool {
        self.findings.is_empty()
    }
}

#[must_use]
pub fn runtime_prompt_fragment() -> &'static str {
    "CLEAROPS COMMUNICATION POLICY — ACTIVE\n\n\
Use short, direct sentences. Prefer active voice and name the actor when it is known.\n\
For procedures, write explicit commands and keep one action in each step. Use a vertical list when a sentence contains multiple actions.\n\
Do not hide instructions, requirements, or limits in notes. Notes provide information only.\n\
Do not use vague status claims such as 'should work', 'probably fixed', 'looks okay', or 'almost done'. State the observable status and the evidence.\n\
Do not claim completion until the requested implementation is connected to a production execution path and integration tests prove that path. A standalone crate, helper, or command is not complete when the normal Medusa runtime does not use it.\n\
Final reports must identify the action, result, remaining blocker, and verification evidence."
}

pub fn analyze(text: &str, config: &ClarityConfig) -> Report {
    let limit = config.max_words.unwrap_or(config.kind.default_word_limit());
    let mut findings = Vec::new();

    for (index, line) in text.lines().enumerate() {
        let line_number = index + 1;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        for sentence in split_sentences(trimmed) {
            let words = word_count(sentence);
            if words > limit {
                findings.push(Finding {
                    rule: RuleId::LongSentence,
                    severity: Severity::Warning,
                    line: line_number,
                    message: format!("sentence has {words} words; the configured limit is {limit}"),
                    excerpt: sentence.to_owned(),
                });
            }
        }

        let lower = trimmed.to_ascii_lowercase();
        if contains_ambiguous_status(&lower) {
            findings.push(Finding {
                rule: RuleId::AmbiguousStatus,
                severity: Severity::Warning,
                line: line_number,
                message: "replace vague completion language with an observable status and evidence"
                    .to_owned(),
                excerpt: trimmed.to_owned(),
            });
        }

        if lower.starts_with("note:") && note_contains_instruction(&lower) {
            findings.push(Finding {
                rule: RuleId::InstructionInNote,
                severity: Severity::Error,
                line: line_number,
                message: "move the instruction out of the note and into a procedure step"
                    .to_owned(),
                excerpt: trimmed.to_owned(),
            });
        }

        if likely_passive_voice(&lower) {
            findings.push(Finding {
                rule: RuleId::PassiveVoice,
                severity: Severity::Warning,
                line: line_number,
                message: "identify the actor and use active voice when the actor is known"
                    .to_owned(),
                excerpt: trimmed.to_owned(),
            });
        }
    }

    Report {
        word_limit: limit,
        findings,
    }
}

fn split_sentences(line: &str) -> impl Iterator<Item = &str> {
    line.split(['.', '!', '?'])
        .map(str::trim)
        .filter(|item| !item.is_empty())
}

fn word_count(sentence: &str) -> usize {
    sentence
        .split_whitespace()
        .filter(|word| word.chars().any(char::is_alphanumeric))
        .count()
}

fn contains_ambiguous_status(line: &str) -> bool {
    [
        "probably fixed",
        "should be good",
        "should work",
        "seems good",
        "looks okay",
        "mostly pass",
        "almost done",
        "should pass",
    ]
    .iter()
    .any(|phrase| line.contains(phrase))
}

fn note_contains_instruction(line: &str) -> bool {
    let body = line.trim_start_matches("note:").trim_start();
    [
        "add ",
        "check ",
        "continue ",
        "do ",
        "install ",
        "make sure ",
        "remove ",
        "run ",
        "set ",
        "update ",
        "use ",
    ]
    .iter()
    .any(|verb| body.starts_with(verb))
}

fn likely_passive_voice(line: &str) -> bool {
    const AUXILIARIES: [&str; 8] = [
        " is ",
        " are ",
        " was ",
        " were ",
        " be ",
        " been ",
        " being ",
        " will be ",
    ];
    let padded = format!(" {line} ");
    AUXILIARIES.iter().any(|auxiliary| {
        padded.find(auxiliary).is_some_and(|position| {
            padded[position + auxiliary.len()..]
                .split_whitespace()
                .next()
                .is_some_and(|word| {
                    let word = word.trim_matches(|ch: char| !ch.is_alphabetic());
                    word.ends_with("ed")
                        || ["given", "held", "known", "made", "sent", "shown", "written"]
                            .contains(&word)
                })
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_policy_requires_real_integration() {
        let fragment = runtime_prompt_fragment();
        assert!(fragment.contains("production execution path"));
        assert!(fragment.contains("integration tests"));
        assert!(fragment.contains("standalone crate"));
    }

    #[test]
    fn accepts_clear_procedure() {
        let report = analyze(
            "ACTION: Run the test suite.\nRESULT: All required checks passed.",
            &ClarityConfig {
                kind: TextKind::Procedural,
                max_words: None,
            },
        );
        assert!(report.passed(), "{:#?}", report.findings);
    }

    #[test]
    fn detects_long_sentence() {
        let report = analyze(
            "This sentence contains many words and continues with extra information that should be split into smaller units for a reader.",
            &ClarityConfig {
                kind: TextKind::Descriptive,
                max_words: Some(10),
            },
        );
        assert_eq!(report.findings[0].rule, RuleId::LongSentence);
    }

    #[test]
    fn detects_instruction_in_note() {
        let report = analyze(
            "NOTE: Run the migration before deployment.",
            &ClarityConfig::default(),
        );
        assert!(
            report
                .findings
                .iter()
                .any(|finding| finding.rule == RuleId::InstructionInNote)
        );
    }

    #[test]
    fn detects_vague_status() {
        let report = analyze("The fix should be good now.", &ClarityConfig::default());
        assert!(
            report
                .findings
                .iter()
                .any(|finding| finding.rule == RuleId::AmbiguousStatus)
        );
    }

    #[test]
    fn detects_passive_voice() {
        let report = analyze(
            "The configuration was changed by Medusa.",
            &ClarityConfig::default(),
        );
        assert!(
            report
                .findings
                .iter()
                .any(|finding| finding.rule == RuleId::PassiveVoice)
        );
    }
}
