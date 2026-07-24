# Repository index scale evidence

Medusa keeps a deterministic scale regression in `crates/medusa-intelligence/tests/index_scale.rs`.

The fixture creates 1,000 indexed Rust source files and 200 additional files under generated and vendor trees that must be ignored. It verifies:

- a complete index build finishes within 60 seconds;
- an incremental refresh of 10 modified files finishes within 10 seconds;
- the estimated owned heap for symbols, references, paths, and parse errors remains below 64 MiB;
- incremental refresh produces the same index as a clean rebuild;
- generated, vendor, build, dependency, environment, and metadata directories do not enter the index.

The ceilings are intentionally broad enough for GitHub-hosted Windows, macOS, and Linux runners. They are regression limits rather than microbenchmark claims. Tight performance comparisons should be run on pinned hardware, while these tests protect production behavior from accidental full rescans, unbounded index growth, and ignore-policy regressions.
