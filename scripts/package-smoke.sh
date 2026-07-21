#!/usr/bin/env bash
set -euo pipefail
cargo build --release --locked --bin medusa --bin medusa-recall
medusa_binary="target/release/medusa"
recall_binary="target/release/medusa-recall"
"$medusa_binary" --version | grep -F 'medusa 1.0.0'
"$recall_binary" --help | grep -F 'medusa-recall'
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
cp "$medusa_binary" "$tmp/medusa"
cp "$recall_binary" "$tmp/medusa-recall"
"$tmp/medusa" --help | grep -F 'Autonomous coding agent'
"$tmp/medusa-recall" --help | grep -F 'search'
sha256sum "$tmp/medusa" "$tmp/medusa-recall" > "$tmp/SHA256SUMS"
test -s "$tmp/SHA256SUMS"
echo package-smoke-ok
