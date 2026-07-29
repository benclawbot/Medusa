#!/usr/bin/env bash
set -euo pipefail
set +x

if [[ -z "${MINIMAX_API_KEY:-}" ]]; then
  echo "MINIMAX_API_KEY is required for live coding end-to-end tests" >&2
  exit 2
fi

LIVE_E2E_TIMEOUT_SECONDS="${LIVE_E2E_TIMEOUT_SECONDS:-1500}"
HEARTBEAT_SECONDS="${LIVE_E2E_HEARTBEAT_SECONDS:-60}"
STARTED_AT="$(date +%s)"
CURRENT_PHASE="initialization"
HEARTBEAT_PID=""

log_phase() {
  CURRENT_PHASE="$1"
  printf '[live-e2e] phase=%s elapsed=%ss\n' "$CURRENT_PHASE" "$(( $(date +%s) - STARTED_AT ))"
}

write_failure_summary() {
  local exit_code=$?
  mkdir -p "${ARTIFACTS:-live-e2e-artifacts}"
  printf '{"passed":0,"total":3,"sessions":1,"provider":"minimax","credential_persisted":false,"verification_contract_unchanged":false,"result":"failed","phase":"%s","exit_code":%d,"elapsed_seconds":%d}\n' \
    "$CURRENT_PHASE" "$exit_code" "$(( $(date +%s) - STARTED_AT ))" \
    > "${ARTIFACTS:-live-e2e-artifacts}/summary.json"
  printf 'name=multi-language-repair\nresult=failed\nphase=%s\nexit_code=%d\nelapsed_seconds=%d\n' \
    "$CURRENT_PHASE" "$exit_code" "$(( $(date +%s) - STARTED_AT ))" \
    > "${ARTIFACTS:-live-e2e-artifacts}/failure.txt"
  exit "$exit_code"
}

cleanup() {
  if [[ -n "$HEARTBEAT_PID" ]]; then
    kill "$HEARTBEAT_PID" 2>/dev/null || true
    wait "$HEARTBEAT_PID" 2>/dev/null || true
  fi
  rm -rf "${ROOT:-}"
}

trap write_failure_summary ERR
trap cleanup EXIT

log_phase "build-medusa"
cargo build --release --locked --bin medusa
MEDUSA="$(pwd)/target/release/medusa"
# Keep the fixture outside /tmp: Linux containment mounts a private tmpfs at
# /tmp, so a repository created there is intentionally invisible in bwrap.
ROOT="$(pwd)/target/live-e2e-work"
ARTIFACTS="$(pwd)/live-e2e-artifacts"
REPO="$ROOT/multi-language-repair"
rm -rf "$ROOT" "$ARTIFACTS"
mkdir -p "$ARTIFACTS" "$REPO/src" "$REPO/.medusa/sessions"
test -d "$REPO/.medusa/sessions"
test -w "$REPO/.medusa/sessions"

log_phase "prepare-fixture"
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

log_phase "autonomous-session"
printf '[live-e2e] session=multi-language-repair validations=3 timeout=%ss heartbeat=%ss\n' \
  "$LIVE_E2E_TIMEOUT_SECONDS" "$HEARTBEAT_SECONDS"
(
  while sleep "$HEARTBEAT_SECONDS"; do
    printf '[live-e2e] heartbeat phase=%s elapsed=%ss session=multi-language-repair\n' \
      "$CURRENT_PHASE" "$(( $(date +%s) - STARTED_AT ))"
  done
) &
HEARTBEAT_PID=$!

echo "::group::live coding session: multi-language-repair (3 independent validations)"
set +e
timeout --signal=TERM --kill-after=30s "${LIVE_E2E_TIMEOUT_SECONDS}s" \
  "$MEDUSA" --repo "$REPO" \
    --set model.provider=minimax \
    --set model.name=MiniMax-M3 \
    --set model.protocol=anthropic \
    --set model.base_url=https://api.minimax.io/anthropic \
    --set model.auth=api-key \
    --set model.tool_calling=true \
    --set model.streaming=false \
    run "$OBJECTIVE" 2>&1 | tee "$ARTIFACTS/multi-language-repair.log"
MEDUSA_STATUS=${PIPESTATUS[0]}
set -e
kill "$HEARTBEAT_PID" 2>/dev/null || true
wait "$HEARTBEAT_PID" 2>/dev/null || true
HEARTBEAT_PID=""
echo "::endgroup::"
if [[ "$MEDUSA_STATUS" -eq 124 || "$MEDUSA_STATUS" -eq 137 ]]; then
  echo "[live-e2e] autonomous session timed out after ${LIVE_E2E_TIMEOUT_SECONDS}s" >&2
  exit 124
fi
if [[ "$MEDUSA_STATUS" -ne 0 ]]; then
  echo "[live-e2e] autonomous session exited with status $MEDUSA_STATUS" >&2
  exit "$MEDUSA_STATUS"
fi

log_phase "verify-contract-integrity"
VERIFIER_AFTER="$(sha256sum "$REPO/verify.sh" | awk '{print $1}')"
TEST_AFTER="$(sha256sum "$REPO/test.mjs" | awk '{print $1}')"
PACKAGE_AFTER="$(sha256sum "$REPO/package.json" | awk '{print $1}')"

test "$VERIFIER_BEFORE" = "$VERIFIER_AFTER"
test "$TEST_BEFORE" = "$TEST_AFTER"
test "$PACKAGE_BEFORE" = "$PACKAGE_AFTER"
test -x "$REPO/verify.sh"

log_phase "run-independent-verification"
(cd "$REPO" && ./verify.sh) | tee -a "$ARTIFACTS/multi-language-repair.log"
test "$(tr -d '\n' < "$REPO/value.txt")" = "42"
test -s "$REPO/src/slugify.py"
test -s "$REPO/src/counter.js"

log_phase "collect-evidence"
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
  "elapsed_seconds=$(( $(date +%s) - STARTED_AT ))" \
  > "$ARTIFACTS/multi-language-repair/result.txt"

printf '{"passed":3,"total":3,"sessions":1,"provider":"minimax","credential_persisted":false,"verification_contract_unchanged":true,"result":"passed","elapsed_seconds":%d}\n' \
  "$(( $(date +%s) - STARTED_AT ))" > "$ARTIFACTS/summary.json"
log_phase "complete"
echo "live-coding-e2e-ok:3/3-in-one-session"
