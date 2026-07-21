# Skill dependency lock receipts

Status: design and implementation in progress.

## Goal

Pin the exact approved project-skill graph used during a lifecycle transition so later restore, probation, graduation, and operator inspection can distinguish a valid graph from an unchanged graph.

Dependency validation currently proves that the graph is structurally safe at the moment it is resolved. Lock receipts add content integrity and explicit drift evidence without introducing automatic updates or lifecycle transitions.

## Receipt format

Each approved project skill may carry a generated `dependency-lock.json` beside `SKILL.md` and `dependencies.json`:

```json
{
  "schema_version": 1,
  "selected": "release",
  "order": ["repository-conventions", "verification", "release"],
  "skills": [
    {
      "name": "repository-conventions",
      "skill_sha256": "...",
      "manifest_sha256": "..."
    },
    {
      "name": "verification",
      "skill_sha256": "...",
      "manifest_sha256": "..."
    },
    {
      "name": "release",
      "skill_sha256": "...",
      "manifest_sha256": "..."
    }
  ],
  "graph_sha256": "..."
}
```

The graph digest is computed from a canonical serialization of the selected skill, deterministic topological order, skill digests, and manifest digests. Missing manifests use the SHA-256 digest of an empty dependency declaration rather than an ambiguous null value.

## Operator commands

```text
medusa skills lock-dependencies NAME
medusa skills lock-dependencies NAME --check
medusa skills lock-dependencies NAME --json
medusa skills verify-dependency-lock NAME
medusa skills verify-dependency-lock NAME --json
```

`lock-dependencies` is an explicit writer. It atomically replaces only the selected skill's generated lock receipt. `--check` computes and compares without writing. `verify-dependency-lock` is read-only and fails closed on missing, malformed, unsafe, or stale receipts.

## Lifecycle integration

- Quarantine preserves the existing lock receipt with the skill.
- Restore validates active dependencies and verifies the quarantined lock receipt before moving the skill back into the approved root.
- Probation records the verified graph digest alongside probation evidence.
- Graduation requires the current graph digest to match the digest verified when probation began.
- Dependency content or manifest drift is reported explicitly; Medusa never rewrites the lock automatically.
- Operators may deliberately refresh the receipt with `lock-dependencies` after reviewing the changed graph.

## Runtime policy

Runtime loading continues to resolve and validate the live graph. A present lock receipt is verified before instructions are injected. A missing receipt remains allowed during the initial rollout so existing approved skills continue to work, but stale or malformed receipts fail closed.

A later migration may make receipts mandatory after every approved skill has an explicit lock.

## Security and determinism

- SHA-256 is used for portable deterministic evidence.
- Paths remain confined to `.medusa/skills`.
- Symlinks remain rejected.
- Receipt serialization uses stable field order and lexical ordering where graph order does not already determine order.
- Writes use an atomic temporary-file rename within the selected skill directory.
- Lock verification never installs, restores, quarantines, graduates, or edits another skill.

## Verification coverage

Tests must cover deterministic receipts, diamond graphs, content drift, manifest drift, missing lock files, malformed receipts, wrong selected skill, reordered receipt entries, symlink rejection, atomic replacement, runtime rejection of stale locks, restore rejection, probation digest capture, and graduation drift rejection.

## Rollout boundary

This feature applies only to approved project skills in `.medusa/skills`. User-scoped and `.claude` compatibility skills remain outside the trust boundary until they have equivalent provenance and lifecycle controls.
