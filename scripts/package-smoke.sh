#!/usr/bin/env bash
set -euo pipefail
cargo build --release --locked --bin medusa --bin medusa-recall --bin medusa-github-operation
medusa_binary="target/release/medusa"
recall_binary="target/release/medusa-recall"
github_operation_binary="target/release/medusa-github-operation"
"$medusa_binary" --version | grep -F 'medusa 1.0.0'
"$recall_binary" --help | grep -F 'medusa-recall'
"$github_operation_binary" --help | grep -F 'medusa-github-operation'
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
cp "$medusa_binary" "$tmp/medusa"
cp "$recall_binary" "$tmp/medusa-recall"
cp "$github_operation_binary" "$tmp/medusa-github-operation"
"$tmp/medusa" --help | grep -F 'Autonomous coding agent'
"$tmp/medusa-recall" --help | grep -F 'search'
"$tmp/medusa-recall" --help | grep -F 'list'
"$tmp/medusa-github-operation" --help | grep -F 'interchangeable backend'
"$tmp/medusa" recall --help | grep -F 'search'
"$tmp/medusa" recall --help | grep -F 'list'
empty_repo="$tmp/empty-repo"
mkdir -p "$empty_repo"
"$tmp/medusa" recall --repo "$empty_repo" list | grep -F 'No recorded sessions.'
sha256sum "$tmp/medusa" "$tmp/medusa-recall" "$tmp/medusa-github-operation" > "$tmp/SHA256SUMS"
test -s "$tmp/SHA256SUMS"
echo package-smoke-ok
