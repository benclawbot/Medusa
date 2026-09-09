#!/usr/bin/env bash
set -euo pipefail

# Hosted Ubuntu images can retain a stale Google Chrome package index. It is
# unrelated to Medusa's Linux dependencies and can make otherwise deterministic
# bootstrap steps fail with an APT hash mismatch. Disable that optional source
# for the lifetime of this ephemeral runner before updating the system index.
if [[ ! -d /etc/apt/sources.list.d ]]; then
  exit 0
fi

for source in /etc/apt/sources.list.d/*; do
  [[ -f "$source" ]] || continue
  if grep -qi 'dl.google.com/linux/chrome-stable' "$source"; then
    echo "Disabling stale third-party APT source: $source" >&2
    mv -- "$source" "$source.medusa-disabled"
  fi
done
