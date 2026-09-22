#!/usr/bin/env bash
set -euo pipefail

has_live_prerequisites() {
  command -v bwrap >/dev/null 2>&1 \
    && command -v pkg-config >/dev/null 2>&1 \
    && pkg-config --exists dbus-1
}

if [[ "${MEDUSA_FORCE_LIVE_APT_BOOTSTRAP:-0}" != "1" ]] && has_live_prerequisites; then
  echo "live Ubuntu prerequisites already available; skipping apt bootstrap"
  exit 0
fi

apt_args=(
  -o Acquire::Retries=2
  -o Acquire::http::Timeout=15
  -o Acquire::https::Timeout=15
  -o Acquire::http::ConnectTimeout=10
  -o Acquire::https::ConnectTimeout=10
)

if ! sudo bash "$(dirname "$0")/disable-google-chrome-apt-source.sh"; then
  echo "::warning title=APT source quarantine unavailable::continuing with the bounded prerequisite update" >&2
fi

if ! timeout --signal=TERM --kill-after=10s 120s sudo apt-get "${apt_args[@]}" update; then
  echo "::error title=Live prerequisite unavailable::apt update failed or exceeded 120s while installing live Ubuntu prerequisites"
  exit 2
fi

if ! timeout --signal=TERM --kill-after=10s 120s sudo apt-get "${apt_args[@]}" install --yes \
  bubblewrap \
  libdbus-1-dev \
  pkg-config
then
  echo "::error title=Live prerequisite unavailable::live Ubuntu prerequisite installation failed or exceeded 120s"
  exit 2
fi

command -v bwrap >/dev/null 2>&1 || {
  echo "::error title=Live prerequisite unavailable::bubblewrap installation completed without a bwrap executable"
  exit 2
}

command -v pkg-config >/dev/null 2>&1 && pkg-config --exists dbus-1 || {
  echo "::error title=Live prerequisite unavailable::DBus development metadata remains unavailable after installation"
  exit 2
}
