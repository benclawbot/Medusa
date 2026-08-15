from pathlib import Path

src = Path('.github/workflows/issue-874-runtime-wiring.yml').read_text().splitlines()
start = next(i for i, line in enumerate(src) if "python3 - <<'PY'" in line) + 1
end = next(i for i in range(start, len(src)) if src[i].strip() == 'PY')
prefix = '          '
script = '\n'.join(
    line[len(prefix):] if line.startswith(prefix) else line for line in src[start:end]
) + '\n'

# Replace the brittle continuity whole-method patch with a structural insertion.
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

# Replace the brittle engine-call patch with a one-line structural insertion.
begin = script.index(
    "old = '''                match engine.step_with_observer_and_context_and_turn_instruction_for_phase("
)
finish_marker = "          s = s.replace(old, new, 1)\n"
finish = script.index(finish_marker, begin) + len(finish_marker)
replacement = '''marker = "                match engine.step_with_observer_and_context_and_turn_instruction_for_phase(\\n"
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
script = script[:begin] + replacement + script[finish:]

# Replace the post-step patch structurally as well.
begin = script.index(
    "old = '''            let _ = events.send(RuntimeEvent::Progress { turn: session.turn });"
)
finish = script.index(finish_marker, begin) + len(finish_marker)
replacement = '''marker = "            let _ = events.send(RuntimeEvent::Progress { turn: session.turn });\\n"
if marker not in s:
    raise SystemExit("post-step marker missing")
replacement_line = marker + "            let _ = crate::coding_trajectory::sync_and_render(&state.repo, &session, None)?;\\n"
s = s.replace(marker, replacement_line, 1)
'''
script = script[:begin] + replacement + script[finish:]

Path('/tmp/issue874.py').write_text(script)
exec(compile(script, '/tmp/issue874.py', 'exec'), {'__name__': '__main__'})
