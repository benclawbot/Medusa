# Contributor Workflow Authoring

CI workflows under `.github/workflows/` should reuse the composite actions under
`.github/actions/` instead of repeating the same setup blocks in every file.

## Available composite actions

| Action | Replaces |
|---|---|
| `./.github/actions/setup-rust` | `actions/checkout@v4` + `dtolnay/rust-toolchain@1.88.0` (+ `rustfmt, clippy` components) |
| `./.github/actions/cargo-locked` | Any `cargo <subcommand> --locked ...` step |
| `./.github/actions/cargo-test`   | `cargo test` invocations with optional `-p <crate>` filter |

## When to add a new composite action

If a pattern appears in 3+ workflows, extract it. If a workflow has unique
checkout semantics (custom ref, submodules, persist-credentials), keep the
inline `actions/checkout` and don't force it through `setup-rust`.

## When NOT to refactor

- Workflows that already use a composite action.
- Workflows that have a `permissions:` block with elevated scopes and a
  `with: ref:` checkout — these typically live in `ci.yml` and the
  `phase1-*.yml` family.
- `release-recovery.yml` (pure dispatch, no Rust).
- `snapshot-repository.yml` (uses `fetch-depth: 1` + a custom `tar`).

## Adding the action to a new workflow

```yaml
- name: Setup Rust toolchain
  uses: ./.github/actions/setup-rust

- name: Validate committed dependency authority
  uses: ./.github/actions/cargo-locked
  with:
    command: "metadata --locked --format-version 1 >/dev/null"

- name: Targeted tests
  uses: ./.github/actions/cargo-test
  with:
    crate: medusa-runtime
    test_args: "--lib"
```
