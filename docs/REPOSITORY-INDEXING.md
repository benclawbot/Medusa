# Repository indexing

Medusa's repository intelligence starts with a deterministic snapshot of indexable text files. The snapshot is shared by syntax indexing and future ranked retrieval so discovery rules do not drift between subsystems.

## Discovery contract

`RepositorySnapshot::scan`:

- walks the repository in stable relative-path order;
- includes common source, configuration, documentation, and web-language extensions;
- excludes `.git`, `.medusa`, generated outputs, dependency trees, caches, virtual environments, build directories, and vendor directories;
- rejects files containing NUL bytes in the first 8 KiB as binary content;
- records each eligible file's byte length and SHA-256 fingerprint.

The Rust `CodeIndex` consumes the snapshot's `.rs` paths, preserving existing syntax-aware symbol and reference extraction while removing its independent full-repository discovery path.

## Incremental invalidation

`RepositorySnapshot::changes_since` returns stable, sorted `added`, `modified`, and `removed` path sets. A file is modified only when its fingerprint changes. This provides the invalidation boundary for subsequent incremental symbol indexing and ranked retrieval work.

Repository switches and branch-changing Git operations should replace the baseline snapshot. File edits can rescan and apply only the reported path changes once per-path index fragments are introduced.

## Current scope

This first #135 implementation slice establishes deterministic discovery and invalidation metadata. Follow-up work will add:

- per-file symbol fragments and incremental index updates;
- language-aware extraction beyond Rust;
- filename, symbol, reference, and content ranking;
- explicit retrieval budgets with exclusion and truncation explanations;
- performance and memory benchmarks plus TUI/Desktop visibility.
