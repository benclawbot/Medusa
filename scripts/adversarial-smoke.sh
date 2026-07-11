#!/usr/bin/env bash
set -euo pipefail

run_named_test() {
  local package="$1"
  local test_name="$2"
  local output
  echo "::group::adversarial test: $package::$test_name"
  set +e
  output="$(cargo test -p "$package" "$test_name" --locked -- --nocapture 2>&1)"
  local status=$?
  set -e
  printf '%s\n' "$output"
  if [[ $status -ne 0 ]]; then
    echo "adversarial test failed: $package::$test_name" >&2
    exit "$status"
  fi
  if ! grep -Eq 'test result: ok\. 1 passed; 0 failed' <<<"$output"; then
    echo "required adversarial test is missing, ambiguous, or did not run exactly once: $package::$test_name" >&2
    exit 1
  fi
  echo "::endgroup::"
}

run_named_test medusa-agent parent_path_escape_is_denied
run_named_test medusa-agent symlink_escape_is_denied
run_named_test medusa-agent dangerous_shell_commands_are_denied
run_named_test medusa-agent sandbox_blocks_network_and_external_writes
run_named_test medusa-agent patch_apply_tool_uses_guarded_transaction
run_named_test medusa-workers parallel_feature_fixture_merges_and_verifies
run_named_test medusa-workers conflicting_workers_abort_cleanly
run_named_test medusa-extensions malicious_mcp_cannot_read_secret_or_redefine_policy
run_named_test medusa-hardening operational_events_redact_credentials
run_named_test medusa-hardening arbitrary_archive_paths_never_escape

echo adversarial-smoke-ok
