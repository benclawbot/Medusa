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
ARTIFACTS="$(pwd)/live-e2e-artifacts"
REPO="$ROOT/multi-language-repair"
rm -rf "$ARTIFACTS"
mkdir -p "$ARTIFACTS" "$REPO/src"
trap 'rm -rf "$ROOT"' EXIT

git -C "$REPO" init -q -b main
git -C "$REPO" config user.name "Medusa Live E2E"
git -C "$REPO" config user.email "medusa-e2e@example.invalid"

cat > "$REPO/value.txt" <<'EOF'
41
EOF

cat > "$REPO/src/slugify.py" <<'EOF'
def slugify(value: str) -> str:
    raise NotImplementedError("implement me")
EOF

cat > "$REPO/src/counter.js" <<'EOF'
export function applyCounter(state, action) {
  if (action.type === 'increment') return { count: state.count - 1 };
  if (action.type === 'decrement') return { count: state.count + 1 };
  return state;
}
EOF

cat > "$REPO/package.json" <<'EOF'
{"type":"module","scripts":{"test":"node test.mjs"}}
EOF

cat > "$REPO/test.mjs" <<'EOF'
import assert from 'node:assert/strict';
import { applyCounter } from './src/counter.js';
assert.deepEqual(applyCounter({count: 2}, {type: 'increment'}), {count: 3});
assert.deepEqual(applyCounter({count: 2}, {type: 'decrement'}), {count: 1});
const original = {count: 2};
assert.equal(applyCounter(original, {type: 'noop'}), original);
console.log('verified-javascript-counter');
EOF

cat > "$REPO/verify.sh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

test "$(tr -d '\n' < value.txt)" = "42"
echo verified-rust-value-fix

python3 - <<'PY'
from src.slugify import slugify
assert slugify("Hello, World!") == "hello-world"
assert slugify("  Multiple   spaces  ") == "multiple-spaces"
assert slugify("Already-Slugged") == "already-slugged"
assert slugify("Crème brûlée") == "creme-brulee"
print("verified-python-slugify")
PY

npm test
EOF
chmod +x "$REPO/verify.sh"

git -C "$REPO" add -A
git -C "$REPO" commit -q -m baseline

VERIFIER_BEFORE="$(sha256sum "$REPO/verify.sh" | awk '{print $1}')"
TEST_BEFORE="$(sha256sum "$REPO/test.mjs" | awk '{print $1}')"
PACKAGE_BEFORE="$(sha256sum "$REPO/package.json" | awk '{print $1}')"

OBJECTIVE="Inspect this repository and repair all three product defects without modifying verify.sh, test.mjs, package.json, fixtures, or expected outputs. Correct value.txt to the verified value, robustly implement src/slugify.py while preserving its public API, and repair the counter transitions in src/counter.js. Run ./verify.sh, iterate until every check passes, and stop only after all three independent validations succeed."

echo "::group::live coding test: multi-language-repair"
"$MEDUSA" --repo "$REPO" run "$OBJECTIVE" 2>&1 | tee "$ARTIFACTS/multi-language-repair.log"
echo "::endgroup::"

VERIFIER_AFTER="$(sha256sum "$REPO/verify.sh" | awk '{print $1}')"
TEST_AFTER="$(sha256sum "$REPO/test.mjs" | awk '{print $1}')"
PACKAGE_AFTER="$(sha256sum "$REPO/package.json" | awk '{print $1}')"

test "$VERIFIER_BEFORE" = "$VERIFIER_AFTER"
test "$TEST_BEFORE" = "$TEST_AFTER"
test "$PACKAGE_BEFORE" = "$PACKAGE_AFTER"
test -x "$REPO/verify.sh"

(cd "$REPO" && ./verify.sh) | tee -a "$ARTIFACTS/multi-language-repair.log"
test "$(tr -d '\n' < "$REPO/value.txt")" = "42"
test -s "$REPO/src/slugify.py"
test -s "$REPO/src/counter.js"

mkdir -p "$ARTIFACTS/multi-language-repair"
git -C "$REPO" diff --binary > "$ARTIFACTS/multi-language-repair/change.patch"
git -C "$REPO" status --short > "$ARTIFACTS/multi-language-repair/status.txt"
if [[ -d "$REPO/.medusa/sessions" ]]; then
  cp -R "$REPO/.medusa/sessions" "$ARTIFACTS/multi-language-repair/sessions"
fi
printf '%s\n' \
  "name=multi-language-repair" \
  "objective=$OBJECTIVE" \
  "result=passed" \
  "independent_assertions=3" \
  > "$ARTIFACTS/multi-language-repair/result.txt"

printf '{"passed":3,"total":3,"sessions":1,"provider":"minimax","credential_persisted":false,"verification_contract_unchanged":true}\n' > "$ARTIFACTS/summary.json"
echo "live-coding-e2e-ok:3/3-in-one-session"
