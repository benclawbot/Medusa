# Complete Installation Guide Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the Medusa README sufficient to install every prerequisite, install Medusa on Windows, macOS, or Linux, configure live model access, and verify the result.

**Architecture:** Keep installation guidance in `README.md` and organize it as a fast path followed by operating-system-specific prerequisite setup, shared Medusa installation, verification, and focused troubleshooting. Do not add executable installer scripts or change application code.

**Tech Stack:** Markdown, PowerShell, POSIX shell, Winget, Homebrew, apt, dnf, pacman, Rustup, Cargo.

## Global Constraints

Git, Rust 1.88 or newer, and Cargo are required.
`MINIMAX_API_KEY` is required for live MiniMax execution and must not be written into the repository.
Node.js 22 is optional unless browser verification is enabled.
The installed executable must resolve from Cargo's binary directory on `PATH`.
The first optimized source build may take several minutes.

---

### Task 1: Replace the README installation flow

**Files:**
- Modify: `README.md:21-43`
- Reference: `rust-toolchain.toml`
- Reference: `docs/superpowers/specs/2026-07-12-complete-installation-guide-design.md`

**Interfaces:**
- Consumes: the repository's pinned Rust 1.88.0 toolchain and `crates/medusa-cli` Cargo package.
- Produces: copy-pasteable installation and verification commands for Windows, macOS, and Linux users.

- [ ] **Step 1: Expand requirements and add the fast path**

Replace the current requirements and installation blocks with explicit required versus optional prerequisites. Add the existing-tools fast path:

```bash
git clone https://github.com/benclawbot/Medusa.git
cd Medusa
cargo install --path crates/medusa-cli --locked
medusa --version
medusa doctor
```

State that `medusa doctor` reports live execution unavailable until `MINIMAX_API_KEY` is configured and that the initial release build may take several minutes.

- [ ] **Step 2: Add Windows PowerShell setup**

Document Winget installs for Git and Rustup, optional Node.js 22 installation, opening a new PowerShell session, ensuring `%USERPROFILE%\.cargo\bin` is in the persistent user `PATH`, installing the pinned toolchain, cloning Medusa, and running Cargo install. Use PowerShell syntax throughout and include:

```powershell
winget install --id Git.Git -e
winget install --id Rustlang.Rustup -e
winget install --id OpenJS.NodeJS.22 -e
rustup toolchain install 1.88.0 --profile minimal --component clippy,rustfmt
rustup default 1.88.0
```

- [ ] **Step 3: Add macOS setup**

Document Xcode command-line tools, Homebrew Git and optional Node 22, Rustup installation, Cargo environment loading, the pinned toolchain, cloning, and Cargo installation. Use `brew install git`, `brew install node@22`, and the official Rustup bootstrap command with an explicit note to inspect remote installation commands before running them.

- [ ] **Step 4: Add Linux setup**

Provide separate prerequisite commands for Debian/Ubuntu (`apt`), Fedora/RHEL (`dnf`), and Arch (`pacman`). Install compiler/build essentials, Git, and curl before Rustup. Keep Node 22 explicitly optional and link users to their distribution or Node.js installation guidance instead of assuming one package name works across every supported distribution.

- [ ] **Step 5: Add credential and PATH configuration**

Document session-only and persistent-safe patterns for `MINIMAX_API_KEY` without placing a real key in repository files. Explain that users should restart their shell after persistent environment changes. Include Windows user-`PATH` repair and POSIX `source "$HOME/.cargo/env"` guidance.

- [ ] **Step 6: Add verification and troubleshooting**

Add commands for `git --version`, `rustc --version`, `cargo --version`, optional `node --version`, `medusa --version`, and `medusa doctor`. Explain how to diagnose Cargo installed but not on `PATH`. For Rustup partial-toolchain conflicts, first recommend closing Rust/Cargo processes and retrying `rustup toolchain install 1.88.0`; describe uninstall/reinstall of only the `1.88.0` toolchain as the final recovery step and clearly state that it removes that toolchain before downloading it again.

- [ ] **Step 7: Validate the documentation**

Run:

```powershell
git diff --check
rg -n "Rust 1\.88|Node\.js 22|MINIMAX_API_KEY|cargo install --path crates/medusa-cli --locked|medusa doctor|\.cargo" README.md
git status --short
```

Expected: `git diff --check` exits 0; every required installation concept is present; only the intended README and planning documents are changed.

- [ ] **Step 8: Commit the README update**

```powershell
git add README.md
git commit -m "docs: make Medusa installation self-contained"
```

### Task 2: Activate the installed CLI on this Windows account

**Files:**
- Modify: none

**Interfaces:**
- Consumes: `C:\Users\thoma\.cargo\bin\medusa.exe` installed by Cargo.
- Produces: a persistent user `PATH` entry that lets new terminals resolve `medusa` by name.

- [ ] **Step 1: Add Cargo's binary directory without replacing existing PATH entries**

Read the user-scoped `PATH`, compare entries case-insensitively after trimming trailing separators, and append `C:\Users\thoma\.cargo\bin` only when absent. Save the result with `[Environment]::SetEnvironmentVariable('Path', $updatedPath, 'User')`.

- [ ] **Step 2: Refresh and verify command discovery**

Prepend the Cargo directory to the current process `PATH`, then run:

```powershell
Get-Command medusa
medusa --version
```

Expected: `Get-Command` resolves `C:\Users\thoma\.cargo\bin\medusa.exe` and the version command prints `medusa 1.0.0` with exit code 0.
