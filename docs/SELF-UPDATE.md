# Self-Update (`medusa update`)

`medusa update` upgrades the running CLI binary to the latest verified
revision on `main`. The flow has three phases:

## Phase 1 — Prebuilt artifact (fast path)

If CI has published a revision-bound prebuilt binary for the running
platform, `medusa update` downloads it, verifies its signed manifest, and
swaps it into place atomically. The prebuilt tarball is ~14 MB on Linux
and downloads in ~2 seconds on a typical connection. This is the happy
case and what production users see in the common path.

## Phase 2 — Wait for the prebuilt artifact (default fallback)

If CI has not yet finished publishing the prebuilt binary for the running
revision (typical: 1–5 minutes after a `main` commit), `medusa update`
polls the GitHub release endpoint every 15 seconds for up to **10 minutes**
before giving up. This matches the behavior of Codex CLI and Claude Code:
neither tool recompiles from source when the install method they manage
already produces a prebuilt binary, and neither takes 15 minutes to update.

Override the timeout with `--wait-for-prebuilt=<secs>` or the
`MEDUSA_UPDATE_PREBUILT_TIMEOUT_SECS` env var.

When the timeout elapses without the prebuilt artifact appearing, the
update fails with a clear error message pointing the user at:
- the `rolling-main-cli` GitHub Actions workflow to diagnose CI failures
- `--wait-for-prebuilt=<longer>` to wait longer
- `--local-build` to opt out of waiting and compile from source

## Phase 3 — Local incremental compile (`--local-build`)

When `--local-build` is passed (or the wait timed out and the user re-runs
with `--local-build`), Medusa falls back to a local `cargo install` from
the cached update directory. The build:

- reuses an existing cached binary when the same revision + host triple
  have been compiled before (cache short-circuit, sub-second)
- otherwise runs a full `cargo install --release` with the cache short-circuit
  at the workspace level (typical: 10–15 minutes on first run, ~6–12 minutes
  on subsequent updates of the same revision)

The local-build path exists for offline development and CI hermetic builds.
Production deployments should rely on the prebuilt artifact.

### Cache location

```
<repo>/.medusa/update-cache/
├── cargo-target/       # CARGO_TARGET_DIR — shared across all medusa update runs
│   ├── release/medusa  # cached binary
│   ├── incremental/    # Cargo incremental state
│   └── ...             # all 394 crates' artifacts
├── last-revision       # SHA1 of the last successful build
└── host-triple         # rustc -vV host triple at build time
```

### What triggers a cache hit

The local compile is **skipped entirely** when all three are true:

1. `last-revision` matches the requested main revision
2. `host-triple` matches the current `rustc -vV` output
3. `cargo-target/release/medusa` exists and is non-empty

When all three match, `medusa update` copies the cached binary into the
install root and proceeds straight to the atomic swap — typically under
500 ms instead of 5–15 minutes.

When any of the three drift (new main revision, toolchain upgrade, cache
wipe), the local compile runs in full and overwrites the cache.

### Cargo invocation (when the cache misses)

```
cargo install \
  --git https://github.com/benclawbot/Medusa.git \
  --rev <sha> \
  --locked \
  --bin medusa \
  --root <temp-install-root> \
  medusa-cli
```

With these env vars:

| Env var | Why |
|---|---|
| `CARGO_TARGET_DIR=<repo>/.medusa/update-cache/cargo-target` | Shared target dir across runs |
| `CARGO_INCREMENTAL=1` | Force incremental even under the release profile |
| `CARGO_PROFILE_RELEASE_LTO=thin` | Skip full LTO on unchanged crates (saves 20–60s) |
| `CARGO_PROFILE_RELEASE_CODEGEN_UNITS=256` | Better incremental reuse, smaller objects |
| `RUSTC_WRAPPER=sccache` | Set automatically when `sccache` is on PATH |

### Why incremental is safe here

The Medusa repo's `Cargo.lock` is the dependency authority (verified by
`--locked`). When the requested revision and lockfile are unchanged, the
incremental cache state is also unchanged. Cross-revision contamination is
prevented by the `last-revision` marker, and cross-toolchain contamination
is prevented by the `host-triple` marker.

### Failure modes

| Symptom | Likely cause |
|---|---|
| `cargo install` exits non-zero | Source change broke compilation on `main` — wait for CI fix |
| Cache never hits | `CARGO_TARGET_DIR` was overridden by the user's environment |
| Binary copy fails | `cargo-target/release/` was deleted between runs |
| Host triple mismatch | Toolchain upgrade — full rebuild expected on the next run |
