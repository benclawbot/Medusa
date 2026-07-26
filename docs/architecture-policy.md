# Architecture policy

Medusa treats a production capability as integrated only when its crate is reachable from a shipped root through ordinary Cargo dependency edges, exercised by tests, and observable through a user-facing or operational interface.

## Shipped roots

The reviewed root set lives in `.github/architecture-policy.json`. It currently contains `medusa-cli`, `medusa-daemon`, `medusa-tui`, `medusa-runtime`, `medusa-agent`, and `medusa-browserd`.

## Integration baseline

The workspace integration epic is complete only when every production crate is reachable from a shipped root through normal or build dependency edges and `medusa-testkit` is reachable through dev edges only. The policy therefore carries no crate-reachability exemptions: a newly orphaned crate is a blocking CI failure.

## Adding a crate

1. Add the crate to the Cargo workspace.
2. Connect it to a shipped root with a normal or build dependency. Test-support crates must be reachable through a dev dependency.
3. Add integration proof that exercises the capability through the shipped surface.
4. Run:

```bash
cargo generate-lockfile
python3 scripts/architecture-policy.py self-test
python3 scripts/architecture-policy.py check \
  --root . \
  --policy .github/architecture-policy.json \
  --report architecture-policy-report.md
```

A newly orphaned crate fails CI.

## Temporary exemptions

Exemptions are migration tools, not proof of integration. Every entry requires an owner, a concrete reason, and an ISO expiry date. Expired exemptions fail CI. Remove an exemption in the same PR that establishes the real dependency path.

The clean baseline intentionally contains no crate exemptions. Any future exemption must be narrowly scoped, reviewed, and paired with a tracked removal plan.

## Hidden dependencies

Cross-crate source copying is forbidden. Do not use `include!`, build-script reads, or generated concatenation to import another crate's `src` tree. Add a normal Cargo dependency and expose an explicit API instead.

Rust source files under a crate's `src` directory must be reachable from `lib.rs` or `main.rs` through the module tree. Reviewed legacy exceptions are listed explicitly in the policy file and must be removed when their audit issue is resolved.

## CI evidence

The architecture workflow uploads `architecture-policy-report.md`, including each workspace crate's reachability classification and every policy violation.
