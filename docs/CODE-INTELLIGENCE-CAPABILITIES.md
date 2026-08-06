# Code-intelligence capability levels

`medusa-intelligence` and the v2 capability registry distinguish the availability of a production tool from the semantic depth of each language adapter. The production `semantic_capabilities` tool is the authoritative machine-readable report. It is generated from typed Rust profiles and is available anywhere the repository itself is accessible.

## Current production truth

| Language | Adapter | Production depth | Important limits |
| --- | --- | --- | --- |
| Rust | `tree-sitter-rust` static index | Parsed declarations plus repository-wide exact-name definition and reference queries | References are syntax-token occurrences, not type-directed resolution. Compiler diagnostics are not dispatched. |
| Python | bounded lexical scanner | `def`, `async def`, and `class` declarations plus comment- and string-aware identifier occurrences | This is not a Python parser. Diagnostics and rename are unavailable. |
| TypeScript/JavaScript | repository-scoped `typescript-language-server` dispatcher | Definitions, references, diagnostics, workspace symbols, and guarded cross-file rename through production entrypoints | No parser-backed parsed-symbol claim. LSP output is untrusted until repository scope, ambiguity, range, static-path, freshness, and exact-snapshot checks pass. |

The registry owns the production `CodeIntelligence` capability. `code_index`, `semantic_capabilities`, `typescript_semantic`, and `symbol_rename` are registered under it rather than under generic filesystem access. `patch_apply` remains a filesystem mutation because it does not depend on semantic analysis.

## Guarded rename paths

The Rust path remains deliberately narrower than a language-server rename. It requires a complete Rust parse, exactly one indexed definition, Rust-only occurrences, a valid replacement identifier, expected source bytes, and repository-confined paths.

The TypeScript/JavaScript path additionally requires:

- one exact workspace symbol and supported `prepareRename` behavior;
- independent reference discovery;
- agreement between normalized semantic edits and independently discovered paths;
- no resource operations, confirmation-requiring edits, scope escapes, empty replacements, or overlapping ranges;
- exact SHA-256 snapshots for every touched file;
- conversion of LSP UTF-16 ranges to byte-precise expected-content edits;
- a fresh deterministic workspace fingerprint immediately before transaction preparation;
- a guarded `PatchTransaction` commit before formatting and impacted-test evidence is returned.

Ambiguity, unsupported capability responses, parse/protocol errors, stale bytes, incomplete coverage, repository switching, workspace drift, and path escapes fail before mutation. The production claim is merge-gated by the final cross-platform `Code Intelligence Certification` suite and the repository’s exhaustive issue-closing gates.

## Workspace freshness

TypeScript workspace discovery chooses the nearest repository-confined configuration/package root and deterministically enumerates supported sources. Each returned workspace contains:

- `repository_fingerprint`, binding the canonical repository identity;
- `workspace_fingerprint`, binding that repository, selected workspace, package/configuration bytes, sorted source paths, and source content digests;
- an exact supported-source count.

Rediscovery must reproduce both fingerprints for the workspace to be fresh. Source/configuration changes invalidate the fingerprint; identical content at another repository root is a repository switch. Generated, vendor, dependency, build, coverage, declaration-bundle, and minified-output paths remain outside semantic coverage and do not affect the fingerprint.

## Ownership and extension procedure

- Capability schema and production readiness: `crates/medusa-capabilities/src/registry.rs`
- Typed language claims: `crates/medusa-intelligence/src/capabilities.rs`
- Static index and refresh authority: `crates/medusa-intelligence/src/index.rs` and `snapshot.rs`
- TypeScript workspace and freshness evidence: `crates/medusa-intelligence/src/typescript_workspace.rs`
- LSP process and normalization: `crates/medusa-intelligence/src/lsp*.rs`
- Guarded rename validation and snapshots: `crates/medusa-intelligence/src/guarded_rename.rs`
- Guarded patch transaction: `crates/medusa-intelligence/src/patch.rs`
- Production agent handlers: `crates/medusa-agent/src/tools/intelligence.rs`
- Living architecture record: `docs/architecture/typescript-code-intelligence.md`

A language level may be promoted only after its real production entrypoint has a lifecycle owner, dependency discovery, permission mapping, fail-closed handler, freshness evidence, cross-platform fixtures, benchmarks, and capability-registry tests. Merely adding parser or LSP helper code does not change the production claim.
