# Medusa documentation

Use this index to find the repository's main operator and maintainer references.

Every tracked Markdown document and its current or historical disposition is recorded in [`documentation-inventory.json`](documentation-inventory.json). See [documentation governance](DOCUMENTATION-GOVERNANCE.md) for the link, review, and drift policy.

## Using Medusa

- [TUI keyboard shortcuts](tui-keyboard-shortcuts.md) — interactive composer, transcript, run, and modal controls.
- [TUI troubleshooting](tui-troubleshooting.md) — recovery steps for redraw, cancellation, scrolling, clipboard, and restored drafts.
- [Compatibility](COMPATIBILITY.md) — supported platforms and compatibility expectations.

## Capabilities and releases

- [Capability evidence](CAPABILITY-EVIDENCE.md) — auditable mapping from shipped capabilities to implementation and validation gates.
- [Provider support authority](provider-support.json) and [rendered guide](PROVIDER-SUPPORT.md) — selectable provider, dogfood, credential, and Realtime status.
- [Release process](RELEASE.md) — versioning, validation, packaging, provenance, and draft-release workflow.

## Architecture and maintenance

- [Architecture living index](architecture/INDEX.md) — current ownership, state-machine, trust-boundary, and certification authority.
- [Public API governance](PUBLIC-API-BASELINE.md) — governed Rust compatibility surfaces and CI enforcement.
- [Observability](OBSERVABILITY.md) — operational health, support bundles, metrics, and redaction boundaries.

Documents carrying the `Historical record —` banner are retained only as implementation or decision evidence and are not current setup or status guidance.

The root [README](../README.md) remains the primary installation and quick-start guide.
