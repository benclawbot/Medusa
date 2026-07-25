# Rust AST-aware structured edits

Medusa can now plan Rust edits against parsed syntax nodes rather than fragile text searches.

## Workflow

1. Parse a Rust source file with `RustStructuredEditPlanner`.
2. Resolve a target by syntax kind and semantic name, or by a document-local node ID.
3. Add one or more `RustAstEdit` operations.
4. Call `finish()` to generate a language-neutral `StructuredEditPlan`.
5. Supply file snapshots containing `rust_snapshot_ast_nodes(document)` and apply the plan through `apply_structured_transaction`.

Every generated edit contains:

- the expected file hash;
- the expected original content;
- an expected symbol when the target has a name;
- a stable AST identity consisting of syntax kind and byte range;
- intent and provenance metadata for review and audit.

`finish()` applies all planned edits to staged text and reparses it. Malformed Rust is rejected before the working tree is mutated.

## Supported operations

- replace or delete a syntax node;
- insert content before or after a syntax node;
- replace a function body;
- replace a function signature;
- change item visibility;
- add or remove imports;
- add module declarations.

Import and module additions are idempotent. Missing or ambiguous targets return typed errors rather than falling back to text matching.
