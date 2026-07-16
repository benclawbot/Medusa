# Security Policy

Do not report vulnerabilities in public issues. Use GitHub private vulnerability reporting when enabled.

## Security authority

Medusa's security behavior is defined by the current implementation, tests, and CI guardrails in this repository—not by a historical specification document. The removed `MEDUSA_SPEC.md` is not an authority and must not be used to assess current guarantees.

The relevant implementation areas include:

- guarded tool execution and capability checks in `crates/medusa-agent`;
- filesystem, shell, browser, web, patch, and repository tool policies in the agent tool modules;
- secret redaction and bounded output handling in the output and evidence pipeline;
- session persistence, provenance, and verification controls in the agent and memory crates;
- dependency, unsafe-code, source-size, test, fuzz, and release guardrails under `.github/workflows`, `scripts`, and repository policy files.

## Reporting expectations

Include the affected version or commit, platform, reproduction steps, impact, and any relevant logs with secrets removed. Do not include live credentials, private repository contents, or personal data in a report.

## Guarantee boundaries

Security claims in the README or other documentation must correspond to behavior covered by current code and tests. Planned hardening is not an implemented guarantee. When documentation and implementation disagree, treat the implementation and passing security tests as authoritative and report the documentation mismatch.