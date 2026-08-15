from pathlib import Path

path = Path('.github/issue-874-runtime-driver.py')
source = path.read_text()
start = source.index("old_resume = '''")
end_marker = "script = script.replace(old_resume, new_resume, 1)\n"
end = source.index(end_marker, start) + len(end_marker)
replacement = r'''resume_begin = script.index("pub(crate) fn render_for_resume(")
resume_end = script.index("\nfn reduce(", resume_begin)
new_resume = r''' + "'''" + r'''pub(crate) fn restore_for_resume(
    repo: &Path,
    session: &AgentSession,
    provider_fallback: bool,
) -> Result<Option<String>, RuntimeError> {
    let store = store(repo, session.id.as_str());
    let continuity = match store.load() {
        Ok(value) => value,
        Err(medusa_session_continuity::ContinuityError::Io(error))
            if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(RuntimeError::agent(error)),
    };
    let Some(existing) = continuity.task.coding_trajectory.as_ref() else {
        return Ok(None);
    };
    let mut restored = if provider_fallback {
        existing
            .restored_for_provider_fallback()
            .map_err(RuntimeError::agent)?
    } else {
        existing.restored_for_resume().map_err(RuntimeError::agent)?
    };
    restored.invalidate_for_repository_drift(repository_checkpoint(repo));
    restored.validate().map_err(RuntimeError::agent)?;

    let mut task = continuity.task.clone();
    task.attention_required |= !restored.remaining_blockers.is_empty();
    task.verification_evidence = restored
        .verification_receipts
        .iter()
        .map(|receipt| format!("{}:{:?}", receipt.command, receipt.outcome))
        .collect();
    task.file_changes = restored.modified_files.clone();
    task.coding_trajectory = Some(restored.clone());
    let event_id = format!(
        "trajectory-resume:{}:{}:{}",
        session.id,
        restored.resume_hops,
        digest_json(&restored)?
    );
    let outcome = store
        .project_task(
            &event_id,
            SessionEventKind::TrajectoryRestored {
                resume_hops: restored.resume_hops,
            },
            task,
        )
        .map_err(RuntimeError::agent)?;
    let authoritative = outcome
        .session()
        .task
        .coding_trajectory
        .as_ref()
        .ok_or_else(|| RuntimeError::agent("restored trajectory projection disappeared"))?;
    render(authoritative).map(Some)
}
''' + "'''" + r'''
script = script[:resume_begin] + new_resume + script[resume_end:]

script = script.replace("render_for_resume(repo.path(), &session, true)", "restore_for_resume(repo.path(), &session, true)")
script = script.replace("render_for_resume(repo.path(), &session, false)", "restore_for_resume(repo.path(), &session, false)")
verification_assert = 'assert!(first.contains("verification"));'
if verification_assert not in script:
    raise SystemExit("verification assertion missing")
script = script.replace(
    verification_assert,
    verification_assert + '\n        medusa_agent::compact_session(&mut session, Some("repair regression"))\n            .expect("forced compaction");',
    1,
)
provider_assert = 'assert!(!restored.contains("provider-native-1"));'
if provider_assert not in script:
    raise SystemExit("provider fallback assertion missing")
script = script.replace(
    provider_assert,
    'assert!(restored.contains("keep public API stable"));\n        ' + provider_assert + '\n        let stored = store(repo.path(), session.id.as_str()).load().expect("stored resume");\n        assert_eq!(stored.task.coding_trajectory.as_ref().expect("trajectory").resume_hops, 1);',
    1,
)
drift_assert = 'assert!(drifted.contains("repository drift requires trajectory revalidation"));'
if drift_assert not in script:
    raise SystemExit("drift assertion missing")
script = script.replace(
    drift_assert,
    drift_assert + '\n        let drifted_stored = store(repo.path(), session.id.as_str()).load().expect("stored drift");\n        let drifted_trajectory = drifted_stored.task.coding_trajectory.as_ref().expect("trajectory");\n        assert_eq!(drifted_trajectory.resume_hops, 2);\n        assert!(drifted_trajectory.relevant_paths.iter().all(|path| path.stale));',
    1,
)
'''
path.write_text(source[:start] + replacement + source[end:])
