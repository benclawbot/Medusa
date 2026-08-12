# Refinement authority migration

Issue #819 moves production learned behavior to the repository-scoped canonical authority:

    .medusa/refinement-authority/
      journal.json       authoritative append-only lifecycle journal
      approvals.json     proposal-bound approval receipts
      privacy.json       canonical learning admission policy
      active.json        rebuildable active projection
      migrations.jsonl   one-way import receipts
      quarantine/        bounded corrupt or ambiguous source records
      authority.lock     short-lived cross-platform mutation lock

medusa-improvement owns this state. medusa-context only validates and projects lifecycle events, and remains filesystem-free. medusa-runtime is the only production command and turn-selection boundary. Desktop, TUI, daemon, and other frontends use that boundary.

## Migration behavior

Runtime startup, resume, and each relevant turn open the authority. When capture is enabled, RefinementMigrator scans the compatibility sources below and records a stable receipt. A source fingerprint and source record ID make reruns idempotent; a rerun is recorded as already_imported. A legacy status such as active, adopted, approved, or installed never creates a canonical approval or activation event.

| Compatibility source | Disposition |
| --- | --- |
| .medusa/learning-review/state.json | Import candidate items and valid privacy state; old active/review strings remain non-active unless a verifiable proposal-bound receipt exists. |
| .medusa/learnings.json and .medusa/user/learnings.json | Import scoped candidates; runtime no longer selects directly from these files. |
| .medusa/memory/lessons, learning proposals, skill proposals, improvement history | Import typed repository candidates with source evidence and quarantine malformed or unsupported records. |
| .medusa/engineering/improvements.json | Import untrusted compatibility candidates; local active, adopted, benchmark, and rollback fields do not authorize behavior. |

Corrupt JSON is copied to quarantine/ with a bounded receipt. A corrupt canonical journal or approval document is quarantined and selection fails closed. A missing or corrupt active.json is rebuilt from the valid journal and approval bindings.

## Commands and lifecycle

The shared runtime commands are:

/learning show [filter], /learning inspect ID, /learning propose repository|user|session KEY VALUE, /learning validate ID, /learning evaluate ID pass|fail, /learning approve ID, /learning activate ID, /learning supersede ID, /learning defer ID, /learning suspend ID, /learning rollback ID, /learning reject ID, /learning delete ID, /learning privacy, and /learning export.

Proposal, validation, evaluation, approval, activation, suspension, supersession, rollback, rejection, and deletion append canonical events with an expected revision. Stale revisions and concurrent writers fail without publishing a new projection. Activation requires a successful evaluation and an explicit approval receipt bound to the exact proposal digest.

Before a prompt is assembled, selection checks repository/user/session scope, artifact kind, objective match, explicit exclusions, and equal-precedence conflicts. The rendered prompt context and .medusa/learning-selection-audit.jsonl include the canonical proposal ID and version, evidence IDs, approval receipt ID, and journal head hash. A matching task can receive an active refinement; a nearby nonmatching task receives no content. Suspension, rollback, or tombstoning removes future selection.

## Compatibility and removal window

The old learning-review and scoped-memory APIs remain readable for the bounded migration window so existing data can be imported and exported. Their runtime lifecycle and retrieval paths no longer authorize production behavior. Desktop engineering records are displayed as read-only compatibility candidates until issue #824 replaces that dashboard. Executable skill activation remains governed by issue #760. Removing old files is intentionally outside #819.

Privacy is fail-closed. The canonical privacy.json is checked before migration, proposal admission, retrieval, and telemetry. If the canonical file is absent, the valid legacy privacy state is imported once; if a privacy file is corrupt, learning is unavailable rather than silently enabled.
