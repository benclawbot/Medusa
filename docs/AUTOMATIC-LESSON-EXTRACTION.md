# Automatic lesson extraction

Medusa evaluates every completed session for reusable procedural knowledge. The extractor runs after durable session persistence and writes reviewable JSON proposals under:

```text
.medusa/learning/proposals/<session-id>.json
```

The extractor never writes directly to canonical memory. A generated lesson remains in `proposed` state until a later review or approval workflow promotes it.

## Eligibility

A session is eligible only when all of the following are true:

- the session completed successfully
- verification evidence exists
- the task was non-trivial, based on turns, tool observations, failures, or multiple evidence items
- at least one safe procedure item and one safe evidence item can be retained

Incomplete, cancelled, unverified, and trivial sessions do not generate proposals.

## Extracted information

Each proposal records:

- source session identifier
- repository fingerprint
- creation timestamp
- lesson classification
- concise title and summary
- reusable procedure steps
- verification and rejected-approach evidence
- observed tools
- bounded confidence score
- `proposed` lifecycle status

The current classifications are command, debugging, repository convention, verification, platform fix, and recovery.

## Safety and bounds

Extraction is deterministic and local. It does not make another provider request. Secret-like strings containing common credential markers are excluded. Procedure and evidence text is normalized, length-bounded, deduplicated, and capped to prevent uncontrolled proposal growth.

Proposal creation is best-effort. A lesson extraction failure never prevents the canonical session record from being persisted.

## Relationship to later roadmap work

This feature only creates proposals. Agent-created skills, automatic refinement, provenance scoring, lazy skill loading, automatic prior-session retrieval, learning review UI, and closed-loop metrics are separate roadmap items and remain separate pull requests.
