# Session Recall Wiring

Medusa now composes successful same-repository session recall into the existing repository retrieval context before a model turn.

The integration reuses the canonical `.medusa/session-recall.sqlite3` store and existing prompt-budgeted repository context path. It does not introduce a second memory store or skill lifecycle.

Recall is bounded to three successful sessions, filtered by repository fingerprint, and each excerpt is capped before injection. The model is instructed to treat recalled material as evidence and re-check the current repository before acting.

The retrieval path remains best-effort: an unavailable or empty recall index never blocks repository inspection or model execution.
