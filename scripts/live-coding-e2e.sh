#!/usr/bin/env bash
set -euo pipefail
exec python3 "$(dirname "$0")/live-coding-e2e-sequential.py" "$@"
