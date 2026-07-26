# Rust intelligence implementation comparison

Issue: #311

## Canonical direction

`ast.rs` and `symbol_table.rs` remain the production-facing AST and symbol APIs while the alternate implementations are compiled under tests during consolidation. This prevents unreachable code from silently rotting and gives the migration work executable characterization coverage without exposing two competing public authorities.

## Capability matrix

| Capability | Active implementation | Alternate implementation | Disposition |
|---|---|---|---|
| Per-file named AST nodes | `ast.rs` retains all nodes, parent/children, diagnostics | `rust_ast.rs` retains named nodes, field names, semantic names, error flags | Partial overlap; preserve active document API and migrate semantic names/field metadata where useful |
| Malformed-source handling | Explicit `ParseDiagnostic` collection | Per-node `has_error`/`is_missing` and file `has_errors` | Complementary; characterize both before consolidation |
| Repository-wide AST index | Built indirectly by `CodeIndex` | `RustAstIndex` supports deterministic build and snapshot-delta refresh | Unique; candidate for integration behind the canonical indexing path |
| Incremental invalidation | `IndexRefresh` and snapshots in active index path | `RustAstIndex::refresh` consumes `SnapshotDelta` | Partial overlap; consolidate around one invalidation authority |
| Symbol extraction | `symbol_table.rs` extracts file-local definitions/scopes | `rust_symbols_v2.rs` indexes repository-wide modules, impls, methods, bindings | Alternate has broader coverage; preserve with parity tests |
| Qualified/simple lookup | `find_qualified`, `find_simple` | `qualified`, `named`, `in_file`, `in_scope` | Partial overlap; normalize naming in canonical API |
| Scope visibility resolution | `resolve_in_scope` walks numeric parent scopes | `resolve_visible` uses stable scope identifiers | Alternate behavior is potentially superior; characterize shadowing and visibility first |
| Stable serialization identity | Active symbols use hashed IDs | Alternate symbols and scopes use deterministic hashed identities | Both support stable identity; add parity tests before choosing representation |
| Traits, impls, methods, locals | Active support is narrower | Explicit trait, impl, method, parameter, local and shadowing support | Unique/superior alternate behavior; must be migrated, not deleted |
| Module/import resolution | Separate active `resolution.rs` and `module_graph.rs` | Alternate symbol table derives file module paths but is not a full resolver | Keep resolution authority in active modules; use alternate metadata only where non-duplicative |

## First characterization gate

The alternate modules are now included under `cfg(test)`. Their existing tests therefore compile and run as part of the crate test target, covering AST ranges, malformed Rust, incremental refresh, modules, impls, trait methods, locals, shadowing, lookup dimensions, serialization identity, and stable symbol IDs.

## Next consolidation steps

1. Add parity fixtures that build both implementations from the same Rust source.
2. Move semantic node names and field metadata into the canonical AST representation.
3. Integrate repository-wide incremental AST refresh through the existing index lifecycle.
4. Extend the canonical symbol table with method, parameter, local, trait and impl coverage.
5. Remove alternate modules only after every row above has executable parity proof.
