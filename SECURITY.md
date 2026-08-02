# Security Policy

Do not report vulnerabilities in public issues. Use GitHub private vulnerability reporting when enabled.

## Security authority

Medusa's security behavior is defined by the current implementation, tests, and CI guardrails in this repository—not by a historical specification document. The removed `MEDUSA_SPEC.md` is not an authority and must not be used to assess current guarantees.

The relevant implementation areas include:

- guarded tool execution and capability checks in `crates/medusa-agent`;
- filesystem, shell, browser, web, patch, and repository tool policies in the agent tool modules;
- secret redaction and bounded output handling in the output and evidence pipeline;
- session persistence, provenance, and verification controls in the agent and memory crates;
- dependency, unsafe-code, test, fuzz, and release guardrails under `.github/workflows`, `scripts`, and repository policy files.

## Explicit unsafe Rust and native FFI boundary

The workspace default is `[workspace.lints.rust] unsafe_code = "forbid"`. The only exception is the Windows native-API boundary in `crates/medusa-process-containment`, which explicitly uses `[lints.rust] unsafe_code = "deny"` and permits unsafe Rust only through local module-declaration exceptions for:

- `base_container` — Windows composable-sandbox, process, pipe, environment, library-loader, and Job Object APIs;
- `windows` — Windows Job Object, process, thread, snapshot, and handle APIs;
- `windows_acl` — Windows token, SID, security-descriptor, ACL, and local-allocation APIs.

There is no crate-wide `allow(unsafe_code)`. Safe modules in the containment crate remain covered by `deny`, and every other workspace crate must inherit the workspace `forbid` lint or explicitly deny it.

The exact reviewed source inventory, classification, and rationale are authoritative in [`docs/architecture/unsafe-rust-policy.json`](docs/architecture/unsafe-rust-policy.json). `python scripts/check-unsafe-boundary.py` fails when unsafe syntax appears outside that allowlist, an allowlisted file is moved, a new containment source is unclassified, another crate loses lint coverage, or an exception is widened. `python scripts/test-unsafe-boundary.py` supplies adversarial fixtures. The `Unsafe Rust Boundary` workflow runs those checks and compiles the containment crate on Linux, macOS, and Windows for pull requests, releases, and `main`.

Changes to the policy, checker, workflow, containment source inventory, or unsafe modules require the CODEOWNERS security review. Every unsafe operation or logically grouped operation must retain a nearby `SAFETY:` rationale. See [`docs/architecture/UNSAFE-RUST-BOUNDARY.md`](docs/architecture/UNSAFE-RUST-BOUNDARY.md) for the trust boundary and change procedure.

## Reporting expectations

Include the affected version or commit, platform, reproduction steps, impact, and any relevant logs with secrets removed. Do not include live credentials, private repository contents, or personal data in a report.

## Guarantee boundaries

Security claims in the README or other documentation must correspond to behavior covered by current code and tests. Planned hardening is not an implemented guarantee. When documentation and implementation disagree, treat the implementation and passing security tests as authoritative and report the documentation mismatch.
