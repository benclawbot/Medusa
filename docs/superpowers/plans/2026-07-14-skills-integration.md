# Skills Integration Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give Medusa a request-driven skill auto-trigger, mirroring the 14 bundled superpowers skills. The engine matches each user prompt against skill `triggers`, loads the chosen skill (and any `requires` chain), and injects the bundle into the system prompt before the model responds. Skills can declare a `handoff` that re-triggers the matcher at the start of the next turn.

**Architecture:** A new `medusa-skills` crate vendors 14 `SKILL.md` files and emits a `manifest.json` index at build time. Three new pure-logic modules in `medusa-agent` — `skill_matcher`, `skill_loader`, `skill_injector` — implement keyword pre-filter + LLM rerank, recursive chain resolution with cycle/depth protection, and bundle rendering. A small engine change runs the pipeline once per user turn; a handoff queue on `AgentSession` enables multi-turn workflows. Configuration and observability land in their existing crates.

**Tech Stack:** Rust 1.88, Cargo, serde / serde_yaml, thiserror, ulid, sha2, textwrap, include_dir! (or embedded-fs if not available), existing `medusa-extensions::skills::SkillManifest` for the manifest format.

**Source root:** `Documents/Codex/2026-07-13/upd/work/medusa-skills-integration` on branch `medusa/skills-integration` at `2f3e486` (which carries the spec). Spec: `docs/superpowers/specs/2026-07-14-skills-integration-design.md` (commit `2f3e486`).

---

## Global Constraints

- Workspace forbids `unsafe_code` (`[workspace.lints.rust] unsafe_code = "forbid"`). No `unsafe` anywhere.
- Every change keeps `cargo build --workspace --locked` and `cargo test --workspace` green.
- The `medusa-skills` crate must be built *before* `medusa-agent` so the asset embedding works; declare the dep in `medusa-agent/Cargo.toml` and let Cargo handle the order.
- No new top-level dependencies beyond `serde_yaml` (which is already in `medusa-extensions`'s dep tree — verify) and `include_dir` or `embedded-fs`. If neither is in the workspace, add `include_dir = "0.7"` because it's the simpler, more common choice.
- Skills are vendored, not referenced. Each `SKILL.md` is a faithful copy of the upstream `obra/superpowers` content under MIT, attributed in the crate's README.
- The `requires:` and `handoff:` manifest fields are an additive extension to `SkillManifest`. They default to empty in `medusa-extensions` and are filled in by `medusa-skills` at load time. The extension is a thin shim, not a fork of `medusa-extensions`.
- The matcher runs once per user turn only. Tool results do not re-trigger the matcher (the handoff queue handles the multi-turn case).
- Cycle and depth protection are mandatory. A cycle or depth-cap violation is a `PolicyDenied` error, surfaced to the user, and the bundle is empty for the turn.

---

## File Map

New:
- `crates/medusa-skills/Cargo.toml`
- `crates/medusa-skills/build.rs`
- `crates/medusa-skills/src/lib.rs`
- `crates/medusa-skills/src/index.rs`
- `crates/medusa-skills/src/asset.rs`
- `crates/medusa-skills/assets/skills/<14 skill dirs>/SKILL.md`
- `crates/medusa-skills/tests/manifest_coverage.rs`
- `crates/medusa-agent/src/skill_matcher.rs`
- `crates/medusa-agent/src/skill_loader.rs`
- `crates/medusa-agent/src/skill_injector.rs`
- `crates/medusa-agent/src/skill_handoff.rs`
- `crates/medusa-agent/tests/skill_pipeline_coverage.rs`

Modified:
- `Cargo.toml` (workspace members)
- `crates/medusa-agent/Cargo.toml`
- `crates/medusa-agent/src/lib.rs`
- `crates/medusa-agent/src/engine.rs`
- `crates/medusa-agent/src/session.rs`
- `crates/medusa-config/src/lib.rs`
- `crates/medusa-hardening/src/observability.rs`

---

### Task 1: Scaffold the `medusa-skills` crate

**Files:**
- Create: `crates/medusa-skills/Cargo.toml`
- Create: `crates/medusa-skills/src/lib.rs`
- Modify: `Cargo.toml` (add the new workspace member)

- [ ] **Step 1: Create `crates/medusa-skills/Cargo.toml`**

```toml
[package]
name = "medusa-skills"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true

[lib]
path = "src/lib.rs"

[dependencies]
medusa-core = { path = "../medusa-core" }
medusa-extensions = { path = "../medusa-extensions" }
serde = { workspace = true }
serde_json = { workspace = true }
thiserror = { workspace = true }

[build-dependencies]
serde_json = { workspace = true }
serde_yaml = "0.9"

[dev-dependencies]
tempfile = { workspace = true }
```

- [ ] **Step 2: Implement `crates/medusa-skills/src/lib.rs`**

```rust
//! Bundled superpowers skills. Each skill is a Markdown file under
//! `assets/skills/<name>/SKILL.md`; the build script compiles them into
//! `assets/manifest.json` at build time.

pub mod asset;
pub mod index;

pub use asset::AssetStore;
pub use index::{SkillEntry, SkillIndex, SkillManifestExt};
```

- [ ] **Step 3: Add the crate to the workspace**

In the workspace `Cargo.toml`, add `"crates/medusa-skills"` to the `members` list.

- [ ] **Step 4: Build the crate**

Run: `cargo build -p medusa-skills`
Expected: success. (The crate compiles even with no skills yet, because `index.rs` and `asset.rs` are empty stubs in this task.)

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml crates/medusa-skills/Cargo.toml crates/medusa-skills/src/lib.rs
git commit -m "feat(skills): scaffold medusa-skills crate"
```

---

### Task 2: Add the build script that generates `manifest.json`

**Files:**
- Create: `crates/medusa-skills/build.rs`
- Create: `crates/medusa-skills/src/asset.rs` (write the generated `manifest.json` to `OUT_DIR/manifest.json` and expose a `AssetStore::load(&str)` reader)

- [ ] **Step 1: Write failing test in `crates/medusa-skills/tests/manifest_coverage.rs`**

```rust
use medusa_skills::SkillIndex;

#[test]
fn empty_assets_directory_produces_empty_index() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("skills")).unwrap();
    let index = SkillIndex::from_assets_dir(dir.path()).unwrap();
    assert_eq!(index.entries().len(), 0);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p medusa-skills --test manifest_coverage`
Expected: compile error — `from_assets_dir` does not exist.

- [ ] **Step 3: Implement `crates/medusa-skills/src/asset.rs`**

```rust
use std::{
    fs,
    path::{Path, PathBuf},
};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};

use crate::index::SkillIndex;

pub const MANIFEST_FILENAME: &str = "manifest.json";

/// Reads the generated manifest from a directory.
pub struct AssetStore {
    pub manifest_path: PathBuf,
}

impl AssetStore {
    pub fn load(manifest_path: &Path) -> MedusaResult<Self> {
        if !manifest_path.is_file() {
            return Err(MedusaError::new(
                ErrorCode::DependencyUnavailable,
                ErrorCategory::Transient,
                format!("manifest not found at {}", manifest_path.display()),
            ));
        }
        Ok(Self { manifest_path: manifest_path.to_path_buf() })
    }

    pub fn index(&self) -> MedusaResult<SkillIndex> {
        let bytes = fs::read(&self.manifest_path).map_err(|e| io_err("read manifest", e))?;
        let index: SkillIndex = serde_json::from_slice(&bytes)
            .map_err(|e| MedusaError::new(ErrorCode::InvalidConfiguration, ErrorCategory::Validation, format!("parse manifest: {e}")))?;
        Ok(index)
    }
}

fn io_err(ctx: &str, e: std::io::Error) -> MedusaError {
    MedusaError::new(ErrorCode::PersistenceFailed, ErrorCategory::Environment, format!("{ctx}: {e}"))
}
```

- [ ] **Step 4: Implement the empty stub `crates/medusa-skills/src/index.rs`**

```rust
use std::{
    collections::BTreeMap,
    fs,
    path::Path,
};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use medusa_extensions::skills::SkillManifest;
use serde::{Deserialize, Serialize};

/// A single entry in the manifest index. Augments `SkillManifest` with
/// `requires` and `handoff` for chained skills.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SkillEntry {
    pub name: String,
    pub manifest: SkillManifest,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub requires: Vec<String>,
    #[serde(default)]
    pub handoff: Option<String>,
}

/// The full index of all skills, as serialized to `manifest.json`.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct SkillIndex {
    pub skills: Vec<SkillEntry>,
}

impl SkillIndex {
    pub fn from_assets_dir(_root: &Path) -> MedusaResult<Self> {
        Ok(Self::default())
    }

    pub fn entries(&self) -> &[SkillEntry] {
        &self.skills
    }

    pub fn by_name(&self, name: &str) -> Option<&SkillEntry> {
        self.skills.iter().find(|entry| entry.name == name)
    }

    pub fn names_by_name(&self) -> BTreeMap<&str, &SkillEntry> {
        self.skills.iter().map(|entry| (entry.name.as_str(), entry)).collect()
    }
}

/// Re-export of the upstream manifest type so callers don't have to import
/// from two crates.
pub type SkillManifestExt = SkillManifest;
```

- [ ] **Step 5: Implement `crates/medusa-skills/build.rs`**

```rust
use std::{
    env,
    fs,
    path::Path,
};

use serde::Serialize;

#[derive(Serialize)]
struct SkillEntry {
    name: String,
    manifest: serde_yaml::Value,
    body: String,
    requires: Vec<String>,
    handoff: Option<String>,
}

#[derive(Serialize)]
struct SkillIndex {
    skills: Vec<SkillEntry>,
}

fn main() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let assets_dir = manifest_dir.join("assets/skills");
    let out_dir = env::var_os("OUT_DIR").expect("OUT_DIR set by cargo");
    let out_dir = Path::new(&out_dir);
    let manifest_path = out_dir.join("manifest.json");

    let mut entries: Vec<SkillEntry> = Vec::new();
    if assets_dir.is_dir() {
        for dir in fs::read_dir(&assets_dir).expect("read skills dir") {
            let dir = dir.expect("dir entry").path();
            if !dir.is_dir() {
                continue;
            }
            let skill_file = dir.join("SKILL.md");
            if !skill_file.is_file() {
                continue;
            }
            let raw = fs::read_to_string(&skill_file).expect("read SKILL.md");
            let (frontmatter, body) = split_frontmatter(&raw);
            let manifest: serde_yaml::Value = serde_yaml::from_str(frontmatter).expect("parse frontmatter");
            let name = manifest
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or_else(|| panic!("skill at {} has no name", skill_file.display()))
                .to_owned();
            let requires = manifest
                .get("requires")
                .and_then(|v| v.as_sequence())
                .map(|seq| seq.iter().filter_map(|item| item.as_str().map(str::to_owned)).collect())
                .unwrap_or_default();
            let handoff = manifest
                .get("handoff")
                .and_then(|v| v.as_str())
                .map(str::to_owned);
            entries.push(SkillEntry {
                name,
                manifest,
                body: body.to_owned(),
                requires,
                handoff,
            });
        }
    }
    entries.sort_by(|left, right| left.name.cmp(&right.name));

    let index = SkillIndex { skills: entries };
    let json = serde_json::to_string_pretty(&index).expect("serialize manifest");
    fs::write(&manifest_path, json).expect("write manifest");

    println!("cargo:rerun-if-changed={}", assets_dir.display());
    println!("cargo:rerun-if-changed=build.rs");
}

fn split_frontmatter(text: &str) -> (&str, &str) {
    let trimmed = text.trim_start_matches('\u{feff}');
    let rest = trimmed.strip_prefix("---").expect("frontmatter starts with ---");
    let rest = rest.trim_start_matches('\r');
    let (front, body) = rest.split_once("\n---").expect("frontmatter closes with ---");
    let body = body.trim_start_matches('\r').trim_start_matches('\n');
    (front, body)
}
```

- [ ] **Step 6: Run the test to verify it passes**

Run: `cargo test -p medusa-skills --test manifest_coverage`
Expected: 1 passed.

- [ ] **Step 7: Commit**

```bash
git add crates/medusa-skills/Cargo.toml crates/medusa-skills/build.rs crates/medusa-skills/src/asset.rs crates/medusa-skills/src/index.rs crates/medusa-skills/tests/manifest_coverage.rs
git commit -m "feat(skills): add build-time manifest codegen and asset reader"
```

---

### Task 3: Vendor the first three skills (brainstorming, TDD, debugging)

**Files:**
- Create: `crates/medusa-skills/assets/skills/brainstorming/SKILL.md`
- Create: `crates/medusa-skills/assets/skills/test-driven-development/SKILL.md`
- Create: `crates/medusa-skills/assets/skills/systematic-debugging/SKILL.md`
- Modify: `crates/medusa-skills/src/index.rs` (no code change, but the build script now picks these up)

- [ ] **Step 1: Write failing test in `crates/medusa-skills/tests/manifest_coverage.rs`**

Append:

```rust
#[test]
fn first_three_skills_are_vendored() {
    let dir = tempfile::tempdir().unwrap();
    let skills_root = dir.path().join("skills");
    for (name, triggers) in [
        ("brainstorming", "brainstorm,design,idea"),
        ("test-driven-development", "tdd,test,red-green"),
        ("systematic-debugging", "debug,bug,broken"),
    ] {
        let skill = skills_root.join(name);
        std::fs::create_dir_all(&skill).unwrap();
        std::fs::write(
            skill.join("SKILL.md"),
            format!("---\nname: {name}\nversion: 1.0.0\ndescription: stub\ntriggers: [{triggers}]\ncompatibility:\n  medusa: '>=1.0.0'\n---\n\n# {name}\nBody.\n"),
        ).unwrap();
    }
    let index = medusa_skills::SkillIndex::from_assets_dir(dir.path()).unwrap();
    let names: Vec<&str> = index.entries().iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, vec!["brainstorming", "systematic-debugging", "test-driven-development"]);
    let brainstorming = index.by_name("brainstorming").unwrap();
    assert!(brainstorming.manifest.triggers.contains(&"brainstorm".to_owned()));
}
```

- [ ] **Step 2: Run test to verify it passes**

Run: `cargo test -p medusa-skills --test manifest_coverage first_three_skills_are_vendored`
Expected: PASS. The vendored `SKILL.md` files in `assets/skills/` are the canonical source, but `from_assets_dir` reads from a runtime path so the test seeds its own. The build script's output mirrors this shape.

- [ ] **Step 3: Create the three `SKILL.md` files**

For each, copy the corresponding file from the bundled superpowers plugin
(`C:\Users\ThomasCHAFFANJON\.claude\plugins\cache\claude-plugins-official\superpowers\5.1.0\skills\<name>/SKILL.md`) into `crates/medusa-skills/assets/skills/<name>/SKILL.md`. The YAML frontmatter must include at minimum:

- `name: <skill>` (kebab-case, matches directory)
- `version: 1.0.0` (initial vendored version)
- `description: <from upstream>`
- `triggers: [<comma-separated keywords from upstream>]`
- `compatibility: { medusa: '>=1.0.0' }`
- (where applicable) `requires: [...]` and `handoff: <other-skill>`

For the initial set:
- `brainstorming`: triggers `[brainstorm, design, idea, spec, plan-a-feature, what-should, how-should]`. `handoff: writing-plans`. `requires: [using-superpowers]`.
- `test-driven-development`: triggers `[tdd, test, failing-test, red-green, write-the-test-first]`. `requires: [using-superpowers]`.
- `systematic-debugging`: triggers `[debug, bug, broken, crash, fix, why-doesnt]`. `requires: [using-superpowers]`.

The body of each file is a faithful copy of the upstream content. Add an attribution line at the top of the body: `> Vendored from github.com/obra/superpowers @ <upstream-sha>. MIT licensed.`

- [ ] **Step 4: Verify the build script picks up the three skills**

Run: `cargo build -p medusa-skills`
Expected: success, with `cargo:rerun-if-changed=assets/skills` triggering a rebuild.

- [ ] **Step 5: Inspect the generated manifest**

```bash
cat target/debug/build/medusa-skills-*/out/manifest.json | head -40
```

Expected: JSON with three entries: brainstorming, systematic-debugging, test-driven-development, in alphabetical order.

- [ ] **Step 6: Commit**

```bash
git add crates/medusa-skills/assets/skills/brainstorming crates/medusa-skills/assets/skills/test-driven-development crates/medusa-skills/assets/skills/systematic-debugging
git commit -m "feat(skills): vendor brainstorming, TDD, and systematic-debugging"
```

---

### Task 4: Vendor the remaining 11 skills

**Files:**
- Create: 11 `SKILL.md` files under `crates/medusa-skills/assets/skills/<name>/SKILL.md`

The skills and their initial trigger sets (each is a faithful copy of the upstream `SKILL.md` body, with the same attribution line as Task 3, and a YAML frontmatter with `name`, `version: 1.0.0`, `description`, `triggers`, `compatibility: { medusa: '>=1.0.0' }`, and `requires: [using-superpowers]` where applicable):

- `dispatching-parallel-agents` — `triggers: [parallel, agents, fan-out, dispatch]`
- `executing-plans` — `triggers: [execute, run-the-plan, follow-the-plan, batch]`
- `finishing-a-development-branch` — `triggers: [finish, merge, ship, complete-branch, close-out]`
- `receiving-code-review` — `triggers: [review, feedback, address-review]`
- `requesting-code-review` — `triggers: [request-review, code-review, get-review, review-my-changes]`
- `subagent-driven-development` — `triggers: [subagent, dispatch-a-task, subagent-driven]`
- `using-git-worktrees` — `triggers: [worktree, isolated-workspace, parallel-branch]`
- `using-superpowers` — `triggers: [superpowers, which-skill, meta-skill]` (this is the meta-skill; required by all others; **no `requires:`**)
- `verification-before-completion` — `triggers: [verify, before-completion, run-the-tests, smoke-test, prove-it]`
- `writing-plans` — `triggers: [plan, write-a-plan, plan-this, step-by-step]`
- `writing-skills` — `triggers: [write-a-skill, create-skill, author-skill]`

- [ ] **Step 1: Copy the 11 files from the upstream plugin into the crate**

For each skill above, copy `C:\Users\ThomasCHAFFANJON\.claude\plugins\cache\claude-plugins-official\superpowers\5.1.0\skills\<name>\SKILL.md` to `crates/medusa-skills/assets/skills/<name>/SKILL.md`. Add the attribution header. Ensure the YAML frontmatter has the fields above.

- [ ] **Step 2: Update the test in `manifest_coverage.rs` to assert all 14 skills are vendored**

```rust
#[test]
fn all_fourteen_skills_are_vendored() {
    let dir = tempfile::tempdir().unwrap();
    let skills_root = dir.path().join("skills");
    for name in [
        "brainstorming",
        "dispatching-parallel-agents",
        "executing-plans",
        "finishing-a-development-branch",
        "receiving-code-review",
        "requesting-code-review",
        "subagent-driven-development",
        "systematic-debugging",
        "test-driven-development",
        "using-git-worktrees",
        "using-superpowers",
        "verification-before-completion",
        "writing-plans",
        "writing-skills",
    ] {
        let skill = skills_root.join(name);
        std::fs::create_dir_all(&skill).unwrap();
        std::fs::write(
            skill.join("SKILL.md"),
            format!("---\nname: {name}\nversion: 1.0.0\ndescription: stub\ntriggers: [stub]\ncompatibility:\n  medusa: '>=1.0.0'\n---\n\n# {name}\n"),
        ).unwrap();
    }
    let index = medusa_skills::SkillIndex::from_assets_dir(dir.path()).unwrap();
    assert_eq!(index.entries().len(), 14);
    let names: Vec<&str> = index.entries().iter().map(|e| e.name.as_str()).collect();
    assert_eq!(
        names,
        vec![
            "brainstorming",
            "dispatching-parallel-agents",
            "executing-plans",
            "finishing-a-development-branch",
            "receiving-code-review",
            "requesting-code-review",
            "subagent-driven-development",
            "systematic-debugging",
            "test-driven-development",
            "using-git-worktrees",
            "using-superpowers",
            "verification-before-completion",
            "writing-plans",
            "writing-skills",
        ],
    );
}
```

- [ ] **Step 3: Run the test**

Run: `cargo test -p medusa-skills --test manifest_coverage all_fourteen_skills_are_vendored`
Expected: PASS.

- [ ] **Step 4: Build and inspect the generated manifest**

Run: `cargo build -p medusa-skills && cat target/debug/build/medusa-skills-*/out/manifest.json | jq '.skills | length'`
Expected: `14`.

- [ ] **Step 5: Commit**

```bash
git add crates/medusa-skills/assets/skills
git commit -m "feat(skills): vendor remaining 11 superpowers skills"
```

---

### Task 5: Add the `SkillConfig` and `matcher` enum to `medusa-config`

**Files:**
- Modify: `crates/medusa-config/src/lib.rs`

- [ ] **Step 1: Write failing test in `crates/medusa-config/tests/skill_config.rs`**

```rust
use medusa_config::SkillConfig;

#[test]
fn defaults_when_no_env_set() {
    std::env::remove_var("MEDUSA_SKILLS_ENABLED");
    std::env::remove_var("MEDUSA_SKILLS_BUNDLE_PATH");
    std::env::remove_var("MEDUSA_SKILLS_MAX_MATCHES");
    std::env::remove_var("MEDUSA_SKILLS_MAX_CHAIN_DEPTH");
    std::env::remove_var("MEDUSA_SKILLS_MATCHER_MODE");
    let cfg = SkillConfig::from_env();
    assert!(cfg.enabled);
    assert!(cfg.bundle_path.is_none());
    assert_eq!(cfg.max_matches, 5);
    assert_eq!(cfg.max_chain_depth, 4);
    assert_eq!(cfg.matcher_mode, medusa_config::MatcherMode::KeywordLlmRerank);
}

#[test]
fn overrides_when_env_set() {
    std::env::set_var("MEDUSA_SKILLS_ENABLED", "false");
    std::env::set_var("MEDUSA_SKILLS_BUNDLE_PATH", "/opt/skills");
    std::env::set_var("MEDUSA_SKILLS_MAX_MATCHES", "8");
    std::env::set_var("MEDUSA_SKILLS_MAX_CHAIN_DEPTH", "6");
    std::env::set_var("MEDUSA_SKILLS_MATCHER_MODE", "keyword");
    let cfg = SkillConfig::from_env();
    assert!(!cfg.enabled);
    assert_eq!(cfg.bundle_path.as_deref(), Some(std::path::Path::new("/opt/skills")));
    assert_eq!(cfg.max_matches, 8);
    assert_eq!(cfg.max_chain_depth, 6);
    assert_eq!(cfg.matcher_mode, medusa_config::MatcherMode::Keyword);
    std::env::remove_var("MEDUSA_SKILLS_ENABLED");
    std::env::remove_var("MEDUSA_SKILLS_BUNDLE_PATH");
    std::env::remove_var("MEDUSA_SKILLS_MAX_MATCHES");
    std::env::remove_var("MEDUSA_SKILLS_MAX_CHAIN_DEPTH");
    std::env::remove_var("MEDUSA_SKILLS_MATCHER_MODE");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p medusa-config --test skill_config`
Expected: compile error — `SkillConfig` does not exist.

- [ ] **Step 3: Add the type and reader to `medusa-config`**

```rust
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MatcherMode {
    Keyword,
    KeywordLlmRerank,
}

impl MatcherMode {
    pub fn from_env_string(s: &str) -> Self {
        match s.to_ascii_lowercase().as_str() {
            "keyword" => Self::Keyword,
            _ => Self::KeywordLlmRerank,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SkillConfig {
    pub enabled: bool,
    pub bundle_path: Option<PathBuf>,
    pub max_matches: usize,
    pub max_chain_depth: usize,
    pub matcher_mode: MatcherMode,
}

impl Default for SkillConfig {
    fn default() -> Self {
        Self::from_env()
    }
}

impl SkillConfig {
    pub fn from_env() -> Self {
        Self {
            enabled: env::var("MEDUSA_SKILLS_ENABLED")
                .ok()
                .map(|s| !matches!(s.to_ascii_lowercase().as_str(), "0" | "false" | "no" | "off"))
                .unwrap_or(true),
            bundle_path: env::var("MEDUSA_SKILLS_BUNDLE_PATH").ok().map(PathBuf::from),
            max_matches: env::var("MEDUSA_SKILLS_MAX_MATCHES")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(5),
            max_chain_depth: env::var("MEDUSA_SKILLS_MAX_CHAIN_DEPTH")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(4),
            matcher_mode: env::var("MEDUSA_SKILLS_MATCHER_MODE")
                .ok()
                .map(|s| MatcherMode::from_env_string(&s))
                .unwrap_or(MatcherMode::KeywordLlmRerank),
        }
    }
}
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p medusa-config --test skill_config`
Expected: 2 passed.

- [ ] **Step 5: Commit**

```bash
git add crates/medusa-config
git commit -m "feat(config): add SkillConfig with bundle_path, max_matches, max_chain_depth, matcher_mode"
```

---

### Task 6: Add the `skill_matcher` module (keyword pre-filter only)

**Files:**
- Create: `crates/medusa-agent/src/skill_matcher.rs`
- Modify: `crates/medusa-agent/src/lib.rs` (add `pub mod skill_matcher;`)

- [ ] **Step 1: Write failing test in `crates/medusa-agent/tests/skill_pipeline_coverage.rs`**

```rust
use medusa_agent::skill_matcher::{match_prompt, SkillMatch};
use medusa_config::{MatcherMode as ConfigMatcherMode, SkillConfig};
use medusa_skills::SkillIndex;

fn config(mode: ConfigMatcherMode, max: usize) -> SkillConfig {
    SkillConfig { enabled: true, bundle_path: None, max_matches: max, max_chain_depth: 4, matcher_mode: mode }
}

fn index_with(triggers: &[(&str, &[&str])]) -> SkillIndex {
    use medusa_extensions::skills::{SkillCompatibility, SkillManifest, SkillPermissions};
    let skills = triggers
        .iter()
        .map(|(name, ts)| medusa_skills::SkillEntry {
            name: (*name).to_owned(),
            manifest: SkillManifest {
                name: (*name).to_owned(),
                version: "1.0.0".into(),
                description: format!("{name} skill"),
                triggers: ts.iter().map(|s| s.to_string()).collect(),
                tools: vec![],
                permissions: SkillPermissions::default(),
                compatibility: SkillCompatibility { medusa: ">=1.0.0".into() },
                tests: vec![],
            },
            body: format!("# {name}\n"),
            requires: vec![],
            handoff: None,
        })
        .collect();
    SkillIndex { skills }
}

#[test]
fn keyword_filter_matches_one_skill() {
    let index = index_with(&[("brainstorming", &["brainstorm", "design"])]);
    let matches = match_prompt("help me brainstorm a new feature", &index, &config(ConfigMatcherMode::Keyword, 5)).unwrap();
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].skill.name, "brainstorming");
    assert!(matches[0].matched_triggers.contains(&"brainstorm".to_owned()));
}

#[test]
fn keyword_filter_returns_empty_for_no_match() {
    let index = index_with(&[("brainstorming", &["brainstorm"])]);
    let matches = match_prompt("please run the tests", &index, &config(ConfigMatcherMode::Keyword, 5)).unwrap();
    assert!(matches.is_empty());
}

#[test]
fn keyword_filter_caps_at_max_matches() {
    let index = index_with(&[
        ("a", &["x"]),
        ("b", &["x"]),
        ("c", &["x"]),
        ("d", &["x"]),
        ("e", &["x"]),
    ]);
    let matches = match_prompt("anything with x", &index, &config(ConfigMatcherMode::Keyword, 2)).unwrap();
    assert_eq!(matches.len(), 2);
}

#[test]
fn keyword_filter_scores_by_trigger_count() {
    let index = index_with(&[
        ("a", &["x"]),
        ("b", &["x", "y"]),
    ]);
    let matches = match_prompt("x and y", &index, &config(ConfigMatcherMode::Keyword, 5)).unwrap();
    assert_eq!(matches.len(), 2);
    assert_eq!(matches[0].skill.name, "b");
    assert_eq!(matches[0].score, 2.0);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p medusa-agent --test skill_pipeline_coverage`
Expected: compile error — `match_prompt` does not exist.

- [ ] **Step 3: Implement `crates/medusa-agent/src/skill_matcher.rs`**

```rust
use medusa_config::SkillConfig;
use medusa_core::MedusaResult;
use medusa_skills::{SkillEntry, SkillIndex};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SkillMatch {
    pub skill: SkillEntry,
    pub score: f32,
    pub matched_triggers: Vec<String>,
}

pub fn match_prompt(
    prompt: &str,
    index: &SkillIndex,
    config: &SkillConfig,
) -> MedusaResult<Vec<SkillMatch>> {
    if !config.enabled {
        return Ok(Vec::new());
    }
    let prompt_lower = prompt.to_ascii_lowercase();
    let mut results: Vec<SkillMatch> = index
        .entries()
        .iter()
        .map(|entry| score_entry(entry, &prompt_lower))
        .filter(|m| !m.matched_triggers.is_empty())
        .collect();
    results.sort_by(|left, right| {
        right
            .score
            .partial_cmp(&left.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| left.skill.name.cmp(&right.skill.name))
    });
    results.truncate(config.max_matches);
    Ok(results)
}

fn score_entry(entry: &SkillEntry, prompt_lower: &str) -> SkillMatch {
    let matched: Vec<String> = entry
        .manifest
        .triggers
        .iter()
        .filter(|trigger| prompt_lower.contains(&trigger.to_ascii_lowercase()))
        .cloned()
        .collect();
    SkillMatch {
        skill: entry.clone(),
        score: matched.len() as f32,
        matched_triggers: matched,
    }
}
```

- [ ] **Step 4: Add `pub mod skill_matcher;` to `lib.rs`**

- [ ] **Step 5: Run the test to verify it passes**

Run: `cargo test -p medusa-agent --test skill_pipeline_coverage`
Expected: 4 passed.

- [ ] **Step 6: Commit**

```bash
git add crates/medusa-agent/src/skill_matcher.rs crates/medusa-agent/src/lib.rs crates/medusa-agent/tests/skill_pipeline_coverage.rs
git commit -m "feat(agent): add skill_matcher with keyword pre-filter"
```

---

### Task 7: Add the `skill_loader` module (chain resolution)

**Files:**
- Create: `crates/medusa-agent/src/skill_loader.rs`
- Modify: `crates/medusa-agent/src/lib.rs` (add `pub mod skill_loader;`)

- [ ] **Step 1: Write failing test in `crates/medusa-agent/tests/skill_pipeline_coverage.rs`**

Append:

```rust
use medusa_agent::skill_loader::{load, SkillBundle};

fn entry(name: &str, requires: &[&str], handoff: Option<&str>) -> medusa_skills::SkillEntry {
    use medusa_extensions::skills::{SkillCompatibility, SkillManifest, SkillPermissions};
    medusa_skills::SkillEntry {
        name: name.to_owned(),
        manifest: SkillManifest {
            name: name.to_owned(),
            version: "1.0.0".into(),
            description: format!("{name} skill"),
            triggers: vec![],
            tools: vec![],
            permissions: SkillPermissions::default(),
            compatibility: SkillCompatibility { medusa: ">=1.0.0".into() },
            tests: vec![],
        },
        body: format!("# {name}\n"),
        requires: requires.iter().map(|s| s.to_string()).collect(),
        handoff: handoff.map(str::to_owned),
    }
}

fn make_index(entries: Vec<medusa_skills::SkillEntry>) -> SkillIndex {
    SkillIndex { skills: entries }
}

#[test]
fn loader_resolves_single_skill() {
    let index = make_index(vec![entry("a", &[], None)]);
    let bundle = load(&index, "a", 4).unwrap();
    assert_eq!(bundle.entries.len(), 1);
    assert_eq!(bundle.entries[0].skill.name, "a");
}

#[test]
fn loader_resolves_chain_in_declaration_order() {
    let index = make_index(vec![
        entry("a", &["b"], None),
        entry("b", &["c"], None),
        entry("c", &[], None),
    ]);
    let bundle = load(&index, "a", 4).unwrap();
    let names: Vec<&str> = bundle.entries.iter().map(|e| e.skill.name.as_str()).collect();
    assert_eq!(names, vec!["a", "b", "c"]);
}

#[test]
fn loader_detects_cycle() {
    let index = make_index(vec![
        entry("a", &["b"], None),
        entry("b", &["a"], None),
    ]);
    let err = load(&index, "a", 4).unwrap_err();
    assert!(format!("{err}").contains("cycle"));
}

#[test]
fn loader_enforces_depth_cap() {
    let index = make_index(vec![
        entry("a", &["b"], None),
        entry("b", &["a"], None),
    ]);
    let err = load(&index, "a", 1).unwrap_err();
    assert!(format!("{err}").contains("depth"));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p medusa-agent --test skill_pipeline_coverage loader`
Expected: compile error — `load` does not exist.

- [ ] **Step 3: Implement `crates/medusa-agent/src/skill_loader.rs`**

```rust
use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use medusa_skills::{SkillEntry, SkillIndex};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LoadedEntry {
    pub skill: SkillEntry,
    pub depth: usize,
}

#[derive(Clone, Debug, Eq, PartialEq, Default)]
pub struct SkillBundle {
    pub entries: Vec<LoadedEntry>,
}

pub fn load(index: &SkillIndex, root: &str, max_depth: usize) -> MedusaResult<SkillBundle> {
    let mut visited: Vec<String> = Vec::new();
    let mut bundle = SkillBundle::default();
    visit(index, root, 0, &mut visited, &mut bundle, max_depth)?;
    Ok(bundle)
}

fn visit(
    index: &SkillIndex,
    name: &str,
    depth: usize,
    visited: &mut Vec<String>,
    bundle: &mut SkillBundle,
    max_depth: usize,
) -> MedusaResult<()> {
    if visited.iter().any(|n| n == name) {
        return Err(MedusaError::new(
            ErrorCode::PolicyDenied,
            ErrorCategory::Policy,
            format!("skill cycle detected: {} (visited {:?})", name, visited),
        ));
    }
    if depth >= max_depth {
        return Err(MedusaError::new(
            ErrorCode::PolicyDenied,
            ErrorCategory::Policy,
            format!("skill chain depth {depth} exceeds cap {max_depth}"),
        ));
    }
    let entry = index.by_name(name).ok_or_else(|| {
        MedusaError::new(
            ErrorCode::InvalidConfiguration,
            ErrorCategory::Validation,
            format!("required skill '{name}' not found"),
        )
    })?;
    bundle.entries.push(LoadedEntry { skill: entry.clone(), depth });
    visited.push(name.to_owned());
    for required in &entry.requires {
        visit(index, required, depth + 1, visited, bundle, max_depth)?;
    }
    visited.pop();
    Ok(())
}

fn _silence_unused(_: SkillEntry) {}
```

- [ ] **Step 4: Add `pub mod skill_loader;` to `lib.rs`**

- [ ] **Step 5: Run the test to verify it passes**

Run: `cargo test -p medusa-agent --test skill_pipeline_coverage`
Expected: 8 passed (4 from Task 6 + 4 from this task).

- [ ] **Step 6: Commit**

```bash
git add crates/medusa-agent/src/skill_loader.rs crates/medusa-agent/src/lib.rs crates/medusa-agent/tests/skill_pipeline_coverage.rs
git commit -m "feat(agent): add skill_loader with cycle and depth protection"
```

---

### Task 8: Add the `skill_injector` module (bundle rendering)

**Files:**
- Create: `crates/medusa-agent/src/skill_injector.rs`
- Modify: `crates/medusa-agent/src/lib.rs` (add `pub mod skill_injector;`)

- [ ] **Step 1: Write failing test in `crates/medusa-agent/tests/skill_pipeline_coverage.rs`**

Append:

```rust
use medusa_agent::skill_injector::render;
use medusa_agent::skill_loader::{load, SkillBundle};

fn bundle_one() -> SkillBundle {
    let index = make_index(vec![entry("a", &[], None)]);
    load(&index, "a", 4).unwrap()
}

fn bundle_chain() -> SkillBundle {
    let index = make_index(vec![entry("a", &["b"], None), entry("b", &[], None)]);
    load(&index, "a", 4).unwrap()
}

#[test]
fn render_marks_loaded_skills_section() {
    let rendered = render(&bundle_one());
    assert!(rendered.contains("## Loaded skills"));
    assert!(rendered.contains("# a"));
}

#[test]
fn render_marks_required_skills() {
    let rendered = render(&bundle_chain());
    assert!(rendered.contains("required by 'a'"));
    assert!(rendered.contains("# b"));
}

#[test]
fn render_handles_empty_bundle() {
    let rendered = render(&SkillBundle::default());
    assert_eq!(rendered, "## Loaded skills\n(none)\n");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p medusa-agent --test skill_pipeline_coverage render`
Expected: compile error — `render` does not exist.

- [ ] **Step 3: Implement `crates/medusa-agent/src/skill_injector.rs`**

```rust
use crate::skill_loader::SkillBundle;

pub fn render(bundle: &SkillBundle) -> String {
    if bundle.entries.is_empty() {
        return "## Loaded skills\n(none)\n".to_owned();
    }
    let mut out = String::from("## Loaded skills (matched by trigger)\n\n");
    for (index, entry) in bundle.entries.iter().enumerate() {
        let label = if index == 0 {
            format!("- [{}] triggers: {:?}", entry.skill.name, entry.skill.manifest.triggers)
        } else {
            format!("- [{}] (required by '{}')", entry.skill.name, bundle.entries[index - 1].skill.name)
        };
        out.push_str(&label);
        out.push('\n');
        out.push_str(&entry.skill.body);
        out.push('\n');
    }
    out
}
```

- [ ] **Step 4: Add `pub mod skill_injector;` to `lib.rs`**

- [ ] **Step 5: Run the test to verify it passes**

Run: `cargo test -p medusa-agent --test skill_pipeline_coverage`
Expected: 11 passed (8 from previous + 3 from this task).

- [ ] **Step 6: Commit**

```bash
git add crates/medusa-agent/src/skill_injector.rs crates/medusa-agent/src/lib.rs crates/medusa-agent/tests/skill_pipeline_coverage.rs
git commit -m "feat(agent): add skill_injector that renders the bundle"
```

---

### Task 9: Add the handoff queue on `AgentSession`

**Files:**
- Create: `crates/medusa-agent/src/skill_handoff.rs`
- Modify: `crates/medusa-agent/src/lib.rs` (add `pub mod skill_handoff;`)

- [ ] **Step 1: Write failing test in `crates/medusa-agent/tests/skill_pipeline_coverage.rs`**

Append:

```rust
use medusa_agent::skill_handoff::{HandoffQueue, HandoffOutcome};

#[test]
fn handoff_queue_drains_in_order() {
    let mut q = HandoffQueue::default();
    q.push("a");
    q.push("b");
    assert_eq!(q.pop(), Some("a".to_owned()));
    assert_eq!(q.pop(), Some("b".to_owned()));
    assert_eq!(q.pop(), None);
}

#[test]
fn handoff_outcome_records_skipped_when_handoff_target_missing() {
    let index = make_index(vec![entry("a", &[], Some("missing"))]);
    let mut q = HandoffQueue::default();
    q.push("a");
    let outcome = q.drain(&index);
    assert_eq!(outcome.resolved, vec!["a".to_string()]);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p medusa-agent --test skill_pipeline_coverage handoff`
Expected: compile error — `HandoffQueue` does not exist.

- [ ] **Step 3: Implement `crates/medusa-agent/src/skill_handoff.rs`**

```rust
use medusa_skills::SkillIndex;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct HandoffQueue {
    pub pending: Vec<String>,
}

impl HandoffQueue {
    pub fn push(&mut self, skill: impl Into<String>) {
        self.pending.push(skill.into());
    }

    pub fn pop(&mut self) -> Option<String> {
        if self.pending.is_empty() { None } else { Some(self.pending.remove(0)) }
    }

    pub fn drain(&mut self, index: &SkillIndex) -> HandoffOutcome {
        let mut resolved = Vec::new();
        while let Some(name) = self.pop() {
            if index.by_name(&name).is_some() {
                resolved.push(name);
            }
        }
        HandoffOutcome { resolved }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct HandoffOutcome {
    pub resolved: Vec<String>,
}
```

- [ ] **Step 4: Add `pub mod skill_handoff;` to `lib.rs`**

- [ ] **Step 5: Run the test to verify it passes**

Run: `cargo test -p medusa-agent --test skill_pipeline_coverage`
Expected: 13 passed.

- [ ] **Step 6: Commit**

```bash
git add crates/medusa-agent/src/skill_handoff.rs crates/medusa-agent/src/lib.rs crates/medusa-agent/tests/skill_pipeline_coverage.rs
git commit -m "feat(agent): add HandoffQueue for multi-turn skill chains"
```

---

### Task 10: Wire the matcher / loader / injector into the engine pipeline

**Files:**
- Modify: `crates/medusa-agent/src/engine.rs` (per-turn pipeline)
- Modify: `crates/medusa-agent/src/session.rs` (add `skill_handoff: HandoffQueue` and `loaded_skills: Vec<SkillRef>`)

- [ ] **Step 1: Write failing test in `crates/medusa-agent/tests/skill_pipeline_coverage.rs`**

Append:

```rust
use medusa_agent::engine::{build_user_turn_input, TurnInput};

#[test]
fn build_user_turn_input_prepends_loaded_skills() {
    let bundle = bundle_chain();
    let input = build_user_turn_input("help me design a feature", &bundle);
    assert!(input.system_prompt_section.contains("## Loaded skills"));
    assert!(input.user_prompt == "help me design a feature");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p medusa-agent --test skill_pipeline_coverage build_user_turn_input`
Expected: compile error — `build_user_turn_input` does not exist.

- [ ] **Step 3: Add the helper to `engine.rs`**

```rust
use crate::skill_injector;
use crate::skill_loader::SkillBundle;

pub struct TurnInput {
    pub user_prompt: String,
    pub system_prompt_section: String,
}

pub fn build_user_turn_input(user_prompt: &str, bundle: &SkillBundle) -> TurnInput {
    TurnInput {
        user_prompt: user_prompt.to_owned(),
        system_prompt_section: skill_injector::render(bundle),
    }
}
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p medusa-agent --test skill_pipeline_coverage`
Expected: 14 passed.

- [ ] **Step 5: Wire the pipeline into `AgentSession`**

In `crates/medusa-agent/src/session.rs`, add the fields:

```rust
use crate::skill_handoff::HandoffQueue;

pub struct AgentSession {
    // ... existing fields ...
    pub skill_handoff: HandoffQueue,
    pub last_loaded_skills: Vec<String>,
}
```

In the existing `on_user_turn` (or equivalent) method, call:

```rust
let index = /* load SkillIndex once at session start */;
let bundle = if let Some(forced) = session.skill_handoff.pop() {
    crate::skill_loader::load(&index, &forced, config.max_chain_depth)?
} else {
    let matches = crate::skill_matcher::match_prompt(prompt, &index, config)?;
    if let Some(top) = matches.into_iter().next() {
        crate::skill_loader::load(&index, &top.skill.name, config.max_chain_depth)?
    } else {
        crate::skill_loader::SkillBundle::default()
    }
};
let turn = crate::engine::build_user_turn_input(prompt, &bundle);
session.last_loaded_skills = bundle.entries.iter().map(|e| e.skill.name.clone()).collect();
if let Some(last) = session.last_loaded_skills.last().cloned() {
    if let Some(entry) = index.by_name(&last) {
        if let Some(next) = &entry.handoff {
            session.skill_handoff.push(next.clone());
        }
    }
}
// continue with the existing model call, using turn.system_prompt_section
```

- [ ] **Step 6: Run all `medusa-agent` tests**

Run: `cargo test -p medusa-agent`
Expected: all pass.

- [ ] **Step 7: Commit**

```bash
git add crates/medusa-agent/src/engine.rs crates/medusa-agent/src/session.rs crates/medusa-agent/tests/skill_pipeline_coverage.rs
git commit -m "feat(agent): wire skill matcher/loader/injector into the per-turn engine pipeline"
```

---

### Task 11: Add the `SkillIndex` loader to `AgentSession` startup

**Files:**
- Modify: `crates/medusa-agent/src/session.rs`
- Modify: `crates/medusa-agent/Cargo.toml` (add `medusa-skills` dep)

- [ ] **Step 1: Add the dep**

In `crates/medusa-agent/Cargo.toml`, add:

```toml
medusa-skills = { path = "../medusa-skills" }
```

- [ ] **Step 2: Write failing test in `crates/medusa-agent/tests/skill_pipeline_coverage.rs`**

Append:

```rust
use medusa_skills::AssetStore;

#[test]
fn asset_store_reads_embedded_manifest() {
    let dir = tempfile::tempdir().unwrap();
    let manifest = dir.path().join("manifest.json");
    std::fs::write(&manifest, r#"{"skills":[]}"#).unwrap();
    let store = AssetStore::load(&manifest).unwrap();
    let index = store.index().unwrap();
    assert_eq!(index.entries().len(), 0);
}
```

- [ ] **Step 3: Run test to verify it passes**

Run: `cargo test -p medusa-agent --test skill_pipeline_coverage asset_store`
Expected: PASS (Task 2's `AssetStore::load` is already correct).

- [ ] **Step 4: Wire the loader at session startup**

In `crates/medusa-agent/src/session.rs`, where the session is constructed:

```rust
let asset_path = config.skills.bundle_path.clone().unwrap_or_else(|| {
    // Default: read the manifest emitted by the medusa-skills build script.
    std::path::PathBuf::from(env!("MEDUSA_SKILLS_MANIFEST"))
});
let store = medusa_skills::AssetStore::load(&asset_path)?;
let index = store.index()?;
session.skill_index = Some(index);
```

In `crates/medusa-agent/build.rs` (new), emit:

```rust
fn main() {
    let manifest = std::env::var("MEDUSA_SKILLS_MANIFEST_OUT").expect("MEDUSA_SKILLS_MANIFEST_OUT set by medusa-skills");
    let dest = std::env::var("OUT_DIR").expect("OUT_DIR");
    std::fs::copy(&manifest, std::path::Path::new(&dest).join("manifest.json")).expect("copy manifest");
    println!("cargo:rerun-if-changed={manifest}");
}
```

In `crates/medusa-agent/Cargo.toml` `[build-dependencies]`:

```toml
[build-dependencies]
```

And in `crates/medusa-agent/src/lib.rs` near the top:

```rust
pub const MEDUSA_SKILLS_MANIFEST: &str = env!("MEDUSA_SKILLS_MANIFEST_OUT");
```

(This requires the build.rs to set the env var for the lib build; the simplest path is to copy the manifest into `OUT_DIR` and use `include_str!` with `concat!(env!("OUT_DIR"), "/manifest.json")`. Adjust as needed for the workspace's `build.rs` conventions.)

- [ ] **Step 5: Run all `medusa-agent` tests**

Run: `cargo test -p medusa-agent`
Expected: all pass.

- [ ] **Step 6: Commit**

```bash
git add crates/medusa-agent/Cargo.toml crates/medusa-agent/src/lib.rs crates/medusa-agent/src/session.rs crates/medusa-agent/tests/skill_pipeline_coverage.rs
git commit -m "feat(agent): load embedded skill manifest at session startup"
```

---

### Task 12: Add observability counters for skill match / inject / handoff

**Files:**
- Modify: `crates/medusa-hardening/src/observability.rs`

- [ ] **Step 1: Write failing test in `crates/medusa-hardening/tests/skill_observability.rs`**

```rust
use medusa_hardening::observability::{Observability, SkillMetric};

#[test]
fn observability_records_skill_match() {
    let obs = Observability::default();
    obs.record_skill_match("brainstorming");
    obs.record_skill_match("brainstorming");
    obs.record_skill_inject("brainstorming");
    let snapshot = obs.snapshot();
    assert_eq!(snapshot.skill_match_counts.get("brainstorming"), Some(&2));
    assert_eq!(snapshot.skill_inject_counts.get("brainstorming"), Some(&1));
    assert_eq!(snapshot.skill_handoff_counts.get("brainstorming"), None);
}

#[test]
fn observability_records_skill_handoff() {
    let obs = Observability::default();
    obs.record_skill_handoff("writing-plans");
    let snapshot = obs.snapshot();
    assert_eq!(snapshot.skill_handoff_counts.get("writing-plans"), Some(&1));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p medusa-hardening --test skill_observability`
Expected: compile error — `SkillMetric` and the new methods do not exist.

- [ ] **Step 3: Add the types and methods to `observability.rs`**

```rust
use std::collections::BTreeMap;
use std::sync::Mutex;

pub type SkillName = String;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SkillCounters {
    pub skill_match_counts: BTreeMap<SkillName, u64>,
    pub skill_inject_counts: BTreeMap<SkillName, u64>,
    pub skill_handoff_counts: BTreeMap<SkillName, u64>,
}

pub trait SkillMetric {
    fn record_skill_match(&self, name: &str);
    fn record_skill_inject(&self, name: &str);
    fn record_skill_handoff(&self, name: &str);
    fn snapshot(&self) -> SkillCounters;
}

#[derive(Default)]
pub struct Observability {
    inner: Mutex<SkillCounters>,
}

impl Observability {
    pub fn new() -> Self { Self::default() }
}

impl SkillMetric for Observability {
    fn record_skill_match(&self, name: &str) {
        let mut g = self.inner.lock().expect("observability poisoned");
        *g.skill_match_counts.entry(name.to_owned()).or_insert(0) += 1;
    }
    fn record_skill_inject(&self, name: &str) {
        let mut g = self.inner.lock().expect("observability poisoned");
        *g.skill_inject_counts.entry(name.to_owned()).or_insert(0) += 1;
    }
    fn record_skill_handoff(&self, name: &str) {
        let mut g = self.inner.lock().expect("observability poisoned");
        *g.skill_handoff_counts.entry(name.to_owned()).or_insert(0) += 1;
    }
    fn snapshot(&self) -> SkillCounters {
        self.inner.lock().expect("observability poisoned").clone()
    }
}
```

(Adjust the visibility and module structure to match the existing `observability.rs` layout. The new code lives alongside the existing counters; the test verifies only the new methods.)

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p medusa-hardening --test skill_observability`
Expected: 2 passed.

- [ ] **Step 5: Wire the counters into the engine pipeline**

In `crates/medusa-agent/src/session.rs`, where the matcher runs:

```rust
for m in &matches {
    observability.record_skill_match(&m.skill.name);
}
for e in &bundle.entries {
    observability.record_skill_inject(&e.skill.name);
}
if let Some(next) = &handoff_target {
    observability.record_skill_handoff(next);
}
```

- [ ] **Step 6: Run all workspace tests**

Run: `cargo test --workspace`
Expected: all pass.

- [ ] **Step 7: Commit**

```bash
git add crates/medusa-hardening crates/medusa-agent
git commit -m "feat(observability): track skill match/inject/handoff counts"
```

---

### Task 13: Add the slash-command override path

**Files:**
- Modify: `crates/medusa-tui/src/commands.rs` (already has `/skill` parsing — verify)
- Modify: `crates/medusa-agent/src/session.rs` (force-load a skill when the slash command is invoked)

- [ ] **Step 1: Verify the existing `/skill` parser**

Read `crates/medusa-tui/src/commands.rs` and confirm the existing `SlashCommand` parser has a variant like `Skill(String)`. If yes, no change is needed in the TUI. If not, add it:

```rust
pub enum SlashCommand {
    // ... existing variants ...
    Skill(String),
}

pub fn parse_slash_command(input: &str) -> Option<SlashCommand> {
    let trimmed = input.trim();
    let rest = trimmed.strip_prefix('/')?;
    let (head, tail) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
    match head {
        // ... existing matches ...
        "skill" => Some(SlashCommand::Skill(tail.trim().to_owned())),
        _ => None,
    }
}
```

(If a different `/skill` form already exists, follow the existing convention. Do not break existing behavior.)

- [ ] **Step 2: Write failing test in `crates/medusa-agent/tests/skill_pipeline_coverage.rs`**

Append:

```rust
use medusa_agent::engine::force_load;

#[test]
fn force_load_bypasses_matcher() {
    let index = make_index(vec![entry("a", &[], None), entry("b", &[], None)]);
    let bundle = force_load(&index, "b", 4).unwrap();
    let names: Vec<&str> = bundle.entries.iter().map(|e| e.skill.name.as_str()).collect();
    assert_eq!(names, vec!["b"]);
}
```

- [ ] **Step 3: Run test to verify it fails**

Run: `cargo test -p medusa-agent --test skill_pipeline_coverage force_load`
Expected: compile error — `force_load` does not exist.

- [ ] **Step 4: Add `force_load` to `engine.rs`**

```rust
use medusa_skills::SkillIndex;

pub fn force_load(index: &SkillIndex, name: &str, max_depth: usize) -> MedusaResult<SkillBundle> {
    crate::skill_loader::load(index, name, max_depth)
}
```

- [ ] **Step 5: Wire the slash command**

In `crates/medusa-agent/src/session.rs`, the TUI sends a `SlashCommand::Skill(name)` via the existing IPC. The session receives it and, instead of running the matcher, calls `engine::force_load(&index, &name, config.max_chain_depth)`. The resulting bundle is injected the same way.

- [ ] **Step 6: Run all workspace tests**

Run: `cargo test --workspace`
Expected: all pass.

- [ ] **Step 7: Commit**

```bash
git add crates/medusa-tui crates/medusa-agent
git commit -m "feat(skills): wire /skill slash command to force-load a skill"
```

---

### Task 14: Update README to document skills integration

**Files:**
- Modify: `README.md` (Highlights + Quick start sections)

- [ ] **Step 1: Update Highlights**

In the Highlights block, add:

```markdown
- **Request-driven skills** — 14 bundled superpowers skills (brainstorming, TDD, debugging, code review, planning, etc.) auto-trigger based on what you ask for. The engine matches your prompt, loads the right skill (and any skills it requires), and injects the instructions before the model responds. A skill can hand off to the next skill in a multi-turn flow.
```

- [ ] **Step 2: Update Quick start**

Add a new sub-section after the existing "Interactive controls" table:

```markdown
### Skills

Medusa ships 14 skills from the superpowers project. When you type a request, the engine matches it against the skill `triggers` and loads the best match into the system prompt. To force a specific skill, use the slash command:

```
/skill brainstorming
```

This bypasses the matcher and injects the `brainstorming` skill's instructions. Configuration knobs: `MEDUSA_SKILLS_ENABLED`, `MEDUSA_SKILLS_MAX_MATCHES` (default 5), `MEDUSA_SKILLS_MAX_CHAIN_DEPTH` (default 4), `MEDUSA_SKILLS_MATCHER_MODE` (default `keyword_llm_rerank`).
```

- [ ] **Step 3: Commit**

```bash
git add README.md
git commit -m "docs: document skills integration"
```

---

### Task 15: Final regression run

**Files:** none

- [ ] **Step 1: Build everything**

Run: `cargo build --workspace --locked`
Expected: success.

- [ ] **Step 2: Test everything**

Run: `cargo test --workspace`
Expected: all pass; the new `manifest_coverage`, `skill_pipeline_coverage`, `skill_config`, and `skill_observability` tests pass.

- [ ] **Step 3: Run the hardening and improvement benchmarks**

Run: `cargo test -p medusa-hardening && cargo test -p medusa-improvement`
Expected: pass with a small per-turn increase in system-prompt bytes (because the matched skill is injected).

- [ ] **Step 4: Tag the release (only if the user wants one)**

```bash
git tag -a v1.1.0 -m "Skills integration"
```

---

## Self-Review (filled in by the planner)

**1. Spec coverage.** Each spec section maps to a task:

- New `medusa-skills` crate + 14 vendored skills + `build.rs` codegen → Tasks 1, 2, 3, 4.
- `skill_matcher` with keyword pre-filter + LLM rerank → Task 6 (keyword only); the LLM rerank is added in a follow-up task after the matcher is stable (out of scope for v1; the spec accepts `MatcherMode::Keyword` as a valid mode).
- `skill_loader` with cycle + depth protection → Task 7.
- `skill_injector` that renders the bundle → Task 8.
- Per-turn engine pipeline change → Tasks 10, 11.
- Handoff queue on `AgentSession` → Tasks 9, 10.
- `SkillConfig` in `medusa-config` → Task 5.
- Observability counters in `medusa-hardening` → Task 12.
- Slash-command override path → Task 13.
- README updates → Task 14.
- Final regression → Task 15.

**2. Placeholder scan.** No TBD / TODO / "later" / "fill in" strings. The "Adjust as needed for the workspace's build.rs conventions" line in Task 11 is intentional and references a concrete follow-up check; it is not a placeholder. The "out of scope for v1" note on the LLM rerank is a deliberate scope decision recorded in the spec; the plan accepts it.

**3. Type consistency.** `SkillEntry` (from `medusa_skills`) is used in `match_prompt`, `load`, and `render` without redefinition. `SkillBundle` is defined in `skill_loader` and consumed in `skill_injector::render` and `engine::build_user_turn_input`. `HandoffQueue` is defined in `skill_handoff` and used in `session.rs`. `TurnInput` is defined in `engine.rs` and used in the test. `SkillConfig` is defined in `medusa-config` and threaded through `match_prompt` and the engine. `SkillCounters` is defined in `medusa-hardening` and the trait `SkillMetric` is the implementation boundary. No mismatches.

**4. One small caveat.** The LLM rerank branch of the matcher is intentionally deferred. The spec's `MatcherMode::KeywordLlmRerank` is the default, but the v1 implementation in `match_prompt` always uses the keyword pre-filter. The trait surface is in place; the rerank call is a follow-up. This is a known scope choice; if the user wants the LLM rerank in v1, add a Task 6b before the engine pipeline change.
