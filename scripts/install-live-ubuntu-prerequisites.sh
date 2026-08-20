#!/usr/bin/env bash
set -euo pipefail

if [[ "${MEDUSA_FORCE_LIVE_APT_BOOTSTRAP:-0}" != "1" ]] && command -v bwrap >/dev/null 2>&1; then
  echo "bubblewrap already available; skipping apt bootstrap"
  exit 0
fi

apt_args=(
  -o Acquire::Retries=2
  -o Acquire::http::Timeout=15
  -o Acquire::https::Timeout=15
  -o Acquire::http::ConnectTimeout=10
  -o Acquire::https::ConnectTimeout=10
)

if ! timeout --signal=TERM --kill-after=10s 120s sudo apt-get "${apt_args[@]}" update; then
  echo "::error title=Live prerequisite unavailable::apt update failed or exceeded 120s while installing bubblewrap"
  exit 2
fi

if ! timeout --signal=TERM --kill-after=10s 120s sudo apt-get "${apt_args[@]}" install --yes bubblewrap; then
  echo "::error title=Live prerequisite unavailable::bubblewrap installation failed or exceeded 120s"
  exit 2
fi

command -v bwrap >/dev/null 2>&1 || {
  echo "::error title=Live prerequisite unavailable::bubblewrap installation completed without a bwrap executable"
  exit 2
}
