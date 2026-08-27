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

The preserved crates are deliberately not all wired into this path:

- `medusa-markdown-memory` is an older in-memory chunk index and is superseded for canonical memory
  by `medusa-memory`.
- `medusa-turn-assembly` overlaps the live request assembly and prompt-cache provenance owners;
  only a demonstrated unique budget/provenance behavior should be migrated from it.
- `medusa-memory-consolidation` and `medusa-memory-writeback` do not understand the current
  refinement-authority lifecycle and must not write active memory independently.
- `medusa-context-retrieval` still lacks an authoritative `ContextLedger` projection in the
  shipped runtime; repository retrieval and turn budgeting currently own those boundaries.
- `medusa-mcp-cache` still needs a bridge to the actual `DesktopCommanderClient` schemas, tool
  policy, repository/session scope, and cacheability rules before it can safely cache results.
