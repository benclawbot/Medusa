#!/usr/bin/env bash
set -euo pipefail

workflow_dir="${MEDUSA_WORKFLOW_DIR:-.github/workflows}"
allowlist="${MEDUSA_WORKFLOW_WRITE_ALLOWLIST:-docs/workflow-write-allowlist.txt}"
failed=0

if [[ ! -d "$workflow_dir" ]]; then
  echo "missing workflow directory: $workflow_dir" >&2
  exit 2
fi

if [[ ! -f "$allowlist" ]]; then
  echo "missing workflow write allowlist: $allowlist" >&2
  exit 2
fi

declare -A allowed
while IFS='|' read -r path reason extra; do
  [[ -z "$path" || "$path" == \#* ]] && continue

  if [[ -n "${extra:-}" ]]; then
    echo "invalid workflow allowlist entry with extra fields: $path" >&2
    exit 2
  fi
  if [[ "$path" != .github/workflows/*.yml && "$path" != .github/workflows/*.yaml ]]; then
    echo "invalid workflow allowlist path: $path" >&2
    exit 2
  fi
  if [[ -z "$reason" ]]; then
    echo "missing workflow write justification for $path" >&2
    exit 2
  fi
  if [[ -n "${allowed[$path]:-}" ]]; then
    echo "duplicate workflow allowlist entry: $path" >&2
    exit 2
  fi

  allowed["$path"]="$reason"
done < "$allowlist"

mapfile -d '' workflows < <(
  find "$workflow_dir" -maxdepth 1 -type f \
    \( -name '*.yml' -o -name '*.yaml' \) -print0 | sort -z
)

if (( ${#workflows[@]} == 0 )); then
  echo "no workflow files found in $workflow_dir" >&2
  exit 2
fi

printf '%-72s %-16s\n' WORKFLOW CONTENTS_PERMISSION
for file in "${workflows[@]}"; do
  repository_path=".github/workflows/${file##*/}"
  permission="read-or-default"

  if grep -Eq '^[[:space:]]*contents:[[:space:]]*write([[:space:]]*(#.*)?)?$' "$file"; then
    permission="write"
    if [[ -z "${allowed[$repository_path]:-}" ]]; then
      echo "unregistered contents: write permission: $repository_path" >&2
      failed=1
    fi
  elif [[ -n "${allowed[$repository_path]:-}" ]]; then
    echo "stale workflow write allowlist entry: $repository_path no longer requests contents: write" >&2
    failed=1
  fi

  printf '%-72s %-16s\n' "$repository_path" "$permission"

  if grep -Ev '^[[:space:]]*#' "$file" \
    | grep -Eq '(^|[[:space:];|&])git[[:space:]]+push([[:space:]]|$)'; then
    echo "forbidden direct git push in workflow: $repository_path" >&2
    failed=1
  fi

  if grep -Ev '^[[:space:]]*#' "$file" \
    | grep -Eq '(^|[[:space:];|&])(rm|mv|cp|touch)[[:space:]][^#]*\.github/workflows/'; then
    echo "forbidden workflow self-modification in: $repository_path" >&2
    failed=1
  fi
done

for path in "${!allowed[@]}"; do
  file="$workflow_dir/${path##*/}"
  if [[ ! -f "$file" ]]; then
    echo "stale workflow write allowlist entry: $path does not exist" >&2
    failed=1
  fi
done

if (( failed != 0 )); then
  exit 1
fi

echo "workflow-hygiene-check-ok"
