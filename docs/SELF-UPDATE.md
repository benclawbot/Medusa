# Self-Update (`medusa update`)

`medusa update` upgrades the running CLI binary to the latest verified
revision on `main`. The flow has two phases:

## Phase 1 — Prebuilt artifact (default)

If CI has published a revision-bound prebuilt binary for the running
platform, `medusa update` downloads it, verifies its signed manifest, and
swaps it into place atomically. This is the fast path and what production
users see in the happy case.

## Phase 2 — Local incremental compile (fallback)

If the prebuilt is unavailable (CI still publishing, network failure,
self-hosted runner), `medusa update` falls back to compiling locally from
the verified `main` revision. The fallback uses **incremental compilation
across invocations** to keep subsequent updates cheap.

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
