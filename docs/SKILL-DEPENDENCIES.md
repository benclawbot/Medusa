# Skill dependencies

Status: implemented for approved project skills in `.medusa/skills`.

## Goal

An approved Medusa skill may declare other approved project skills that must be loaded before it, while preserving deterministic context, explicit operator control, path confinement, and reversible lifecycle transitions.

## Manifest

Place `dependencies.json` beside the dependent skill's `SKILL.md`:

```json
{
  "schema_version": 1,
  "requires": ["repository-conventions", "verification"]
}
```

`requires` is optional and defaults to an empty list. Every entry must be a single safe skill directory name. Duplicate entries are rejected rather than silently normalized.

## Resolution policy

- Dependencies resolve only inside the approved project root `.medusa/skills`.
- Dependency resolution is centralized in the existing runtime skill loader, so every selected project skill follows the same validation and context-budget path.
- User-scoped and `.claude` compatibility skills remain independent because they do not yet share equivalent lifecycle receipts.
- Missing dependencies, self-dependencies, duplicates, malformed manifests, path traversal, symlink escapes, and dependency cycles are rejected.
- Resolution produces a deterministic topological order; lexical ordering breaks ties.
- Each dependency is loaded once and before every dependent.
- The existing 64,000-byte skill-context limit applies to the complete resolved graph.
- Invalid graphs fail closed before any selected skill instructions are injected.

## Lifecycle constraints

- Quarantine is refused while an active approved skill depends directly or transitively on the target.
- Restore validates the quarantined manifest and refuses the transition until every declared dependency is active.
- Probation continues to evaluate the restored skill using its own evidence.
- Graduation revalidates the complete dependency graph and is refused after graph drift.
- No command automatically quarantines, restores, graduates, installs, or rewrites a dependency.

## Operator commands

```text
medusa skills dependencies NAME
medusa skills dependencies NAME --json
medusa skills validate-dependencies
medusa skills validate-dependencies --json
```

`dependencies NAME` reports direct requirements, deterministic load order, and reverse dependents. `validate-dependencies` validates every approved project skill. JSON output is deterministic and suitable for CI and automation.

## Verification coverage

The dependency graph test suite covers deterministic diamond ordering, direct and transitive relationships, reverse dependents, missing dependencies, duplicate declarations, self-dependencies, readable cycle reporting, unsafe names, total byte-budget enforcement, and symlink escape rejection.

CLI and lifecycle tests compile and run with dependency inspection, validation, quarantine protection, restore validation, and graduation graph revalidation wired into the existing commands. The runtime and CLI crates pass Clippy with warnings denied. Refactor Guardrails confirms that all production source files remain within the 800-line ceiling and that workflow hygiene and compatibility baselines remain valid. The independent Tauri lockfile is refreshed so Desktop checks can continue using `--locked` consistently on Linux, Windows, and macOS.

## Rollout boundary

This implementation covers project-approved `.medusa/skills` only. Expanding dependency resolution to user-scoped or compatibility roots requires a separate lifecycle and precedence design so project skills cannot implicitly trust unmanaged instructions.
