#!/usr/bin/env bash
set -euo pipefail
cargo build --release --locked --bin medusa --bin medusa-recall --bin medusa-github-operation
medusa_binary="target/release/medusa"
recall_binary="target/release/medusa-recall"
github_operation_binary="target/release/medusa-github-operation"
workspace_version="$(
  awk '
    /^\[workspace\.package\]$/ { in_workspace_package = 1; next }
    /^\[/ && in_workspace_package { exit }
    in_workspace_package && /^version = "/ {
      sub(/^version = "/, "")
      sub(/".*$/, "")
      print
      exit
    }
  ' Cargo.toml
)"
test -n "$workspace_version"
version_line="$("$medusa_binary" --version)"
case "$version_line" in
  "medusa $workspace_version"|"medusa $workspace_version."*|"medusa $workspace_version "*) ;;
  *)
    printf 'unexpected medusa version: %s\n' "$version_line" >&2
    exit 1
    ;;
esac
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
if command -v sha256sum >/dev/null 2>&1; then
  sha256sum "$tmp/medusa" "$tmp/medusa-recall" "$tmp/medusa-github-operation" > "$tmp/SHA256SUMS"
else
  shasum -a 256 "$tmp/medusa" "$tmp/medusa-recall" "$tmp/medusa-github-operation" > "$tmp/SHA256SUMS"
fi
test -s "$tmp/SHA256SUMS"
echo package-smoke-ok
