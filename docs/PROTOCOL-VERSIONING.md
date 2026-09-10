# Compatibility and Versioning

Medusa independently versions the workspace packages, wire protocol, and configuration schema.

## Protocol

- Major mismatches are incompatible.
- A consumer accepts the same major and a peer minor version less than or equal to its own.
- New optional fields require a minor increment and defaults.
- Removing or changing required fields requires a major increment.
- Unknown fields are rejected on integrity-sensitive envelopes.

### Channel rules

- Frontend command/event channel (`medusa-protocol`): same-major,
  older-or-equal-minor acceptance via `ProtocolVersion::accepts`.
- Daemon job channel (`medusa-daemon`, flat `u16` `DAEMON_PROTOCOL_VERSION`):
  exact match via `job_protocol_compatible`. The flat version has no minor
  component to negotiate, so any bump is incompatible. This deliberate split
  is enforced by tests on both sides.

### Integrity-sensitive (major-only) envelopes

The following types use `deny_unknown_fields` and are pinned as major-only:
`ProtocolVersion`, `SessionAction`, `EventEnvelope`,
`FrontendCommandEnvelope`, `FrontendEventEnvelope`. Any shape change to these
requires a major protocol bump. Additive minor-bump surfaces must NOT deny
unknown fields, so older consumers keep accepting newer-minor payloads.

## Fingerprints

- Canonical content fingerprints are lowercase 64-character SHA-256 hex
  digests. `medusa-execution-checkpoint` (`CheckpointError::InvalidFingerprint`)
  and `medusa-recovery-coordinator` (`RecoveryError::InvalidFingerprint`)
  reject anything else at verification time.
- `medusa-session-continuity` trajectory fingerprints are opaque non-empty
  correlation tags, validated for presence at intake
  (`ContinuityError::InvalidFingerprint`); they are NOT required to be
  SHA-256. Convert to the canonical form before handing a fingerprint to
  checkpoint, coordinator, or recovery verification.

## Configuration

- Version `1` is the initial schema.
- Unknown fields are rejected to prevent misspelled safety settings.
- Migrations must be explicit, tested, and reversible.
- Precedence is CLI, environment, project, user, then built-in defaults.
