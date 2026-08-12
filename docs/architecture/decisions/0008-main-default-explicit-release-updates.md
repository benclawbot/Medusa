# ADR 0008: Main is the default updater and releases are explicit

- Status: accepted
- Date: 2026-08-12
- Supersedes: ADR 0002 default-channel decision
- Owners: release and security maintainers

## Context

ADR 0002 made verified prebuilt releases the default `medusa update` authority and kept the moving `main` source build behind `--channel source`. The signed release path remains the correct trust boundary for stable prebuilt artifacts, but that default couples ordinary updater use to release-manifest availability. A published release whose Ed25519 manifest authority has not yet been produced therefore blocks the default update command even when the user intends to follow current `main` development.

Medusa already retains a distinct `MainBranchUpdater` that resolves the moving `main` revision and builds it locally. These two update paths have different trust, dependency, and rollout models and should be selected directly in the command surface rather than through a generic channel string.

## Decision

`medusa update` follows the latest `main` revision and builds it locally.

`medusa update --release` selects the stable verified prebuilt release path. That path continues to require the Ed25519-signed release manifest, exact signed artifact metadata, platform matching, digest verification, confined extraction, rollout policy, and health-checked atomic replacement.

The public `--channel release|source` selector is removed. `--allow-downgrade` is valid only together with `--release`.

The two paths never fall back to one another:

- a source discovery or build failure does not install a release;
- a missing, invalid, unsigned, or otherwise ineligible release does not compile `main`.

`--check` and `--automatic` apply to whichever path the user explicitly selected.

## Consequences

- Developers and users tracking Medusa development can use `medusa update` without depending on release-signing cadence.
- Stable release consumers opt in explicitly with `medusa update --release` and retain the full verified-prebuilt trust model from ADR 0002.
- The default main path requires the supported Rust/Cargo toolchain because it compiles locally.
- An unsigned release can never weaken or redirect the main update path, and a source failure can never bypass release verification.
- Existing automation that used `--channel` must migrate to the direct command forms.

## Preserved decisions from ADR 0002

ADR 0002 remains authoritative for the verified-prebuilt release trust model itself: Ed25519 manifest authority, key lifecycle, artifact verification, package-manager behavior, rollout sequence, atomic replacement, startup acknowledgement, and rollback. ADR 0008 supersedes only its choice of the default CLI channel and the `--channel source` UX.

## Evidence

- `crates/medusa-cli/src/main.rs`
- `crates/medusa-cli/src/update_command.rs`
- `crates/medusa-update/src/source.rs`
- `docs/RELEASE.md`
- `docs/RELEASE-SIGNING.md`
- `docs/architecture/PREBUILT-UPDATES.md`
- `.github/workflows/verified-prebuilt-update.yml`
