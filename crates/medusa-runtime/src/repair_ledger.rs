use std::collections::{BTreeMap, BTreeSet};

use medusa_agent::AgentSession;
use medusa_protocol::EventPayload;
use medusa_session_continuity::{
    RepairAttemptCheckpoint, RepairLedgerEntry, RepairLedgerTransition, VerificationOutcome,
};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone)]
pub(crate) struct Projection {
    pub entries: Vec<RepairLedgerEntry>,
    pub generation: u64,
    pub cursor: u64,
}

pub(crate) fn project(
    session: &AgentSession,
    repository_fingerprint: &str,
    existing: &[RepairLedgerEntry],
    generation: u64,
    cursor: u64,
) -> Projection {
    let mut entries = existing.to_vec();
    let mut generation = generation;
    let mut cursor = cursor;
    let mut changed_files = BTreeSet::new();

    let starting_cursor = cursor;
    for event in session
        .events
        .iter()
        .filter(|event| event.sequence > starting_cursor)
    {
        cursor = cursor.max(event.sequence);
        match &event.payload {
            EventPayload::FileTransactionCommitted { paths, .. } => {
                if let Ok(value) = serde_json::to_value(paths) {
                    collect_paths(&value, &mut changed_files);
                }
            }
            EventPayload::VerificationCompleted { passed, evidence } => {
                generation = generation.saturating_add(1);
                let source_ref = format!(
                    ".medusa/sessions/{}/journal.jsonl#{}",
                    session.id, event.sequence
                );
                let command = infer_command(evidence, event.sequence);
                let parsed = if *passed {
                    Vec::new()
                } else {
                    parse_diagnostics(evidence, &command, &source_ref, generation)
                };
                reconcile_generation(
                    &mut entries,
                    parsed,
                    generation,
                    &changed_files,
                    repository_fingerprint,
                    &command,
                    *passed,
                );
                changed_files.clear();
            }
            EventPayload::RuntimeFailed { message } => {
                generation = generation.max(1);
                add_non_verification_failure(
                    &mut entries,
                    "runtime",
                    message,
                    generation,
                    format!(
                        ".medusa/sessions/{}/journal.jsonl#{}",
                        session.id, event.sequence
                    ),
                );
            }
            EventPayload::SessionFailed { error } => {
                generation = generation.max(1);
                add_non_verification_failure(
                    &mut entries,
                    "session",
                    &error.to_string(),
                    generation,
                    format!(
                        ".medusa/sessions/{}/journal.jsonl#{}",
                        session.id, event.sequence
                    ),
                );
            }
            _ => {}
        }
    }

    entries.sort_by(|left, right| {
        right
            .unresolved()
            .cmp(&left.unresolved())
            .then_with(|| right.last_generation.cmp(&left.last_generation))
            .then_with(|| left.fingerprint.cmp(&right.fingerprint))
    });
    entries.truncate(128);

    Projection {
        entries,
        generation,
        cursor,
    }
}

fn reconcile_generation(
    entries: &mut Vec<RepairLedgerEntry>,
    mut observed: Vec<RepairLedgerEntry>,
    generation: u64,
    changed_files: &BTreeSet<String>,
    repository_fingerprint: &str,
    command: &str,
    passed: bool,
) {
    cluster_common_roots(&mut observed);
    let observed_fingerprints = observed
        .iter()
        .map(|entry| entry.fingerprint.clone())
        .collect::<BTreeSet<_>>();

    for existing in entries.iter_mut().filter(|entry| entry.unresolved()) {
        if !changed_files.is_empty() {
            let files = changed_files.iter().cloned().collect::<Vec<_>>();
            let attempt_id = format!("repair:{}:{}", generation, existing.fingerprint);
            if !existing.repairs.iter().any(|attempt| attempt.id == attempt_id) {
                existing.repairs.push(RepairAttemptCheckpoint {
                    id: attempt_id,
                    failure_fingerprint: existing.fingerprint.clone(),
                    changed_files: files,
                    outcome: if passed {
                        VerificationOutcome::Passed
                    } else {
                        VerificationOutcome::Failed
                    },
                    hypothesis: format!("mutation before verification generation {generation}"),
                    repository_fingerprint: repository_fingerprint.to_owned(),
                });
            }
        }

        if passed && existing.command == command {
            existing.transition = RepairLedgerTransition::Resolved;
            existing.last_generation = generation;
        } else if observed_fingerprints.contains(&existing.fingerprint) {
            existing.transition = RepairLedgerTransition::Persisted;
            existing.last_generation = generation;
        } else if same_identity_exists(existing, &observed) {
            existing.transition = RepairLedgerTransition::Transformed;
            existing.last_generation = generation;
        } else if !changed_files.is_empty() {
            existing.transition = RepairLedgerTransition::Resolved;
            existing.last_generation = generation;
        }
    }

    for mut item in observed {
        if let Some(existing) = entries
            .iter_mut()
            .find(|entry| entry.fingerprint == item.fingerprint)
        {
            let is_new_occurrence = item
                .source_refs
                .first()
                .is_some_and(|source| !existing.source_refs.contains(source));
            if is_new_occurrence {
                existing.occurrence_count = existing.occurrence_count.saturating_add(1);
                existing.source_refs.append(&mut item.source_refs);
            }
            if !existing.changed_details.contains(&item.summary) && existing.summary != item.summary {
                existing.changed_details.push(item.summary.clone());
                existing.changed_details.truncate(16);
            }
            existing.summary = item.summary;
            existing.last_generation = generation;
            existing.transition = RepairLedgerTransition::Persisted;
            existing.root_fingerprint = item.root_fingerprint;
            existing.cascade = item.cascade;
        } else {
            entries.push(item);
        }
    }
}

fn same_identity_exists(existing: &RepairLedgerEntry, observed: &[RepairLedgerEntry]) -> bool {
    observed.iter().any(|candidate| {
        candidate.command == existing.command
            && candidate.file == existing.file
            && candidate.test == existing.test
            && candidate.symbol == existing.symbol
            && candidate.diagnostic_class == existing.diagnostic_class
    })
}

fn cluster_common_roots(entries: &mut [RepairLedgerEntry]) {
    let mut roots = BTreeMap::<(String, String), String>::new();
    for entry in entries.iter_mut() {
        let Some(file) = entry.file.clone() else {
            continue;
        };
        let key = (file, entry.diagnostic_class.clone());
        if let Some(root) = roots.get(&key) {
            entry.root_fingerprint = Some(root.clone());
            entry.cascade = true;
        } else {
            roots.insert(key, entry.fingerprint.clone());
        }
    }
}

fn parse_diagnostics(
    evidence: &[String],
    command: &str,
    source_ref: &str,
    generation: u64,
) -> Vec<RepairLedgerEntry> {
    let chunks = diagnostic_chunks(evidence);
    let mut result = Vec::new();
    let mut seen = BTreeSet::new();
    for chunk in chunks {
        if chunk.trim().is_empty() || looks_like_summary_only(&chunk) {
            continue;
        }
        let diagnostic_class = classify(&chunk);
        let file = extract_file(&chunk);
        let test = extract_test(&chunk);
        let symbol = extract_symbol(&chunk);
        let scope = file
            .as_deref()
            .and_then(|path| path.split('/').next())
            .unwrap_or("workspace")
            .to_owned();
        let summary = compact_summary(&chunk);
        let normalized = normalize_for_fingerprint(&summary);
        let fingerprint = digest(
            format!(
                "{}|{}|{}|{}|{}|{}|{}",
                diagnostic_class,
                command,
                scope,
                file.as_deref().unwrap_or(""),
                symbol.as_deref().unwrap_or(""),
                test.as_deref().unwrap_or(""),
                normalized
            )
            .as_bytes(),
        );
        if !seen.insert(fingerprint.clone()) {
            continue;
        }
        result.push(RepairLedgerEntry {
            fingerprint,
            source: "verification".to_owned(),
            command: command.to_owned(),
            scope,
            file,
            symbol,
            test,
            diagnostic_class,
            summary,
            first_generation: generation,
            last_generation: generation,
            occurrence_count: 1,
            changed_details: Vec::new(),
            source_refs: vec![source_ref.to_owned()],
            root_fingerprint: None,
            cascade: false,
            transition: RepairLedgerTransition::New,
            repairs: Vec::new(),
        });
    }
    if result.is_empty() {
        let summary = evidence
            .iter()
            .find(|line| !line.trim().is_empty())
            .map(|line| compact_summary(line))
            .unwrap_or_else(|| "authoritative verification failed".to_owned());
        let fingerprint = digest(format!("verification|{command}|{summary}").as_bytes());
        result.push(RepairLedgerEntry {
            fingerprint,
            source: "verification".to_owned(),
            command: command.to_owned(),
            scope: "workspace".to_owned(),
            file: None,
            symbol: None,
            test: None,
            diagnostic_class: "verification".to_owned(),
            summary,
            first_generation: generation,
            last_generation: generation,
            occurrence_count: 1,
            changed_details: Vec::new(),
            source_refs: vec![source_ref.to_owned()],
            root_fingerprint: None,
            cascade: false,
            transition: RepairLedgerTransition::New,
            repairs: Vec::new(),
        });
    }
    result
}

fn diagnostic_chunks(evidence: &[String]) -> Vec<String> {
    let mut chunks = Vec::new();
    for item in evidence {
        let lines = item.lines().collect::<Vec<_>>();
        if lines.len() <= 1 {
            chunks.push(item.clone());
            continue;
        }
        let mut current = String::new();
        for line in lines {
            if is_diagnostic_start(line) && !current.trim().is_empty() {
                chunks.push(std::mem::take(&mut current));
            }
            if !current.is_empty() {
                current.push('\n');
            }
            current.push_str(line);
        }
        if !current.trim().is_empty() {
            chunks.push(current);
        }
    }
    chunks
}

fn is_diagnostic_start(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.starts_with("error")
        || trimmed.starts_with("warning")
        || trimmed.starts_with("test ") && trimmed.ends_with("FAILED")
        || trimmed.contains(": error TS")
        || trimmed.contains(" - error TS")
        || trimmed.contains("[ERROR]")
}

fn looks_like_summary_only(value: &str) -> bool {
    let trimmed = value.trim();
    if trimmed.starts_with("$ ") && trimmed.lines().count() == 1 {
        return true;
    }
    let lower = trimmed.to_ascii_lowercase();
    lower.starts_with("error: could not compile")
        || lower.starts_with("test result: failed")
        || lower.starts_with("failures:")
}

fn infer_command(evidence: &[String], _sequence: u64) -> String {
    evidence
        .iter()
        .flat_map(|item| item.lines())
        .find_map(|line| line.trim().strip_prefix("$ ").map(str::to_owned))
        .unwrap_or_else(|| "authoritative-verification".to_owned())
}

fn classify(value: &str) -> String {
    let lower = value.to_ascii_lowercase();
    if lower.contains("clippy") || lower.contains("warning:") {
        "lint"
    } else if lower.contains("error ts") || lower.contains("typecheck") {
        "typecheck"
    } else if lower.contains("test ") && lower.contains("failed")
        || lower.contains("assertion")
        || lower.contains("panicked at")
    {
        "test"
    } else if lower.contains("architecture") || lower.contains("boundary") {
        "architecture"
    } else if lower.contains("error[") || lower.contains("error:") {
        "compile"
    } else {
        "verification"
    }
    .to_owned()
}

fn extract_file(value: &str) -> Option<String> {
    for line in value.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("-->") {
            return path_before_position(rest.trim());
        }
        if let Some(index) = trimmed.find(": error TS") {
            return path_before_position(&trimmed[..index]);
        }
        if let Some(index) = trimmed.find(" - error TS") {
            return path_before_position(&trimmed[..index]);
        }
    }
    None
}

fn path_before_position(value: &str) -> Option<String> {
    let cleaned = value.trim();
    let mut parts = cleaned.rsplitn(3, ':');
    let last = parts.next();
    let second = parts.next();
    let prefix = parts.next();
    if last.is_some_and(|part| part.chars().all(|ch| ch.is_ascii_digit()))
        && second.is_some_and(|part| part.chars().all(|ch| ch.is_ascii_digit()))
    {
        prefix.map(|path| path.trim().replace('\\', "/"))
    } else {
        Some(cleaned.replace('\\', "/"))
    }
}

fn extract_test(value: &str) -> Option<String> {
    value.lines().find_map(|line| {
        let trimmed = line.trim();
        trimmed
            .strip_prefix("test ")
            .and_then(|rest| rest.strip_suffix(" ... FAILED"))
            .map(str::to_owned)
            .or_else(|| {
                trimmed
                    .strip_prefix("---- ")
                    .and_then(|rest| rest.strip_suffix(" stdout ----"))
                    .map(str::to_owned)
            })
    })
}

fn extract_symbol(value: &str) -> Option<String> {
    value.lines().find_map(|line| {
        let trimmed = line.trim();
        trimmed
            .strip_prefix("in function `")
            .and_then(|rest| rest.strip_suffix('`'))
            .map(str::to_owned)
    })
}

fn compact_summary(value: &str) -> String {
    value
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .take(12)
        .collect::<Vec<_>>()
        .join(" | ")
        .chars()
        .take(1200)
        .collect()
}

fn normalize_for_fingerprint(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let mut digit_run = false;
    for ch in value.chars() {
        if ch.is_ascii_digit() {
            if !digit_run {
                output.push('#');
            }
            digit_run = true;
        } else {
            digit_run = false;
            output.push(ch.to_ascii_lowercase());
        }
    }
    output
}

fn add_non_verification_failure(
    entries: &mut Vec<RepairLedgerEntry>,
    class: &str,
    message: &str,
    generation: u64,
    source_ref: String,
) {
    let summary = compact_summary(message);
    let fingerprint = digest(format!("{class}|{}", normalize_for_fingerprint(&summary)).as_bytes());
    if let Some(existing) = entries.iter_mut().find(|entry| entry.fingerprint == fingerprint) {
        if !existing.source_refs.contains(&source_ref) {
            existing.occurrence_count = existing.occurrence_count.saturating_add(1);
            existing.source_refs.push(source_ref);
        }
        existing.last_generation = generation;
        existing.transition = RepairLedgerTransition::Persisted;
        return;
    }
    entries.push(RepairLedgerEntry {
        fingerprint,
        source: class.to_owned(),
        command: class.to_owned(),
        scope: "runtime".to_owned(),
        file: None,
        symbol: None,
        test: None,
        diagnostic_class: class.to_owned(),
        summary,
        first_generation: generation,
        last_generation: generation,
        occurrence_count: 1,
        changed_details: Vec::new(),
        source_refs: vec![source_ref],
        root_fingerprint: None,
        cascade: false,
        transition: RepairLedgerTransition::New,
        repairs: Vec::new(),
    });
}

fn collect_paths(value: &serde_json::Value, output: &mut BTreeSet<String>) {
    match value {
        serde_json::Value::String(value) if value.contains('/') || value.contains('\\') => {
            output.insert(value.replace('\\', "/"));
        }
        serde_json::Value::Array(values) => {
            for value in values {
                collect_paths(value, output);
            }
        }
        serde_json::Value::Object(values) => {
            for (key, value) in values {
                if matches!(
                    key.as_str(),
                    "path" | "file" | "files" | "changed_paths" | "modified_files"
                ) {
                    collect_paths(value, output);
                }
            }
        }
        _ => {}
    }
}

fn digest(value: &[u8]) -> String {
    format!("{:x}", Sha256::digest(value))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_multiple_diagnostics_and_keeps_expansion_handle() {
        let evidence = vec![r#"error[E0308]: mismatched types
  --> crates/a/src/lib.rs:12:3
error[E0425]: cannot find value `x`
  --> crates/b/src/lib.rs:8:9"#.to_owned()];
        let entries = parse_diagnostics(evidence.as_slice(), "cargo check", "journal#9", 3);
        assert_eq!(entries.len(), 2);
        assert!(entries.iter().all(|entry| entry.source_refs == ["journal#9"]));
        assert!(entries
            .iter()
            .any(|entry| entry.file.as_deref() == Some("crates/a/src/lib.rs")));
        assert!(entries
            .iter()
            .any(|entry| entry.file.as_deref() == Some("crates/b/src/lib.rs")));
    }

    fn entry(command: &str, file: &str, fingerprint: &str) -> RepairLedgerEntry {
        RepairLedgerEntry {
            fingerprint: fingerprint.to_owned(),
            source: "verification".to_owned(),
            command: command.to_owned(),
            scope: "crates".to_owned(),
            file: Some(file.to_owned()),
            symbol: None,
            test: None,
            diagnostic_class: "compile".to_owned(),
            summary: fingerprint.to_owned(),
            first_generation: 1,
            last_generation: 1,
            occurrence_count: 1,
            changed_details: Vec::new(),
            source_refs: vec!["journal#1".to_owned()],
            root_fingerprint: None,
            cascade: false,
            transition: RepairLedgerTransition::New,
            repairs: Vec::new(),
        }
    }

    #[test]
    fn passing_narrow_check_resolves_only_matching_command() {
        let mut entries = vec![
            entry("cargo check -p alpha", "crates/alpha/src/lib.rs", "alpha"),
            entry("cargo test -p beta", "crates/beta/src/lib.rs", "beta"),
        ];
        reconcile_generation(
            &mut entries,
            Vec::new(),
            2,
            &BTreeSet::new(),
            "repo-a",
            "cargo check -p alpha",
            true,
        );
        assert!(!entries[0].unresolved());
        assert!(entries[1].unresolved());
    }

    #[test]
    fn clusters_cascades_and_retains_new_generation_failures() {
        let evidence = vec![r#"error[E0308]: root mismatch
  --> crates/a/src/lib.rs:12:3
error[E0425]: cascading lookup failure
  --> crates/a/src/lib.rs:18:9"#.to_owned()];
        let mut first = parse_diagnostics(&evidence, "cargo check", "journal#1", 1);
        cluster_common_roots(&mut first);
        assert_eq!(first.len(), 2);
        assert!(!first[0].cascade);
        assert!(first[1].cascade);
        assert_eq!(first[1].root_fingerprint.as_ref(), Some(&first[0].fingerprint));

        let root = first[0].clone();
        let mut entries = first;
        let mut introduced = entry("cargo check", "crates/b/src/lib.rs", "introduced");
        introduced.first_generation = 2;
        introduced.last_generation = 2;
        reconcile_generation(
            &mut entries,
            vec![root, introduced],
            2,
            &BTreeSet::new(),
            "repo-a",
            "cargo check",
            false,
        );
        assert!(entries.iter().any(|entry| entry.fingerprint == "introduced" && entry.first_generation == 2));
        assert!(entries.iter().any(|entry| entry.transition == RepairLedgerTransition::Persisted));
    }

    #[test]
    fn normalization_deduplicates_location_only_changes() {
        let first = normalize_for_fingerprint("error E0308 at src/lib.rs:12:3 expected 4 values");
        let second = normalize_for_fingerprint("error E0308 at src/lib.rs:88:7 expected 9 values");
        assert_eq!(first, second);
    }
}
