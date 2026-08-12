# Typed provenance source coverage

Issue #820 routes improvement-relevant runtime signals through the schema-versioned provenance
graph in `medusa-improvement::provenance`. The adapter matches every `EventPayload` variant
exhaustively. A variant that is added to `medusa-protocol` without an explicit graph disposition
therefore fails compilation and cannot silently bypass provenance capture.

The graph currently persists typed observations for user instructions and session actions,
approvals, tool requests/denials/completions/timing, tool artifacts, verification start and result,
worker evidence, parent integration receipts, recovery, cancellation/reset, runtime and session
failure, terminal outcomes, provider execution, and artifact/checkpoint transactions. The remaining
protocol variants are intentionally excluded because they are descriptive context rather than
improvement evidence (session creation/state, goals, plans, assumptions, questions, assistant
transcripts, team snapshots, compaction, and turn-boundary markers); their source events remain in
the canonical session journal and may be added later with a typed adapter.

Every durable observation retains its event ID and sequence range, session/root/trajectory/attempt
lineage, worker and parent linkage, repository origin/common-directory identity, revision, privacy
decision, retention class, authority class, typed outcome, and a digest of the original payload.
Prompt/output text and hidden reasoning are redacted before persistence. JSONL storage is append-only,
deduplicated by stable observation ID, restart-safe, and quarantines malformed records rather than
turning parse failures into positive evidence.
