#!/usr/bin/env bash
set -euo pipefail
PROPTEST_CASES=1024 cargo test -p medusa-hardening arbitrary_archive_paths_never_escape -- --exact
PROPTEST_CASES=1024 cargo test -p medusa-hardening redaction_never_emits_known_secret -- --exact
cargo test -p medusa-protocol tampering_is_detected -- --exact
echo fuzz-smoke-ok
