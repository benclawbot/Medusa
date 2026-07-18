#!/usr/bin/env bash
set -euo pipefail

checker="scripts/check-workflow-hygiene.sh"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

workflow_dir="$tmp/.github/workflows"
allowlist="$tmp/workflow-write-allowlist.txt"
mkdir -p "$workflow_dir"

run_check() {
  MEDUSA_WORKFLOW_DIR="$workflow_dir" \
  MEDUSA_WORKFLOW_WRITE_ALLOWLIST="$allowlist" \
    bash "$checker" >/dev/null 2>&1
}

expect_success() {
  local label="$1"
  if ! run_check; then
    echo "expected workflow hygiene success: $label" >&2
    exit 1
  fi
}

expect_failure() {
  local label="$1"
  if run_check; then
    echo "expected workflow hygiene failure: $label" >&2
    exit 1
  fi
}

cat > "$workflow_dir/safe.yml" <<'YAML'
name: Safe validation
on: [pull_request]
permissions:
  contents: read
jobs:
  validate:
    runs-on: ubuntu-latest
    steps:
      - run: echo safe
YAML
: > "$allowlist"
expect_success "read-only workflow"

cat > "$workflow_dir/unregistered-writer.yml" <<'YAML'
name: Unregistered writer
on: [workflow_dispatch]
permissions:
  contents: write
jobs:
  release:
    runs-on: ubuntu-latest
    steps:
      - run: echo release
YAML
expect_failure "unregistered contents write"
rm "$workflow_dir/unregistered-writer.yml"

cat > "$workflow_dir/release.yml" <<'YAML'
name: Registered release
on: [workflow_dispatch]
permissions:
  contents: write
jobs:
  release:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - run: echo release
YAML
printf '%s\n' '.github/workflows/release.yml|publishes reviewed GitHub releases' > "$allowlist"
expect_success "registered non-self-modifying writer"

cat >> "$workflow_dir/release.yml" <<'YAML'
      - run: git push origin HEAD:main
YAML
expect_failure "direct git push"

cat > "$workflow_dir/release.yml" <<'YAML'
name: Self-editing release
on: [workflow_dispatch]
permissions:
  contents: write
jobs:
  release:
    runs-on: ubuntu-latest
    steps:
      - run: rm -f .github/workflows/release.yml
YAML
expect_failure "workflow self-modification"

rm "$workflow_dir/release.yml"
expect_failure "stale allowlist entry"

echo "workflow-hygiene-tests-ok"
