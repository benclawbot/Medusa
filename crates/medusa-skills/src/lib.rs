//! Bundled superpowers skills. Each skill is a Markdown file under
//! `assets/skills/<name>/SKILL.md`; the build script compiles them into
//! `assets/manifest.json` at build time.

pub mod asset;
pub mod index;

pub use asset::AssetStore;
pub use index::{SkillEntry, SkillIndex};
