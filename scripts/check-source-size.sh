#!/usr/bin/env bash
set -euo pipefail

limit="${MEDUSA_SOURCE_LINE_LIMIT:-1000}"
exceptions="${MEDUSA_SOURCE_SIZE_EXCEPTIONS:-docs/source-size-exceptions.txt}"
failed=0

if [[ ! -f "$exceptions" ]]; then
  echo "missing source-size exception registry: $exceptions" >&2
  exit 2
fi

declare -A allowed
while IFS='|' read -r path max_lines reason; do
  [[ -z "$path" || "$path" == \#* ]] && continue
  if [[ ! "$max_lines" =~ ^[0-9]+$ ]]; then
    echo "invalid exception limit for $path: $max_lines" >&2
    exit 2
  fi
  if [[ -z "$reason" ]]; then
    echo "missing exception rationale for $path" >&2
    exit 2
  fi
  allowed["$path"]="$max_lines"
done < "$exceptions"

printf '%-72s %8s %8s\n' FILE LINES LIMIT
while IFS= read -r -d '' file; do
  lines="$(wc -l < "$file" | tr -d ' ')"
  effective="$limit"
  if [[ -n "${allowed[$file]:-}" ]]; then
    effective="${allowed[$file]}"
  fi
  printf '%-72s %8s %8s\n' "$file" "$lines" "$effective"
  if (( lines > effective )); then
    echo "source-size violation: $file has $lines lines (limit $effective)" >&2
    failed=1
  fi
done < <(find crates -type f -path '*/src/*.rs' -print0 | sort -z)

for file in "${!allowed[@]}"; do
  if [[ ! -f "$file" ]]; then
    echo "stale source-size exception: $file does not exist" >&2
    failed=1
  fi
done

if (( failed != 0 )); then
  exit 1
fi

echo "source-size-check-ok"
