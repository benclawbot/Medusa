# Canonical turn and memory pipeline

`medusa-runtime` owns the shipped turn lifecycle. The current production path is:

1. accept the user prompt and build the runtime policy context;
2. select active, high-confidence project memory through `medusa-memory`;
3. assemble the request through `medusa-agent`, its context budget, and provider prompt-cache provenance;
4. retrieve repository evidence through the live repository index;
5. execute, verify, and persist the turn;
6. record canonical-memory reuse only after a verified terminal outcome.

Project memory is bounded before injection and labeled advisory. Retrieval or reuse failures fail
closed and surface a truthful runtime notice. `medusa-memory` remains the Markdown authority: its
frontmatter carries scope, lifecycle, validation, expiry, provenance, and supersession metadata;
the SQLite index is disposable.

Completed-session learning remains a separate approval-controlled path. It is admitted to the
refinement authority as a probationary candidate and must not be treated as active project memory
until the existing review/graduation lifecycle activates it.

The runtime intentionally has one implementation for each stage. The former experimental
`medusa-markdown-memory`, `medusa-turn-assembly`, `medusa-memory-consolidation`,
`medusa-memory-writeback`, `medusa-context-retrieval`, and `medusa-mcp-cache` crates were removed:
none was reachable from a shipped binary, and each overlapped an authority named above. New work
must extend the canonical owner instead of adding a parallel pipeline.
