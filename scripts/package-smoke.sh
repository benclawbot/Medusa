#!/usr/bin/env bash
set -euo pipefail
cargo build --release --locked --bin medusa
binary="target/release/medusa"
"$binary" --version | grep -F 'medusa 1.0.0'
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
cp "$binary" "$tmp/medusa"
"$tmp/medusa" --help | grep -F 'Autonomous coding agent'
sha256sum "$tmp/medusa" > "$tmp/SHA256SUMS"
test -s "$tmp/SHA256SUMS"
echo package-smoke-ok
