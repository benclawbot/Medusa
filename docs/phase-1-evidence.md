# Phase 1 Exact Evidence

## Exit criterion

The single-agent engine must complete a fixture bug fix after restart and produce exact verification evidence.

## Deterministic fixture

The test creates:

- `value.txt` containing the incorrect value `41`;
- `verify.sh`, which requires `value.txt` to equal `42` and prints `verified-value-42`.

The first engine instance receives a strict `fs_read` tool call and persists the session. A second engine instance loads the checksummed session from disk, receives a strict `fs_write` tool call changing the value to `42`, and then ends its turn. Medusa infers and runs `sh verify.sh`.

## Assertions

The regression test requires all of the following:

```text
value.txt == "42\n"
session.completed == true
verification stdout contains "verified-value-42"
verification evidence contains "exit_status=exit status: 0"
```

The test is named:

```text
medusa_agent::tests::fixture_bug_fix_survives_restart_with_exact_evidence
```

It runs as part of:

```text
cargo test --workspace --all-features
```

## Integrity and safety checks

The same gate also verifies:

- event-envelope checksums and previous-hash continuity on reload;
- atomic session and file replacement;
- rejection of parent-directory path traversal;
- hard denial of destructive shell programs;
- omission of provider thinking blocks from persisted responses;
- retry classification for rate limiting;
- least-privilege check-only CI on the final branch.
