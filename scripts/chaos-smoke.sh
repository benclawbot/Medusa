#!/usr/bin/env bash
set -euo pipefail
cargo test -p medusa-hardening chaos_cycle_recovers_without_corruption -- --exact
cargo test -p medusa-daemon restart_marks_orphaned_jobs_interrupted -- --exact
echo chaos-smoke-ok
