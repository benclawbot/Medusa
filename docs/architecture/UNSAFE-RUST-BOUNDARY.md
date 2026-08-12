# Windows containment unsafe Rust boundary

## Purpose

Medusa's Windows containment implementation must call native Win32 and composable-sandbox APIs. This document defines the narrow exception to the workspace-wide unsafe Rust prohibition and the evidence required to keep that exception auditable.

## Lint hierarchy

- The workspace declares `[workspace.lints.rust] unsafe_code = "forbid"`.
- Every ordinary workspace crate inherits that lint or explicitly uses `deny`/`forbid`.
- `medusa-process-containment` explicitly declares `[lints.rust] unsafe_code = "deny"`.
- Only exact module declarations in the containment crate root may use `#[allow(unsafe_code)]`.
- A crate-wide `#![allow(unsafe_code)]` or a file-local exception inside an implementation module is prohibited.

This hierarchy keeps safe modules in the containment crate compile-time protected while allowing reviewed native calls only where they are required.

## Reviewed modules

The machine-readable authority is [`unsafe-rust-policy.json`](unsafe-rust-policy.json). Its exact source inventory is checked against the repository.

| Module | Native boundary | Ownership obligations |
|---|---|---|
| `base_container` | Composable sandbox process creation, dynamic system-library lookup, pipes, environment blocks, process handles, and Job Objects | Bound every pointer and buffer lifetime, own every returned handle, fail closed when the sandbox API is unavailable, and preserve repository/network restrictions. |
| `windows` | Job Object creation/configuration, thread snapshots and resume, process liveness, termination, and handle closing | Assign a suspended process before execution, close each owned handle exactly once, and keep `Send`/`Sync` justification limited to the owned Job Object wrapper. |
| `windows_acl` | Process token lookup, SIDs, ACL/security descriptors, and LocalAlloc-owned results | Validate every returned structure, preserve allocation lifetimes, close/free resources exactly once, and reject inherited or unexpected access entries. |

`lib.rs` and `flatbuffer_builder.rs` are classified safe and cannot receive unsafe-code exceptions.

## Trust boundary

The safe runtime passes validated paths, arguments, limits, and policy inputs into `medusa-process-containment`. Unsafe modules translate those owned Rust values into native calls. Native pointers, handles, structures, and allocations do not escape as unowned public API state. The crate returns safe Rust results or errors and fails closed rather than choosing an unsandboxed fallback.

The unsafe exception does not widen repository, network, credential, or process authority. Those permissions remain defined by the containment policy and the higher-level command authorization boundary.

## Enforcement

`python scripts/check-unsafe-boundary.py` verifies:

1. the workspace `forbid` lint and every member crate's lint coverage;
2. the containment crate's explicit `deny` lint;
3. the exact source-file inventory and safe/unsafe classification;
4. local exceptions only for reviewed unsafe modules;
5. unsafe blocks, functions, extern declarations, implementations, and traits only in allowlisted files;
6. no stale allowlist entry, moved file, new unclassified file, or implementation-local lint exception.

`python scripts/test-unsafe-boundary.py` tests adversarial policy drift. `.github/workflows/unsafe-rust-boundary.yml` runs the policy tests, repository audit, and containment compilation on Linux, macOS, and Windows for every pull request and release path.

## Review procedure

A change touching an unsafe module, the policy file, checker, workflow, or module exceptions must:

1. explain the native invariant and why safe Rust cannot express the operation;
2. retain or add a nearby `SAFETY:` rationale for every operation or logical group;
3. update the source inventory and reason when files or responsibilities change;
4. include failure-path and resource-lifetime tests where behavior changes;
5. pass the authoritative Windows containment matrix and all unsafe-boundary jobs;
6. receive the required CODEOWNERS review before merge.

Adding a new unsafe file or module is a security-architecture change, not an ordinary refactor. It must be explicitly reviewed and cannot be enabled by broadening an existing exception.
