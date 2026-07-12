# Complete Installation Guide Design

## Goal

Replace the README's assumption-heavy installation section with a complete, copy-pasteable path for Windows, macOS, and Linux. A new user should be able to install or verify every required tool, install Medusa, configure live model access, and confirm the installation without having to infer missing environment setup.

## Scope

The change is limited to `README.md`. It documents installation from the source repository because Medusa is not currently distributed through a package registry or release installer. It does not add executable bootstrap scripts, change application behavior, or alter dependency versions.

## Installation Structure

The README will distinguish core prerequisites from optional browser-verification tooling. Git, Rust 1.88 or newer, Cargo, and a MiniMax API key are required for a live Medusa session. Node.js 22 is required only when browser verification is enabled.

A short fast path will serve users who already have the prerequisites. Separate Windows PowerShell, macOS, and Linux sections will then provide package-manager commands to install missing tools. Linux guidance will cover common `apt`, `dnf`, and `pacman` systems without claiming universal distribution support.

Every platform flow will cover Cargo's binary directory on `PATH`, cloning the repository, installing with the locked dependency graph, setting `MINIMAX_API_KEY` without writing it into the repository, and opening a new shell where necessary.

## Verification and Recovery

The documented verification sequence will run `git --version`, `rustc --version`, `cargo --version`, optional `node --version`, `medusa --version`, and `medusa doctor`. It will explain that the first optimized Rust build can take several minutes.

A focused troubleshooting section will address two failures observed during installation: Cargo being installed but absent from `PATH`, and Rustup reporting a partial or conflicting pinned toolchain. Recovery commands will preserve user data where possible and make destructive toolchain replacement explicit rather than automatic.

`medusa doctor` will be explained as successful only when required checks, including `MINIMAX_API_KEY`, pass. Browser tooling will remain optional and its absence will not be described as blocking the core TUI.

## Alternatives Rejected

Standalone PowerShell and shell installers were rejected because they introduce executable installation code, platform testing obligations, and a larger security surface than the requested README guidance. A checklist-only rewrite was rejected because it would leave prerequisite installation and `PATH` repair to the user. A single cross-platform command block was rejected because package managers and persistent environment configuration differ materially across operating systems.

## Validation

The final README will be reviewed for command syntax, version consistency with `rust-toolchain.toml`, correct repository paths, and absence of real credentials. Commands that are safe in the current Windows environment will be executed directly. Platform-specific commands that cannot be run locally will be checked structurally and kept to established package-manager syntax.
