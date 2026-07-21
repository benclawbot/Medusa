# Skill dependencies

Status: implementation contract for the next skill-lifecycle feature.

## Goal

Allow an approved Medusa skill to declare other approved skills that must be loaded before it, while preserving deterministic context, explicit operator control, path confinement, and reversible lifecycle transitions.

## Manifest

A project skill may include `.medusa/skills/<name>/dependencies.json` beside `SKILL.md`:

```json
{
  "schema_version": 1,
  "requires": ["repository-conventions", "verification"]
}
```

`requires` is optional and defaults to an empty list. Every entry must be a single safe skill directory name. Duplicate entries are rejected rather than silently normalized.

## Resolution policy

- Resolve dependencies only inside the same approved project skill root.
- User-scoped and `.claude` compatibility skills remain independent until their lifecycle can be governed by equivalent receipts.
- Reject missing dependencies, self-dependencies, duplicate declarations, malformed manifests, path traversal, and dependency cycles.
- Produce one deterministic topological order; lexical ordering breaks ties between otherwise independent skills.
- Load each dependency once and before every dependent.
- Apply the existing total skill-context byte budget to the complete resolved graph, not to each file independently.
- Failure is closed: an invalid graph prevents the selected skill from being injected and reports the exact dependency chain.

## Lifecycle constraints

- Quarantine is refused while any active approved skill depends directly or transitively on the target.
- Restore is refused until every declared dependency is active and valid.
- Probation evaluates the restored skill normally; dependency outcomes are not attributed to the dependent without separate evidence.
- Graduation is refused if the dependency graph has become invalid since probation passed.
- No command automatically quarantines, restores, graduates, installs, or rewrites a dependency.

## Operator surface

```text
medusa skills dependencies NAME
medusa skills dependencies NAME --json
medusa skills validate-dependencies
medusa skills validate-dependencies --json
```

The human view shows direct dependencies, transitive order, reverse dependents, and lifecycle blockers. JSON output is deterministic and suitable for CI.

## Required tests

- empty and single-node graphs
- deterministic diamond ordering
- direct and transitive dependencies
- missing dependency rejection
- self-dependency rejection
- duplicate declaration rejection
- cycle detection with a readable chain
- traversal and nested-name rejection
- symlink escape rejection
- total byte-budget enforcement
- quarantine blocked by direct and transitive dependents
- restore blocked by unavailable dependencies
- graduation blocked by graph drift
- JSON output determinism

## Rollout boundary

The first implementation covers project-approved `.medusa/skills` only. Expanding dependency resolution to user-scoped or compatibility roots requires a separate lifecycle and precedence design so project skills cannot implicitly trust unmanaged instructions.
