#!/usr/bin/env bash
set -euo pipefail

APT_GET="${MEDUSA_LIVE_APT_GET:-apt-get}"
APT_TIMEOUT_SECONDS="${MEDUSA_LIVE_APT_TIMEOUT_SECONDS:-120}"
APT_NETWORK_TIMEOUT_SECONDS="${MEDUSA_LIVE_APT_NETWORK_TIMEOUT_SECONDS:-15}"
APT_RETRIES="${MEDUSA_LIVE_APT_RETRIES:-1}"

fail_prerequisite() {
  local message="$1"
  echo "environment_prerequisite_failed: ${message}" >&2
  echo "::error title=Environment prerequisite failure::${message}" >&2
  exit 2
}

require_positive_integer() {
  local name="$1"
  local value="$2"
  if [[ ! "$value" =~ ^[1-9][0-9]*$ ]]; then
    fail_prerequisite "${name} must be a positive integer, got '${value}'"
  fi
}

require_nonnegative_integer() {
  local name="$1"
  local value="$2"
  if [[ ! "$value" =~ ^[0-9]+$ ]]; then
    fail_prerequisite "${name} must be a non-negative integer, got '${value}'"
  fi
}

require_positive_integer "MEDUSA_LIVE_APT_TIMEOUT_SECONDS" "$APT_TIMEOUT_SECONDS"
require_positive_integer "MEDUSA_LIVE_APT_NETWORK_TIMEOUT_SECONDS" "$APT_NETWORK_TIMEOUT_SECONDS"
require_nonnegative_integer "MEDUSA_LIVE_APT_RETRIES" "$APT_RETRIES"

if command -v bwrap >/dev/null 2>&1; then
  echo "bubblewrap already available; skipping apt bootstrap"
  exit 0
fi

command -v timeout >/dev/null 2>&1 || fail_prerequisite "GNU timeout is required to bound apt bootstrap"
command -v "$APT_GET" >/dev/null 2>&1 || fail_prerequisite "apt command '${APT_GET}' is unavailable"

if [[ "${MEDUSA_LIVE_APT_NO_SUDO:-0}" == "1" ]]; then
  PRIVILEGE=()
else
  command -v sudo >/dev/null 2>&1 || fail_prerequisite "sudo is required to install bubblewrap"
  PRIVILEGE=(sudo)
fi

APT_OPTIONS=(
  -o "Acquire::Retries=${APT_RETRIES}"
  -o "Acquire::http::Timeout=${APT_NETWORK_TIMEOUT_SECONDS}"
  -o "Acquire::https::Timeout=${APT_NETWORK_TIMEOUT_SECONDS}"
)

run_bounded_apt() {
  local operation="$1"
  shift
  if ! timeout --foreground "${APT_TIMEOUT_SECONDS}s" \
    "${PRIVILEGE[@]}" "$APT_GET" "${APT_OPTIONS[@]}" "$@"; then
    fail_prerequisite "apt ${operation} failed or exceeded ${APT_TIMEOUT_SECONDS}s while preparing the live sandbox"
  fi
}

run_bounded_apt update update
run_bounded_apt install install --yes bubblewrap

command -v bwrap >/dev/null 2>&1 \
  || fail_prerequisite "bubblewrap installation completed but 'bwrap' is still unavailable"

echo "bubblewrap prerequisite ready"
