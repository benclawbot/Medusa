# ADR 0012: Audit reports and bounded daemon job projections

- Status: accepted
- Owners: `medusa-cli` and `medusa-daemon`
- Scope: projections of existing durable session and job authorities

## Context

CLI audit export treated option values as session IDs, accepted filesystem paths
in place of IDs, redacted each argument string independently, and could attribute
a later write to a denied request. Daemon `List` included complete output for every
historical job, so seventeen jobs with 1 MiB on each stream exceeded the client's
32 MiB response cap and degraded TUI monitoring despite a responsive daemon.

## Decision

The report parser accepts options in standard CLI order and validates the canonical
`ses-ULID` contract before path construction. Credential redaction spans adjacent
argument-array entries and consumes an entire credential value even when it has
spaces. Mutation projection tracks denied requests and durable approval decisions;
an unrelated successful request cannot make a denied or pending path appear changed.

Daemon `List` is a bounded metadata projection: at most 128 records, active jobs
first, then newest creation timestamp and descending ID for ties. Captured stdout
and stderr are empty in summaries. `Status { job_id }` remains the detail route for
full output and older jobs. The server never deletes or modifies durable history
when serving a list.

## Compatibility and recovery

No session-event schema, job-state format, dependency direction, execution authority,
or daemon wire shape changes. Existing clients can decode the same `Jobs`/`JobRecord`
response under protocol v2; consumers requiring output must use `Status`. Invalid
filesystem paths were never valid session identities. Rollback requires only the
previous binaries; persisted sessions and jobs remain readable.

The conformance evidence is in `report_command_coverage`, `list_summary_coverage`,
and the daemon server's active-job projection unit test. These exercise checksummed
events, denial followed by approval or unrelated success, multiword credentials,
malformed CLI input, cross-workspace traversal, output-heavy histories, deterministic
selection, and preservation of full per-job output.
