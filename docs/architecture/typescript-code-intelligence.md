# TypeScript/JavaScript code-intelligence architecture

This record extends the Architecture v2 living index for the TypeScript/JavaScript adapter. Production code and executable tests remain authoritative.

## Ownership

| Concern | Primary owner | Reconstructable projection |
|---|---|---|
| Capability truth and tool registration | `medusa-capabilities` | `semantic_capabilities`, CLI and documentation projections |
| Workspace discovery and freshness evidence | `medusa-intelligence::typescript_workspace` | serialized `TypeScriptWorkspace` returned by production tools |
| Language-server process and protocol normalization | `medusa-intelligence::lsp*` | normalized definitions, references, diagnostics, symbols, and workspace edits |
| Production dispatcher and repository permissions | `medusa-agent::tools::intelligence` | model-visible `typescript_semantic` and `symbol_rename` receipts |
| Mutation authority | `medusa-intelligence::PatchTransaction` | formatting and impacted-test receipts |

No language server, UI, model session, or filesystem watcher owns durable semantic state. The repository contents and configuration files are authoritative; every LSP process is disposable and restartable.

## Data flow

1. The production handler confines the requested path with `safe_path`.
2. `discover_typescript_workspace` selects the nearest `tsconfig.json` or `jsconfig.json`, then the nearest package root, without escaping the repository.
3. Discovery deterministically enumerates supported source files while excluding dependencies, build outputs, generated/vendor trees, declaration bundles, and minified JavaScript.
4. The workspace record binds the canonical repository identity, selected workspace, configuration and package manifests, sorted relative source paths, and source bytes into SHA-256 fingerprints.
5. A disposable `typescript-language-server --stdio` process is started for that workspace.
6. Read-only requests normalize definitions, references, diagnostics, or workspace symbols back to repository-relative paths and ranges.
7. Rename independently discovers references, validates the proposed workspace edit, binds exact file snapshots, prepares the guarded transaction, and commits only if expected bytes still match.
8. The response includes workspace fingerprints plus normalized semantic or mutation evidence.

## Freshness and repository switching

`repository_fingerprint` identifies the canonical repository root. `workspace_fingerprint` includes that repository identity, the selected workspace path, package/configuration bytes, and every supported source path and content digest in deterministic order.

A workspace is fresh only when rediscovery produces both fingerprints unchanged. Source or configuration edits invalidate freshness. Identical repositories at different canonical roots are treated as a repository switch. Changes under ignored or generated paths do not alter the semantic workspace fingerprint because those paths are outside adapter coverage.

These fingerprints are evidence, not a cache. They do not authorize mutation by themselves and cannot replace revision-bound file snapshots in the guarded rename transaction.

## Trust boundaries

- Repository confinement occurs before workspace discovery and before every returned or edited path is consumed.
- Symlinks are not followed during source enumeration.
- Generated, vendor, dependency, build, coverage, and framework-output trees are outside semantic coverage.
- The language-server response is untrusted input until paths, ranges, capability support, ambiguity, static-reference agreement, and exact file snapshots pass validation.
- Resource operations and confirmation-requiring workspace edits are refused.
- The language server cannot widen tool permissions, write files directly, or become a mutation authority.
- Capability claims remain limited to behavior proven through the registered production entrypoint and certification gates.

## Monorepos and large workspaces

The nearest configuration/package root defines the LSP workspace for a target, allowing independent packages in one repository. Discovery is capped at 20,000 supported source files and fails closed above that limit. Tests cover nearest-root selection, generated and ignored paths, deterministic repository switching, source/configuration invalidation, and a 512-source fixture.

The executable benchmark covers 100, 1,000, and 5,000 source files:

```bash
cargo bench -p medusa-intelligence --bench typescript_workspace
```

It verifies source counts and fingerprint determinism on every iteration and emits machine-readable timing lines.

## Extension procedure

A new production language adapter must:

1. define exact typed capability levels and truthful unavailable/partial states;
2. identify one lifecycle and workspace-discovery owner;
3. confine all paths before process startup or result consumption;
4. define deterministic coverage, ignored/generated behavior, size limits, and freshness evidence;
5. normalize adapter output into repository-relative typed structures;
6. keep the adapter process and caches reconstructable rather than authoritative;
7. route mutations through the shared review, verification, snapshot, and transaction authorities;
8. add correctness, ambiguity, stale-state, repository-switching, monorepo, generated/ignored-path, large-workspace, cross-platform, and performance evidence;
9. update the capability registry and this architecture record only after the production entrypoint is certified.
