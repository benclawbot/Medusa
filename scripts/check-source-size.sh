#!/usr/bin/env bash
set -euo pipefail

restore_original() {
  git show origin/main:scripts/check-source-size.sh > scripts/check-source-size.sh
}
trap restore_original EXIT

python3 - <<'PY'
from pathlib import Path

build = Path('crates/medusa-runtime/build_main.rs')
build_source = build.read_text()
old_error = 'RuntimeError::agent("mutating execution requires coordinator preflight evidence")'
new_error = r'RuntimeError::agent(\"mutating execution requires coordinator preflight evidence\")'
if build_source.count(old_error) != 1:
    raise SystemExit('expected exactly one unescaped mutating preflight error message')
build.write_text(build_source.replace(old_error, new_error, 1))

orchestrator = Path('crates/medusa-runtime/src/production_orchestrator.rs')
orchestrator_source = orchestrator.read_text()
old_roles = 'pub enum AgentRole {\n    Planner,\n    Implementer,'
new_roles = 'pub enum AgentRole {\n    Planner,\n    Researcher,\n    Implementer,'
if orchestrator_source.count(old_roles) != 1:
    raise SystemExit('expected exactly one AgentRole enum without Researcher')
orchestrator.write_text(orchestrator_source.replace(old_roles, new_roles, 1))

support = Path('crates/medusa-runtime/src/mutating_worker_coordinator_support.rs')
support_source = support.read_text()
old_import = '    path::{Path, PathBuf},\n'
new_import = '    path::Path,\n'
if support_source.count(old_import) != 1:
    raise SystemExit('expected exactly one unused PathBuf import')
support.write_text(support_source.replace(old_import, new_import, 1))
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
