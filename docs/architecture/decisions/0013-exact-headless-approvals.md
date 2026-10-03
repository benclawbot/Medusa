# ADR 0013: Exact headless command approval

- Status: accepted
- Owner: `medusa-cli`
- Scope: unattended shell-command approval admission

## Context

Headless approval joined executable and arguments into one string, discarded empty
arguments, and collapsed all whitespace. Different argument vectors therefore
matched the same allowlist entry. A newline inside Python code could move a write
out of a conditional while retaining the normalized allowlist match.

## Decision

Allowlist entries are parsed into executable-and-argument vectors using the
existing locked `shlex` package's POSIX quoting syntax. The frontend compares each
requested argument exactly, including empty values and internal whitespace.
Malformed quoting, dangling escapes, empty executable names, and NUL bytes fail
configuration loading. Displayed commands quote argument boundaries.

The CLI only submits an approve response for an exact match. Runtime approval
grants, exact-action fingerprints, expiry, active-plan checks, command policy,
containment, and execution remain owned by their existing authorities.

## Compatibility and recovery

Existing entries containing simple unquoted arguments continue to match. Entries
with spaces or literal backslashes inside an argument must quote that argument; quoted empty arguments
are retained. Quoting syntax is the same on Linux, macOS, and Windows, so Windows
paths with backslashes should use single quotes. Commands remain one per line,
and parsing does not perform shell expansion.

No journal, session, configuration schema, or daemon wire format changes. The CLI
adds a direct dependency on the already locked `shlex` version without changing
any package version. Rollback requires the previous binary, but restores the
known approval ambiguity; it is not a security-equivalent rollback.

## Evidence

Headless approval tests reproduce the old newline and argument-boundary failures,
then verify exact quoted matches, empty arguments, malformed input rejection,
unchanged simple commands, and canonical command-policy admission of the example
scripts. Local fixture execution confirms that the altered script writes a file
while its single-line counterpart does not.
