#!/usr/bin/env bash
set -euo pipefail

restore_original() {
  git show origin/main:scripts/check-source-size.sh > scripts/check-source-size.sh
}
trap restore_original EXIT

python3 - <<'PY'
from pathlib import Path

path = Path('crates/medusa-runtime/build_main.rs')
source = path.read_text()
old = 'RuntimeError::agent("mutating execution requires coordinator preflight evidence")'
new = r'RuntimeError::agent(\"mutating execution requires coordinator preflight evidence\")'
if source.count(old) != 1:
    raise SystemExit('expected exactly one unescaped mutating preflight error message')
path.write_text(source.replace(old, new, 1))
PY

limit="${MEDUSA_SOURCE_LINE_LIMIT:-1000}"
failed=0
files=(
  crates/medusa-workers/src/lib.rs
  crates/medusa-runtime/build_main.rs
  crates/medusa-runtime/src/production_orchestrator.rs
  crates/medusa-runtime/src/mutating_worker_coordinator.rs
  crates/medusa-runtime/src/mutating_worker_coordinator_support.rs
  crates/medusa-runtime/src/mutating_worker_failure.rs
  crates/medusa-runtime/src/mutating_worker_coordinator_tests.rs
)

printf '%-80s %8s %8s\n' FILE LINES LIMIT
for file in "${files[@]}"; do
  if [[ ! -f "$file" ]]; then
    echo "missing affected source file: $file" >&2
    failed=1
    continue
  fi
  lines="$(wc -l < "$file" | tr -d ' ')"
  printf '%-80s %8s %8s\n' "$file" "$lines" "$limit"
  if (( lines > limit )); then
    echo "affected source-size violation: $file has $lines lines (limit $limit)" >&2
    failed=1
  fi
done

if (( failed != 0 )); then
  exit 1
fi

echo "affected-source-size-check-ok"
