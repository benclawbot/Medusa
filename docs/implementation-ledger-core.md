# Core production implementation ledger

## 2026-09-06 — R3 retrieval memo contract

- Baseline: `codex/improve-core-production` at `e3b3c47e`.
- Observed source defect: `RetrievalMemo::retrieve_cached` consulted its map before validating the query and ledger. Its key concatenated query fields and ledger IDs/sequences without framing and omitted item content, so distinct valid ledgers could share a key.
- Change: validate both inputs before lookup; derive a versioned key from length framed canonical serde bytes for the full typed query and every ledger item; define an explicit default capacity of 64 while retaining the minimum capacity of one for `new(0)`.
- Regression evidence: `cargo test -p medusa-context-retrieval --locked` — PASS, 8 unit tests and 8 integration tests, 0 failed; `cargo fmt --all -- --check` — PASS; `git diff --check` — PASS.
- Pre-fix execution: not run in this worktree; the collision and validation tests are direct reproductions of the inspected pre-fix paths.
- Scope: public memo contract only. No production caller was found outside this crate and its tests.

