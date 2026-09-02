from pathlib import Path

CHECKOUT = "actions/checkout@11d5960a326750d5838078e36cf38b85af677262"
LOCAL = "uses: ./.github/actions/setup-rust"

changed = []
for path in sorted(Path('.github/workflows').glob('*.yml')):
    text = path.read_text(encoding='utf-8')
    lines = text.splitlines(keepends=True)
    indexes = [i for i, line in enumerate(lines) if LOCAL in line]
    if not indexes:
        continue

    offset = 0
    for original_index in indexes:
        index = original_index + offset
        uses_line = lines[index]
        uses_indent = len(uses_line) - len(uses_line.lstrip(' '))
        step_indent = max(uses_indent - 2, 0)

        step_start = index
        while step_start > 0:
            previous = lines[step_start - 1]
            stripped = previous.lstrip(' ')
            indent = len(previous) - len(stripped)
            if indent == step_indent and stripped.startswith('- '):
                break
            step_start -= 1

        prefix = ''.join(lines[max(0, step_start - 5):step_start])
        if CHECKOUT in prefix:
            continue

        indent = ' ' * step_indent
        child = ' ' * (step_indent + 2)
        bootstrap = [
            f"{indent}- name: Bootstrap repository for local actions\n",
            f"{child}uses: {CHECKOUT} # v4\n",
        ]
        lines[step_start:step_start] = bootstrap
        offset += len(bootstrap)

    updated = ''.join(lines)
    if updated != text:
        path.write_text(updated, encoding='utf-8')
        changed.append(str(path))

if not changed:
    raise SystemExit('no setup-rust workflow consumers required repair')

print('repaired local-action bootstrap in:')
for path in changed:
    print(path)
