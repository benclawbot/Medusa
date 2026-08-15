from pathlib import Path

root_path = Path("crates/medusa-session-continuity/src/root.rs")
root = root_path.read_text()
old = '''                    changed_files: vec!["crates/medusa-session-continuity/src/root.rs".into()],
                    outcome: VerificationOutcome::Failed,
                }],'''
new = '''                    changed_files: vec!["crates/medusa-session-continuity/src/root.rs".into()],
                    outcome: VerificationOutcome::Failed,
                    hypothesis: "preserve continuity".into(),
                    repository_fingerprint: "repo-a".into(),
                }],'''
if old not in root:
    raise SystemExit("repair initializer marker missing")
root = root.replace(old, new, 1)
root_path.write_text(root)

ledger_path = Path("crates/medusa-runtime/src/repair_ledger.rs")
ledger = ledger_path.read_text()
old = '''        .unwrap_or_else(|| format!("authoritative-verification-{sequence}"))'''
new = '''        .unwrap_or_else(|| "authoritative-verification".to_owned())'''
if old not in ledger:
    raise SystemExit("stable command marker missing")
ledger = ledger.replace(old, new, 1)

old = '''fn infer_command(evidence: &[String], sequence: u64) -> String {'''
new = '''fn infer_command(evidence: &[String], _sequence: u64) -> String {'''
if old not in ledger:
    raise SystemExit("infer command signature marker missing")
ledger = ledger.replace(old, new, 1)

old = '''    for event in session.events.iter().filter(|event| event.sequence > cursor) {
        cursor = cursor.max(event.sequence);'''
new = '''    let starting_cursor = cursor;
    for event in session
        .events
        .iter()
        .filter(|event| event.sequence > starting_cursor)
    {
        cursor = cursor.max(event.sequence);'''
if old not in ledger:
    raise SystemExit("cursor iteration marker missing")
ledger = ledger.replace(old, new, 1)

old = '''fn looks_like_summary_only(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    lower.starts_with("error: could not compile")
        || lower.starts_with("test result: failed")
        || lower.starts_with("failures:")
}'''
new = '''fn looks_like_summary_only(value: &str) -> bool {
    let trimmed = value.trim();
    if trimmed.starts_with("$ ") && trimmed.lines().count() == 1 {
        return true;
    }
    let lower = trimmed.to_ascii_lowercase();
    lower.starts_with("error: could not compile")
        || lower.starts_with("test result: failed")
        || lower.starts_with("failures:")
}'''
if old not in ledger:
    raise SystemExit("summary filter marker missing")
ledger = ledger.replace(old, new, 1)
ledger_path.write_text(ledger)
