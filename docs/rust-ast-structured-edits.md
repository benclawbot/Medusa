# Rust AST-aware structured edits

Medusa can plan Rust edits against parsed syntax nodes rather than fragile text searches.

## Workflow

1. Parse a Rust source file with `RustStructuredEditPlanner`.
2. Resolve a target by syntax kind and semantic name, or by a document-local node ID.
3. Add one or more `RustAstEdit` operations.
4. Call `finish()` to generate a language-neutral `StructuredEditPlan`.
5. Supply file snapshots containing `rust_snapshot_ast_nodes(document)` and apply the plan through `apply_structured_transaction`.

Every generated edit contains the expected file hash and original content, symbol and AST-node preconditions, and reviewable intent/provenance metadata. `finish()` applies all planned edits to staged text and reparses it, so malformed Rust is rejected before the working tree is mutated.

Supported operations include replacing, deleting, and inserting around syntax nodes; changing function bodies, signatures, and visibility; and idempotently adding or removing imports and module declarations.
