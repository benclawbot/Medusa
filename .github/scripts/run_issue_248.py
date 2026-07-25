from pathlib import Path
import traceback

try:
    workflow = Path('.github/workflows/implement-issue-248.yml').read_text().splitlines()
    start = workflow.index('        run: |') + 1
    end = workflow.index('      - name: Format')
    script = '\n'.join(line[10:] if line.startswith('          ') else line for line in workflow[start:end]) + '\n'
    brittle = """if toggle_anchor not in text:
    raise SystemExit('Ctrl+T anchor not found')
text = text.replace(toggle_anchor, toggle_replacement, 1)
"""
    robust = """if toggle_anchor in text:
    text = text.replace(toggle_anchor, toggle_replacement, 1)
else:
    pattern = re.compile(
        r\"(?P<indent>\\s*)if key\\.code == KeyCode::Char\\('t'\\)\\s*\\n\"
        r\"(?P=indent)\\s*&& key\\.modifiers\\.contains\\(KeyModifiers::CONTROL\\)\\s*\\{\\s*\\n\"
        r\"(?P=indent)\\s*self\\.task_list_visible = !self\\.task_list_visible;\\s*\\n\"
        r\"(?P=indent)\\s*return Ok\\(AppAction::Redraw\\);\\s*\\n\"
        r\"(?P=indent)\\}\\s*\\n\"
    )
    match = pattern.search(text)
    if match is None:
        raise SystemExit('Ctrl+T anchor not found')
    indent = match.group('indent')
    replacement = match.group(0) + (
        f\"{indent}if key.code == KeyCode::Char('e')\\n\"
        f\"{indent}    && key.modifiers.contains(KeyModifiers::CONTROL)\\n\"
        f\"{indent}{{\\n\"
        f\"{indent}    self.activity_details_expanded = !self.activity_details_expanded;\\n\"
        f\"{indent}    return Ok(AppAction::Redraw);\\n\"
        f\"{indent}}}\\n\"
    )
    text = text[:match.start()] + replacement + text[match.end():]
"""
    if brittle not in script:
        raise RuntimeError('runner could not patch Ctrl+T transformation block')
    script = script.replace(brittle, robust, 1)
    Path('/tmp/implement_issue_248.py').write_text(script)
    exec(compile(script, '/tmp/implement_issue_248.py', 'exec'))
except BaseException:
    Path('issue248-error.txt').write_text(traceback.format_exc())
    raise
