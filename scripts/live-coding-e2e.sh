#!/usr/bin/env bash
set -euo pipefail
set +x

if [[ -z "${MINIMAX_API_KEY:-}" ]]; then
  echo "MINIMAX_API_KEY is required for live coding end-to-end tests" >&2
  exit 2
fi

cargo build --release --locked --bin medusa
MEDUSA="$(pwd)/target/release/medusa"
ROOT="$(mktemp -d)"
trap 'rm -rf "$ROOT"' EXIT

init_repo() {
  local repo="$1"
  mkdir -p "$repo"
  git -C "$repo" init -q -b main
  git -C "$repo" config user.name "Medusa Live E2E"
  git -C "$repo" config user.email "medusa-e2e@example.invalid"
}

run_case() {
  local name="$1"
  local objective="$2"
  local repo="$ROOT/$name"
  shift 2
  echo "::group::live coding test: $name"
  "$MEDUSA" --repo "$repo" run "$objective"
  (cd "$repo" && ./verify.sh)
  for assertion in "$@"; do
    test -e "$repo/$assertion"
  done
  echo "live-coding-test-ok:$name"
  echo "::endgroup::"
}

# Test 1: inspect and correct an off-by-one defect.
repo="$ROOT/rust-value-fix"
init_repo "$repo"
cat > "$repo/value.txt" <<'EOF'
41
EOF
cat > "$repo/verify.sh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
test "$(tr -d '\n' < value.txt)" = "42"
echo verified-rust-value-fix
EOF
chmod +x "$repo/verify.sh"
git -C "$repo" add -A
git -C "$repo" commit -q -m baseline
run_case rust-value-fix \
  "Inspect this repository, fix the failing off-by-one value, run the repository verification, and stop only when it passes." \
  value.txt

# Test 2: implement a missing Python function with edge-case behavior.
repo="$ROOT/python-slugify"
init_repo "$repo"
mkdir -p "$repo/src"
cat > "$repo/src/slugify.py" <<'EOF'
def slugify(value: str) -> str:
    raise NotImplementedError("implement me")
EOF
cat > "$repo/verify.sh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
python3 - <<'PY'
from src.slugify import slugify
assert slugify("Hello, World!") == "hello-world"
assert slugify("  Multiple   spaces  ") == "multiple-spaces"
assert slugify("Already-Slugged") == "already-slugged"
assert slugify("Crème brûlée") == "creme-brulee"
print("verified-python-slugify")
PY
EOF
chmod +x "$repo/verify.sh"
git -C "$repo" add -A
git -C "$repo" commit -q -m baseline
run_case python-slugify \
  "Implement the missing slugify function robustly, preserving the existing public API. Run verify.sh and iterate until every assertion passes." \
  src/slugify.py

# Test 3: repair a JavaScript state transition without changing the test contract.
repo="$ROOT/javascript-counter"
init_repo "$repo"
mkdir -p "$repo/src"
cat > "$repo/src/counter.js" <<'EOF'
export function applyCounter(state, action) {
  if (action.type === 'increment') return { count: state.count - 1 };
  if (action.type === 'decrement') return { count: state.count + 1 };
  return state;
}
EOF
cat > "$repo/package.json" <<'EOF'
{"type":"module","scripts":{"test":"node test.mjs"}}
EOF
cat > "$repo/test.mjs" <<'EOF'
import assert from 'node:assert/strict';
import { applyCounter } from './src/counter.js';
assert.deepEqual(applyCounter({count: 2}, {type: 'increment'}), {count: 3});
assert.deepEqual(applyCounter({count: 2}, {type: 'decrement'}), {count: 1});
const original = {count: 2};
assert.equal(applyCounter(original, {type: 'noop'}), original);
console.log('verified-javascript-counter');
EOF
cat > "$repo/verify.sh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
npm test
EOF
chmod +x "$repo/verify.sh"
git -C "$repo" add -A
git -C "$repo" commit -q -m baseline
run_case javascript-counter \
  "Diagnose and repair the counter state transitions without changing the test contract. Run verify.sh and finish only after it passes." \
  src/counter.js

echo "live-coding-e2e-ok:3/3"
