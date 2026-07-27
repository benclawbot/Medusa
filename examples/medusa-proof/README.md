# Medusa reproducible safety and recovery proof

Run from a clean Linux checkout:

```bash
cargo medusa-proof --output medusa-proof-artifacts
```

The command delegates every guarantee to the authoritative `cargo product-acceptance` contract. It does not use a private model credential or a separate mock runtime. The small repository in `reference-repository/` documents the bounded coding-task shape used by the deterministic runtime fixture.

Outputs:

- terminal-friendly Plan → Execute Safely → Recover progress
- `medusa-proof-artifacts/medusa-proof.json`
- the source acceptance `summary.json`
- one captured log per authoritative scenario

The public proof currently requires Linux because the shipped Bubblewrap backend is the production evidence for repository-bounded writes, denied external filesystem access, denied network access, and process containment. macOS and Windows continue to run their supported product acceptance contracts in CI; the proof command refuses to over-claim unsupported evidence.
