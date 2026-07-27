#!/usr/bin/env python3
from pathlib import Path

path = Path("crates/medusa-recovery-coordinator/src/view.rs")
text = path.read_text()

old = '''        let preview = input.selected_preview.as_ref();
        let destructive_conflict = preview.is_some_and(|value| {
'''
new = '''        let preview = input.selected_preview.as_ref();
        let selected_checkpoint_valid = preview.is_some_and(|value| {
            input.checkpoints.iter().any(|checkpoint| {
                checkpoint.id == value.checkpoint_id && checkpoint.integrity_verified
            })
        });
        let stale_or_untrusted_preview = preview.is_some() && !selected_checkpoint_valid;
        if stale_or_untrusted_preview {
            warnings.push(
                "The selected recovery preview does not reference an integrity-verified checkpoint; regenerate the preview."
                    .to_owned(),
            );
        }
        let destructive_conflict = preview.is_some_and(|value| {
'''
if old not in text:
    raise SystemExit("preview anchor not found")
text = text.replace(old, new, 1)

old = '''        let blocked = input.source_corrupt || !valid_checkpoint;
        let health = if input.source_corrupt {
            RecoveryHealth::Corrupt
        } else if blocked {
            RecoveryHealth::Blocked
'''
new = '''        let records_blocked = input.source_corrupt || !valid_checkpoint;
        let restore_blocked = records_blocked || stale_or_untrusted_preview;
        let health = if input.source_corrupt {
            RecoveryHealth::Corrupt
        } else if records_blocked || stale_or_untrusted_preview {
            RecoveryHealth::Blocked
'''
if old not in text:
    raise SystemExit("blocked anchor not found")
text = text.replace(old, new, 1)

text = text.replace(
    '''                !blocked && !input.containment_must_be_reestablished,
''',
    '''                !records_blocked && !input.containment_must_be_reestablished,
''',
    1,
)
text = text.replace(
    '''                } else if blocked {
                    "Recovery records are not trustworthy enough to resume."
''',
    '''                } else if records_blocked {
                    "Recovery records are not trustworthy enough to resume."
''',
    1,
)
text = text.replace(
    '''                !blocked && preview.is_some(),
''',
    '''                !restore_blocked && preview.is_some(),
''',
    1,
)
text = text.replace(
    '''                } else if blocked {
                    "Checkpoint integrity is not sufficient for restore."
                } else if destructive_conflict {
''',
    '''                } else if records_blocked {
                    "Checkpoint integrity is not sufficient for restore."
                } else if stale_or_untrusted_preview {
                    "The selected preview is stale or references an untrusted checkpoint."
                } else if destructive_conflict {
''',
    1,
)
text = text.replace(
    '''                !blocked
                    && matches!(
''',
    '''                !records_blocked
                    && matches!(
''',
    1,
)

anchor = '''    #[test]
    fn containment_recheck_blocks_resume() {
'''
tests = '''    #[test]
    fn missing_checkpoint_preview_fails_closed_without_blocking_resume() {
        let mut value = input();
        value.selected_preview = Some(RecoveryPreview {
            checkpoint_id: "missing".to_owned(),
            files: Vec::new(),
            unresolved_risks: Vec::new(),
            repository_matches_checkpoint_base: true,
        });
        let view = RecoveryView::build(value);
        assert_eq!(view.health, RecoveryHealth::Blocked);
        assert!(view.action(RecoveryOperation::Resume).unwrap().enabled);
        let restore = view.action(RecoveryOperation::RestoreCheckpoint).unwrap();
        assert!(!restore.enabled);
        assert!(restore.reason.contains("stale") || restore.reason.contains("untrusted"));
        assert!(view.warnings.iter().any(|warning| warning.contains("regenerate")));
    }

    #[test]
    fn integrity_failed_checkpoint_preview_fails_closed() {
        let mut value = input();
        value.checkpoints = vec![checkpoint("cp-bad", 1, false), checkpoint("cp-good", 2, true)];
        value.selected_preview = Some(RecoveryPreview {
            checkpoint_id: "cp-bad".to_owned(),
            files: Vec::new(),
            unresolved_risks: Vec::new(),
            repository_matches_checkpoint_base: true,
        });
        let view = RecoveryView::build(value);
        assert_eq!(view.health, RecoveryHealth::Blocked);
        assert!(view.action(RecoveryOperation::Resume).unwrap().enabled);
        assert!(!view.action(RecoveryOperation::RestoreCheckpoint).unwrap().enabled);
    }

    #[test]
    fn containment_recheck_blocks_resume() {
'''
if anchor not in text:
    raise SystemExit("test anchor not found")
text = text.replace(anchor, tests, 1)
path.write_text(text)
