# Code-intelligence capability levels

`medusa-intelligence` and the v2 capability registry distinguish the availability of a production tool from the semantic depth of each language adapter. The production `semantic_capabilities` tool is the authoritative machine-readable report. It is generated from typed Rust profiles and is available anywhere the repository itself is accessible.

## Current production truth

| Language | Adapter | Production depth | Important limits |
| --- | --- | --- | --- |
| Rust | `tree-sitter-rust` static index | Parsed declarations plus repository-wide exact-name definition and reference queries | References are syntax-token occurrences, not type-directed resolution. Compiler diagnostics are not dispatched. |
| Python | bounded lexical scanner | `def`, `async def`, and `class` declarations plus comment- and string-aware identifier occurrences | This is not a Python parser. Diagnostics and rename are unavailable. |
| TypeScript/JavaScript | TypeScript compiler language service | Definitions, references, syntactic/semantic diagnostics, workspace symbols, and guarded cross-file rename | Requires Node.js and a discoverable TypeScript compiler module. Results are repository-scoped, bounded to 20,000 supported source files, and bound to adapter version, config, source hashes, and a content-derived workspace fingerprint. |

The registry owns the production `CodeIntelligence` capability. `code_index`, `semantic_capabilities`, `symbol_rename`, `typescript_semantic`, and `typescript_rename` are registered under it rather than under generic filesystem access. `patch_apply` remains a filesystem mutation because it does not depend on semantic analysis.

## Guarded Rust rename

The current production rename is deliberately narrower than a language-server rename. It proceeds only when all of the following hold:

- the repository index contains no Rust parse-error paths;
- exactly one indexed Rust definition has the requested name;
- every matching indexed occurrence is in Rust source;
- the replacement is a valid identifier;
- the durable patch transaction still observes the expected source bytes;
- every path remains inside the repository.

Ambiguous definitions, Python lexical matches, cross-language same-name occurrences, parse errors, stale bytes, overlapping edits, and repository escapes fail before mutation. This is reported as **partial guarded refactoring**, not full semantic rename.

## Ownership and extension procedure

- Capability schema and production readiness: `crates/medusa-capabilities/src/registry.rs`
- Typed language claims: `crates/medusa-intelligence/src/capabilities.rs`
- Static index and refresh authority: `crates/medusa-intelligence/src/index.rs` and `snapshot.rs`
- Guarded patch transaction: `crates/medusa-intelligence/src/patch.rs`
- Production agent handlers: `crates/medusa-agent/src/tools/intelligence.rs`
- TypeScript workspace, freshness, and process lifecycle: `crates/medusa-intelligence/src/typescript_workspace.rs` and `typescript_semantic.rs`
- TypeScript compiler-language-service adapter: `tools/typescript-semantic-adapter.mjs`
- Reusable LSP process and normalization primitives for future adapters: `crates/medusa-intelligence/src/lsp*.rs`

A language level may be promoted only after its real production entrypoint has a lifecycle owner, dependency discovery, permission mapping, fail-closed handler, freshness evidence, cross-platform fixtures, and capability-registry tests. Merely adding parser or LSP helper code does not change the production claim.


## TypeScript/JavaScript trust and cache authority

`discover_typescript_workspace` selects the nearest `tsconfig.json` or `jsconfig.json`, then the nearest package root, without crossing the repository boundary. Dependency, generated, vendor, declaration, minified, build-output, and repository-state paths are excluded. The adapter reparses the selected project for every production request; there is no hidden durable semantic cache. The returned content-derived workspace fingerprint is the freshness authority for follow-up operations and repository switching.

Read-only results include repository-relative paths, exact ranges, per-file source hashes, TypeScript adapter version, config path, package root, source count, and workspace fingerprint. `typescript_rename` requires an optional expected workspace fingerprint, rejects TypeScript rename refusals and ignored or out-of-scope locations, rechecks every file hash, and commits byte-exact edits through `PatchTransaction`. This keeps semantic planning read-only and places mutation under the existing review and verification transaction.
