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
