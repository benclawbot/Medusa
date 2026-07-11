# Medusa Next Steps

## Post-1.0 Modularization Refactor

The first post-release engineering priority is to split the largest source files into smaller, cohesive modules without changing externally observable behavior.

### Why this matters

Several implementation files currently combine protocol definitions, validation, persistence, execution, policy, and test fixtures. This made rapid end-to-end implementation possible, but it increases review cost, merge-conflict risk, compile-time coupling, and the chance that future changes accidentally cross subsystem boundaries.

### Primary targets

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

### Refactoring rules

- Preserve public APIs unless a separate migration note is approved.
- Move code in behavior-preserving commits before redesigning it.
- Keep each production module focused on one responsibility.
- Prefer private modules and narrow re-exports over broad public surfaces.
- Keep test helpers in `tests/`, `testkit`, or `#[cfg(test)]` modules rather than production paths.
- Do not weaken sandbox, path-containment, memory-validation, or rollback controls during extraction.
- Maintain the 90% minimum line-coverage gate throughout the refactor.

### Execution sequence

1. Record baseline API, binary, coverage, performance, and fixture results.
2. Extract pure data types and validators first.
3. Extract persistence and protocol code.
4. Extract tool implementations and sandbox policy.
5. Extract orchestration state machines last.
6. Replace cross-module concrete dependencies with narrow traits only where they reduce coupling.
7. Run the complete release gate after every target crate.
8. Remove obsolete compatibility shims after all downstream callers migrate.

### Acceptance criteria

- No production Rust source file exceeds 800 lines without a documented exception.
- Most modules remain below 400 lines.
- Circular module dependencies are absent.
- Public API changes are documented and migration-tested.
- Line coverage remains at or above 90%.
- All deterministic, live-provider, browser, security, migration, chaos, packaging, and cross-platform gates remain green.
- Performance does not regress by more than 5% on the frozen benchmark suite without an explicit rationale.
- The final architecture document and repository map match the actual module layout.

### Suggested delivery

Deliver this as a sequence of small pull requests, one crate at a time, beginning with `medusa-agent`, because it currently has the broadest responsibility set and the highest future change frequency.
