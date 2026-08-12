# Skill provenance and confidence

Inactive skill proposals keep their learning history in `.medusa/learning/skill-proposals/<name>/manifest.json`.

Manifest schema version 3 records:

- the current effective confidence used for review;
- every contributing lesson and source session;
- the lesson kind, observed tools, and counts of accepted procedure and evidence items;
- a confidence observation for each proposal revision;
- the complete ordered provenance chain for later review or audit.

The effective confidence is monotonic: a later lower-confidence observation remains visible in `confidence_history`, but it cannot silently reduce the proposal's current confidence. Duplicate lesson IDs do not create revisions or duplicate provenance.

Schema-version-2 proposals migrate conservatively when next refined. Existing lesson and confidence values are retained, while unavailable historical detail is marked `legacy-unrecorded` instead of being inferred.

Provenance does not activate a skill. Proposals remain outside `.medusa/skills`, retain `requires_approval: true`, and must be explicitly reviewed before installation.
