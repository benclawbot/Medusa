# Medusa Next Steps

## Post-1.0 Modularization Refactor

The first post-release engineering priority is to split the largest source files into smaller, cohesive modules without changing externally observable behavior.

## Current baseline

The production-hardening phase is merged and the latest release-gate run is green across workspace coverage, named adversarial regressions, fuzzing, security, migrations, chaos recovery, documentation, Linux/macOS/Windows packaging, and the live MiniMax coding scenario.

The current measured workspace line coverage is approximately 75.46%. The enforced non-regression floor is 75%. Reaching 90% remains a separate quality objective and must be achieved through meaningful assertions, not by excluding low-coverage crates or executing lines without validating behavior.

## Why this matters

Several implementation files currently combine protocol definitions, validation, persistence, execution, policy, and test fixtures. This made rapid end-to-end implementation possible, but it increases review cost, merge-conflict risk, compile-time coupling, and the chance that future changes accidentally cross subsystem boundaries.

## Primary targets

1. `crates/medusa-agent/src/lib.rs`
   - `engine.rs`: session lifecycle, model loop, step transitions
   - `session.rs`: persistence, reload, checksums, evidence
   - `tools/mod.rs`: tool registry and dispatch
   - `tools/filesystem.rs`: read/write and symlink-safe containment
   - `tools/shell.rs`: sandbox backend, command policy, output capture
   - `tools/git.rs`: checkpoint and Git policy
   - `tools/intelligence.rs`: code index, patch transactions, symbol rename
   - `verification.rs`: targeted verification and evidence
   - `policy.rs`: shared runtime policy decisions

2. `crates/medusa-intelligence/src/lib.rs`
   - `index.rs`: Tree-sitter parsing, symbols, references
   - `patch.rs`: transactions, overlap/staleness checks, atomic commit
   - `format.rs`: formatter selection and execution
   - `impact.rs`: test-impact analysis
   - `language.rs`: language adapters and parser registry

3. `crates/medusa-memory/src/lib.rs`
   - `schema.rs`: Markdown/frontmatter types
   - `proposal.rs`: proposal validation and secret checks
   - `store.rs`: canonical Markdown persistence
   - `index.rs`: rebuildable SQLite index
   - `retrieval.rs`: scoring and filtering
   - `lifecycle.rs`: reuse, supersession, compaction

4. `crates/medusa-extensions/src/lib.rs`
   - `skills.rs`: skill loading, provenance, checksums
   - `hooks.rs`: hook contracts and execution
   - `mcp.rs`: isolated MCP transport and poisoning defenses
   - `browser.rs`: Playwright sidecar contract and evidence validation
   - `redaction.rs`: shared output redaction

5. `crates/medusa-hardening/src/lib.rs`
   - `migrations.rs`: schema upgrades and rollback
   - `observability.rs`: events, counters, durations, redaction
   - `release.rs`: manifests and package validation
   - `archive.rs`: archive-path safety
   - `chaos.rs`: recovery fixtures

## Refactoring rules

- Preserve public APIs unless a separate migration note is approved.
- Move code in behavior-preserving commits before redesigning it.
- Keep each production module focused on one responsibility.
- Prefer private modules and narrow re-exports over broad public surfaces.
- Keep test helpers in `tests/`, `testkit`, or `#[cfg(test)]` modules rather than production paths.
- Do not weaken sandbox, path-containment, memory-validation, secret-redaction, or rollback controls during extraction.
- Maintain at least the current 75% workspace line-coverage floor throughout the refactor.
- Raise coverage incrementally with meaningful tests, targeting 80%, then 85%, then 90%.
- Treat coverage and adversarial behavior gates as independent release requirements.
- Keep each refactor pull request behavior-preserving and limited to one crate or one tightly coupled extraction.

## Required adversarial regression suite

The following named behaviors must remain explicit release checks. Passing the line-coverage threshold does not substitute for any of them.

- **Symlink escape:** a repository-local symlink pointing outside the repository must be rejected by every read, write, patch, search, and rename path.
- **Traversal and archive escape:** absolute paths, `..`, duplicate archive entries, and platform-specific root/prefix paths must be denied.
- **Hard-deny bypasses:** reject force pushes, destructive Git cleanup/reset operations, shell chaining, `curl | sh`/`wget | sh`, secret-environment enumeration, SSH-key reads, credential-bearing headers, and endpoint-protection tampering attempts.
- **Argument-form bypasses:** policy checks must normalize executable basenames, flag ordering, aliases, shell wrappers, and equivalent `--force` forms rather than rely on exact command strings.
- **Sandbox filesystem boundary:** an executed command must not write outside the repository or its isolated temporary filesystem.
- **Sandbox network boundary:** commands without an explicit network grant must be unable to open outbound sockets or resolve external hosts.
- **Sandbox environment boundary:** undeclared credentials and host environment variables must not be visible to child processes.
- **Worker worktree races:** concurrent workers editing the same logical area must produce a deterministic conflict, preserve both worker commits, and leave the coordinator repository clean after abort.
- **Worker cleanup races:** interrupted or concurrently completed workers must not delete another worker's active worktree or branch.
- **Patch transaction integrity:** stale ranges, overlapping hunks, symlink targets, partial multi-file failures, and formatter failures must leave the repository byte-identical or fully rolled back.
- **Protected verification contract:** autonomous runs must not alter tests, fixtures, snapshots, or verification scripts unless the user explicitly requested that exact modification.
- **Secret exfiltration:** shell, MCP, hooks, logs, artifacts, and model-visible tool output must redact or deny known credentials and token-like values.

Each case must have a stable test name, run as an independently visible step in the dedicated adversarial CI job, and produce evidence that identifies the policy decision or rollback result.

## Delivery sequence

### PR 1 — Baseline and repository map

- Record source-file line counts, public exports, release-gate status, and benchmark commands.
- Add a machine-checkable source-file ceiling.
- Freeze the public API surface for the first extraction.
- Update the architecture map and README.

### PR 2 — `medusa-agent` policy and tool boundaries

- Extract path containment and runtime policy first.
- Extract filesystem, shell, Git, and intelligence tools behind the existing dispatch API.
- Preserve all named adversarial tests and tool JSON schemas.

### PR 3 — `medusa-agent` sessions and engine

- Extract session persistence, evidence, verification, and orchestration state machines.
- Keep serialized formats and resume behavior byte-compatible.

### PR 4 — `medusa-intelligence`

- Extract indexing, patch transactions, formatting, language adapters, and impact analysis.
- Preserve transaction rollback semantics and formatter failure behavior.

### PR 5 — `medusa-memory`

- Extract schema, proposals, persistence, indexing, retrieval, and lifecycle modules.
- Preserve canonical Markdown output and rebuildable-index behavior.

### PR 6 — `medusa-extensions` and browser boundary

- Extract skills, hooks, MCP, browser evidence, and redaction.
- Keep poisoning defenses and credential isolation explicit.

### PR 7 — `medusa-hardening`

- Extract migrations, observability, release validation, archive safety, and chaos fixtures.
- Re-run clean-install, upgrade, rollback, and corrupted-state recovery scenarios.

### PR 8 — Cleanup and coverage expansion

- Remove obsolete compatibility shims.
- Raise coverage with behavior-focused tests for CLI, TUI, provider, extensions, agent, and hardening paths.
- Reconcile the final architecture document and repository map with the implemented module tree.

## Acceptance criteria

- No production Rust source file exceeds 800 lines without a documented exception.
- Most modules remain below 400 lines.
- Circular module dependencies are absent.
- Public API changes are documented and migration-tested.
- Workspace line coverage never falls below 75% during extraction.
- Coverage reaches 90% before the modularization program is declared complete.
- Every named adversarial regression passes independently of the coverage percentage.
- All deterministic, live-provider, browser, security, migration, chaos, packaging, and cross-platform gates remain green.
- Performance does not regress by more than 5% on the frozen benchmark suite without an explicit rationale.
- The final architecture document and repository map match the actual module layout.

## Immediate next action

Begin with PR 1: establish the measurable baseline, add the source-file ceiling check, and freeze the current public API before moving code. The first code-moving PR should then target `medusa-agent`, because it has the broadest responsibility set and the highest future change frequency.
