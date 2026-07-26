# Canonical turn and memory pipeline

`medusa-runtime` owns the shipped turn lifecycle in this order:

1. accept ordered retrieved context;
2. index and retrieve relevant Markdown memory;
3. resolve MCP resources through the scoped result cache;
4. compose the final prompt through `medusa-turn-assembly`;
5. execute and verify the turn;
6. plan and commit memory writeback only after verified success.

Turn provenance records included context identifiers, memory chunk and retrieval fingerprints, MCP cache-hit status, and stable/full prompt fingerprints. Failed or cancelled turns never commit durable memory.

MCP result-cache identity includes server, tool, canonical input, schema fingerprint, protocol version, and optional server version. Sensitive or explicitly non-cacheable responses are not stored, and expired entries are rejected.

The runtime composes turns and coordinates lifecycle state; indexing, cache policy, deterministic assembly, consolidation, and writeback planning remain owned by their dedicated crates. The shipped Cargo-selected roots compile these integrations directly and preserve the dedicated crates as the only policy authorities.
