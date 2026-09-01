# Medusa Security Model

Single-page index of Medusa's security posture. The information is sourced from the
canonical documents linked in each section; this page exists to give external reviewers,
auditors, and enterprise evaluators the answers to standard questions without reading the
whole docs tree.

## 1. Trust boundary

The trust boundary separates Medusa's coordinated runtime (trusted) from the platform
containment layer (boundary) and the external surface (untrusted). Workspace writes are
path-checked, symlink-aware where supported, and transactional. Shell execution fails
closed when the required containment backend is unavailable.

See [`docs/ARCHITECTURE.md` § Containment trust boundary](ARCHITECTURE.md#containment-trust-boundary)
for the full diagram and prose.

## 2. Threat model

**In scope — defended against:**

- Untrusted tool output (path-traversal, symlink redirection) — path-checked, transactional, fail-closed on symlinks in directory mutation.
- Untrusted fetched content used in prompts — containment boundary prevents exfiltration to outside declared paths.
- Errant or compromised worker behavior — isolated worktrees, immutable candidates, mandatory verification gate, parent reviewer with no tools.
- Cancellation / crash mid-mutation — durable `.medusa/` authority + interrupted-lease recovery (new epoch, no success rewrite).
- Race conditions on session continuity, atomic writes, file URIs — race-safe creation, atomic_write helpers, validated session IDs before filesystem path construction (see recent security PRs #1066, #1067, #1068, #1069, #1070).
- Supply-chain drift — committed `Cargo.lock` is the authority for CI, certification, SBOM, and release builds. Locked validation must fail if the manifest is ahead of the lock.

**Out of scope (explicit non-goals):**

- LLM-driven misuse at the prompt level (e.g. a user instructing the agent to do something
  harmful) — Medusa applies containment, not policy. Operators are responsible for
  acceptable-use controls.
- Prompt injection from fetched web content reaching tool execution — bounded by
  containment, not eliminated.
- Autonomous multi-host transactions, recursive subagent delegation, unconstrained
  model-driven team expansion — explicitly outside the production entrypoint. See
  [`docs/ARCHITECTURE.md` § Delegation boundary](ARCHITECTURE.md#orchestration-and-parentsubagent-responsibility).
- Non-Git parallel mutation — only Git workspaces support the bounded DAG.

## 3. Containment per platform

| Platform | Mechanism | Authority |
|---|---|---|
| Linux | Bubblewrap | [`docs/issue-308-*.md`](issue-308-native-windows-sandbox.md), `medusa-process-containment` |
| macOS | Seatbelt | `medusa-process-containment`, recent fix in PR #1035 (analysis sandbox startup traversal) |
| Windows 11 | Composable sandbox (`Experimental_CreateProcessInSandbox`) | [`docs/issue-308-windows-sandbox.md`](issue-308-windows-sandbox.md) |

Shell execution **fails closed** when the required containment backend is unavailable on
the host. Required UI-change verification uses the Node.js browser sidecar as an
internal verification boundary; model-executable browser actions remain quarantined until
their dispatcher, permissions, and authenticated behavioral evidence are certified.

## 4. Verification gate

A "successful model output" is not a "successful workspace mutation". A coordinated
mutation is complete only after, in order:

1. Isolated verification (typed changed-scope validation, targeted candidate checks)
2. Aggregate verification (when parallel Git children)
3. Dedicated zero-tool parent reviewer
4. Independent immutable-candidate verification
5. Authorization
6. Guarded integration / reconciliation
7. Configured primary-workspace verification (e.g. `verify.sh`, `verify.ps1`, recognized project verification)

Missing prerequisites produce explicit failure evidence rather than a false pass.

See [`docs/PRODUCTION-EXECUTION-TRACE.md`](PRODUCTION-EXECUTION-TRACE.md),
[`docs/CAPABILITY-EVIDENCE.md`](CAPABILITY-EVIDENCE.md), and
[`docs/verification-gate-claim`](CAPABILITY-CLAIMS.json) (workspace metadata
`verification_gate = "typed-evidence-and-changed-component-authority"`).

## 5. Durability vs. irreversibility

Two separate concepts, explicitly separated:

- **Reversible-effect journal** — what Medusa controls and can roll back (in-memory state,
  workspace transactions before integration, candidate revisions).
- **External commit** — what has already hit the filesystem or Git remote and cannot be
  rolled back by Medusa alone.

The runtime does not claim to roll back external commits; recovery stops at the last
authoritative state under `.medusa/`. Git integration rolls back to the pre-batch HEAD on
conflict; directory integration restores changed primary paths from a rollback copy if
application fails.

See [`docs/durable-journal-policy.md`](durable-journal-policy.md) and
[`docs/EXECUTION-DURABILITY.md`](EXECUTION-DURABILITY.md).

## 6. Out of scope (architectural)

The production entrypoint explicitly excludes:

- Autonomous nested delegation (workers cannot spawn workers or expand contracts).
- Unconstrained model-driven team expansion.
- Consensus voting.
- Distributed multi-host transactions.
- Non-Git parallel mutation.

Only the root coordinator creates workers. The parent remains a read-only lead.

See [`docs/ARCHITECTURE.md` § Delegation boundary](ARCHITECTURE.md#orchestration-and-parentsubagent-responsibility).

## 7. Reporting vulnerabilities

See [`SECURITY.md`](../SECURITY.md) at the repo root for the supported disclosure channels
and response expectations.

## 8. Recent security work (audit trail)

The capability-evidence ledger and recent closed issues show a sustained security program:

- PR #1074 — fix correctness, security, storage, CI gaps (merged)
- PR #1059 — fix high-severity verified findings (closed, superseded)
- Issues #1060–#1073 — session-continuity race-safety, containment serialization hardening,
  daemon Submit gating through tool policy, atomic-write consolidation, capability-evidence
  validation script repair.

A capability-changing pull request must update the applicable legacy claim, v2 inventory,
index, tests, and deletion target or explain why no record changes. See
[`docs/CONTRIBUTOR-ARCHITECTURE.md`](CONTRIBUTOR-ARCHITECTURE.md).
