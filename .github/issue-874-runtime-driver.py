import re
from pathlib import Path

src = Path('.github/workflows/issue-874-runtime-wiring.yml').read_text().splitlines()
start = next(i for i, line in enumerate(src) if "python3 - <<'PY'" in line) + 1
end = next(i for i in range(start, len(src)) if src[i].strip() == 'PY')
prefix = '          '
script = '\n'.join(
    line[len(prefix):] if line.startswith(prefix) else line for line in src[start:end]
) + '\n'

begin = script.index("anchor = '''    pub fn mutate")
finish = script.index("module = r'''", begin)
replacement = '''marker = "    pub fn handoff("
if marker not in s:
    raise SystemExit("handoff insertion marker missing")
project_task = r"""
    /// Replaces the journal-derived task projection without claiming frontend ownership.
    ///
    /// The canonical session journal remains the execution authority; this writes only the
    /// bounded deterministic projection consumed by continuity/resume.
    pub fn project_task(
        &self,
        event_id: &str,
        event: SessionEventKind,
        task: AuthoritativeTaskState,
    ) -> Result<ApplyOutcome, ContinuityError> {
        let current = self.load()?;
        self.update(
            current.revision,
            event_id,
            |session| {
                session.task = task;
                Ok(event)
            },
            "runtime-projection",
            0,
        )
    }

"""
root.write_text(s.replace(marker, project_task + marker, 1))

'''
script = script[:begin] + replacement + script[finish:]

script = script.replace(
    'AuthoritativeTaskState, CodingTrajectoryCheckpoint, DisprovedHypothesisCheckpoint,',
    'CodingTrajectoryCheckpoint, DisprovedHypothesisCheckpoint,',
)
script = re.sub(
    r'EventPayload::FileTransactionCommitted \{ receipt \} => \{\s*collect_paths\(receipt, &mut modified, &mut relevant\);\s*\}',
    '''EventPayload::FileTransactionCommitted { paths, .. } => {
                let value = serde_json::to_value(paths).map_err(RuntimeError::agent)?;
                collect_paths(&value, &mut modified, &mut relevant);
            }''',
    script,
    count=1,
)
script = re.sub(
    r'EventPayload::RuntimeFailed \{ message \}\s*\| EventPayload::SessionFailed \{ error: message \} => \{\s*let fingerprint = hex_digest\(message\.as_bytes\(\)\);\s*failures\.push\(FailureCheckpoint \{\s*fingerprint,\s*classification: "runtime"\.to_owned\(\),\s*summary: bounded\(message, 1000\),\s*repairs: Vec::new\(\),\s*\}\);\s*\}',
    '''EventPayload::RuntimeFailed { message } => {
                let fingerprint = hex_digest(message.as_bytes());
                failures.push(FailureCheckpoint {
                    fingerprint,
                    classification: "runtime".to_owned(),
                    summary: bounded(message, 1000),
                    repairs: Vec::new(),
                });
            }
            EventPayload::SessionFailed { error } => {
                let message = error.to_string();
                let fingerprint = hex_digest(message.as_bytes());
                failures.push(FailureCheckpoint {
                    fingerprint,
                    classification: "session".to_owned(),
                    summary: bounded(&message, 1000),
                    repairs: Vec::new(),
                });
            }''',
    script,
    count=1,
)
script = script.replace(
    'medusa_agent::record_session_event(&mut session, Actor::Verifier, EventPayload::VerificationCompleted',
    'medusa_agent::record_session_event(&mut session, Actor::System("verifier".to_owned()), EventPayload::VerificationCompleted',
)

old_resume = '''          pub(crate) fn render_for_resume(
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
              let Some(existing) = continuity.task.coding_trajectory else {
                  return Ok(None);
              };
              let mut restored = if provider_fallback {
                  existing
                      .restored_for_provider_fallback()
                      .map_err(RuntimeError::agent)?
              } else {
                  existing.restored_for_resume().map_err(RuntimeError::agent)?
              };
              let repository = repository_checkpoint(repo);
              restored.invalidate_for_repository_drift(repository);
              render(&restored).map(Some)
          }
'''
new_resume = '''          pub(crate) fn restore_for_resume(
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
'''
if old_resume not in script:
    raise SystemExit('resume helper anchor missing')
script = script.replace(old_resume, new_resume, 1)

script = script.replace(
    '''                  let first = sync_and_render(repo.path(), &session, Some("provider-native-1".to_owned())).expect("sync");
                  assert!(first.contains("keep public API stable"));
                  assert!(first.contains("verification"));
                  let restored = render_for_resume(repo.path(), &session, true).expect("restore").expect("context");
                  assert!(restored.contains("repair regression"));
                  assert!(!restored.contains("provider-native-1"));
                  fs::write(repo.path().join("tracked.txt"), "drift").expect("drift");
                  let drifted = render_for_resume(repo.path(), &session, false).expect("drift restore").expect("context");
                  assert!(drifted.contains("repository drift requires trajectory revalidation"));
''',
    '''                  let first = sync_and_render(repo.path(), &session, Some("provider-native-1".to_owned())).expect("sync");
                  assert!(first.contains("keep public API stable"));
                  assert!(first.contains("verification"));
                  medusa_agent::compact_session(&mut session, Some("repair regression"))
                      .expect("forced compaction");
                  let restored = restore_for_resume(repo.path(), &session, true).expect("restore").expect("context");
                  assert!(restored.contains("repair regression"));
                  assert!(restored.contains("keep public API stable"));
                  assert!(!restored.contains("provider-native-1"));
                  let stored = store(repo.path(), session.id.as_str()).load().expect("stored resume");
                  assert_eq!(stored.task.coding_trajectory.as_ref().expect("trajectory").resume_hops, 1);
                  fs::write(repo.path().join("tracked.txt"), "drift").expect("drift");
                  let drifted = restore_for_resume(repo.path(), &session, false).expect("drift restore").expect("context");
                  assert!(drifted.contains("repository drift requires trajectory revalidation"));
                  let drifted_stored = store(repo.path(), session.id.as_str()).load().expect("stored drift");
                  let drifted_trajectory = drifted_stored.task.coding_trajectory.as_ref().expect("trajectory");
                  assert_eq!(drifted_trajectory.resume_hops, 2);
                  assert!(drifted_trajectory.relevant_paths.iter().all(|path| path.stale));
''',
    1,
)

engine_begin = script.index(
    "old = '''                match engine.step_with_observer_and_context_and_turn_instruction_for_phase("
)
post_begin = script.index(
    "old = '''            let _ = events.send(RuntimeEvent::Progress { turn: session.turn });",
    engine_begin,
)
engine_replacement = '''marker = "                match engine.step_with_observer_and_context_and_turn_instruction_for_phase(\\n"
if marker not in s:
    raise SystemExit("engine call marker missing")
turn_prefix = """                let trajectory_context = crate::coding_trajectory::sync_and_render(
                    &state.repo,
                    &session,
                    None,
                )?;
                let turn_context = format!("{skill_context}\\n\\n{trajectory_context}");
"""
s = s.replace(marker, turn_prefix + marker, 1)
context_arg = "                    Some(skill_context.as_str()),"
if context_arg not in s:
    raise SystemExit("engine context argument missing")
s = s.replace(context_arg, "                    Some(turn_context.as_str()),", 1)
'''
script = script[:engine_begin] + engine_replacement + script[post_begin:]

post_begin = script.index(
    "old = '''            let _ = events.send(RuntimeEvent::Progress { turn: session.turn });"
)
post_finish = script.index("lib.write_text(s)", post_begin)
post_replacement = '''marker = "            let _ = events.send(RuntimeEvent::Progress { turn: session.turn });\\n"
if marker not in s:
    raise SystemExit("post-step marker missing")
replacement_line = marker + "            let _ = crate::coding_trajectory::sync_and_render(&state.repo, &session, None)?;\\n"
s = s.replace(marker, replacement_line, 1)
'''
script = script[:post_begin] + post_replacement + script[post_finish:]

script += r"""
error_path = Path('crates/medusa-runtime/src/error.rs')
error_source = error_path.read_text()
resume_anchor = '''        let restored_followups = restore_queued_followups(&session)?;
        let active_session_id = Some(session.id.to_string());
        state.session = Some(session);
'''
resume_replacement = '''        let restored_followups = restore_queued_followups(&session)?;
        let _ = crate::coding_trajectory::restore_for_resume(&repo, &session, false)?;
        let active_session_id = Some(session.id.to_string());
        state.session = Some(session);
'''
if resume_anchor not in error_source:
    raise SystemExit('runtime resume boundary anchor missing')
error_path.write_text(error_source.replace(resume_anchor, resume_replacement, 1))
"""

Path('/tmp/issue874.py').write_text(script)
exec(compile(script, '/tmp/issue874.py', 'exec'), {'__name__': '__main__'})
