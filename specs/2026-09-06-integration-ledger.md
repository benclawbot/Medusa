# Product quality program — integration ledger

## Authorization and baseline

- User requests full codebase investigation, TUI/desktop speed, production quality and UX, plus both updaters (failure to restart / apparently old version).
- User authorizes Luna High subagents, completion of stalled GitHub PRs, new PRs, and merges to main after review/required gates.
- After all merges: run full CI and all tests on integrated main, collect all failures, fix root causes together, rerun integrated validation. No live self-update or killing installed applications is authorized.
- Root checkout C:/Users/thoma/Medusa fast-forwarded main to e3b3c47e24632b4e40fb325fe2a0f13c3f89504d.
- Master spec: specs/2026-09-06-product-quality-and-updater.md. Complete scoped backlog, with evidence states and explicit limitations of coverage.

## GitHub tracking

The backlog is tracked with these issues:

- #1123 — Product quality program: Medusa UX, performance, reliability, and updater
- #1124 — Updater reliability: identity, Windows restart, health acknowledgement, and publication
- #1125 — Desktop and TUI usability: lifecycle correctness, navigation, input, accessibility, and UX
- #1126 — Measured performance and bounded resources across TUI, desktop, runtime, and storage
- #1127 — Core production correctness: journals, provider streaming, caches, memory, proxy, config, tools, and IPC
- #1128 — Finish and repair PR #1122: measured desktop and TUI hardening
- #1129 — Integrated main validation: run full CI and fix all failures after merge

## Workstreams

| Agent | Model | Worktree / local branch | Scope |
|---|---|---|---|
| implement_product_quality | Luna High | C:/Users/thoma/Medusa-review-updater / codex/improve-updater-reliability | U1–U5 updater PRs |
| finish_ui_pr1122 | Luna High | C:/Users/thoma/Medusa-review-ui / codex/continue-quality-pr1122 | Existing PR1122 then remaining D/P1–P4/T/Q work |
| core_production_prs | Luna High | C:/Users/thoma/Medusa-review-core / codex/improve-core-production | R1–R8 / core P5 measurements |

The initial Luna launch exhausted the account quota before producing usable patches. After the user reset usage, bounded updater, UI, and core workers were relaunched. Parent owns independent review/merge ordering/final integrated testing.

Agents send focused PR evidence before merges. UI agent is instructed to copy the master spec unchanged into PR1122 and update documentation inventory.

## Existing PR1122

- Title: Implement measured desktop and TUI hardening spec.
- Remote branch: codex/medusa-spec-p2-p5-t1-t2-q1-q3.
- Inspected head: 7a76859197db3a9fc22e9a1499cb825a6a38200c. Recheck before pushing because another connector worked on it.
- CI run34029929635: three rust-adapter failures and macOS bundle frontend resume assertion failure; Workspace quality canceled and several downstream jobs skipped. No merge yet.
- Parent review sent to agent: DOM bridge onboarding timeout still remains after AppLegacy wrapper; wake listener completion after close can leak/recreate state; watcher registration persists on spawn failure and treats any session as active; session page offsets may race atomic replacement; index maps need bounds; preserve fallback session selection with mismatched primary identity.
- Disproved parent hypothesis: runtime_wakeup wiring is present in lib.rs inline runtime module; agent notified not to change it. Adapter metadata shows test failures, not compile/lint failures. Complete diagnostics are in desktop-tests-Linux/macOS/Windows artifacts; gh job logs returned incomplete early output.

## Executed baseline checks

| Command | Result |
|---|---|
| npm test -- --reporter=dot (desktop) | PASS 40 files /173 tests, 57.60s |
| npm run build (desktop) | PASS; JS281.87kB/CSS128.26kB before gzip |
| cargo test -p medusa-update --all-targets --locked | PASS 42 unit +6 integration on Windows |
| python scripts/test-main-update-bundle.py | PASS 10 tests |
| python scripts/test-documentation.py | PASS documentation-tests-ok |
| python scripts/check-documentation.py | FAIL inventory stale after adding spec; UI agent to update inventory with spec |
| git diff --check (root before source changes) | PASS |

Desktop baseline code unchanged between40ffb00d and e3b3c47e (only two update packaging validation scripts changed). These are baseline results, not post-implementation certification.

## Updater local evidence

- Installed CLI --version reports main e3b3c47e2463.
- Desktop update state is rolled-back. Retained helper is older/different than source at main; precise historical error is not persisted.
- GitHub rolling run34020918575 succeeded for e3b3c47e and revision release has signed CLI/desktop assets. Earlier40ffb00d rolling run failed.
- No live update executed during investigation.

## Final CI plan

Use master spec exact commands plus affected workflows. ci.yml itself is pull_request/workflow_call; deep-validation.yml, desktop.yml, release-gates.yml, product-acceptance.yml and certifications have manual entry points (inspect before dispatch). Full tests must target final merged main SHA, not stale PR results. Collect all failing jobs before repairs; preserve meaningful assertions and investigate flakes. Credential/platform limitations remain explicit.

## Current completion state

Specification written. Implementation in progress; no new PR or merge success claimed. Full integrated tests NOT RUN pending merges.
