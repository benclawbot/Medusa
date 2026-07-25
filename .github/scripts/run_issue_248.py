from pathlib import Path

workflow = Path('.github/workflows/implement-issue-248.yml').read_text().splitlines()
start = workflow.index('        run: |') + 1
end = workflow.index('      - name: Format')
script = '\n'.join(line[10:] if line.startswith('          ') else line for line in workflow[start:end]) + '\n'
Path('/tmp/implement_issue_248.py').write_text(script)
exec(compile(script, '/tmp/implement_issue_248.py', 'exec'))
