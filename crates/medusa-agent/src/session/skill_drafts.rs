use std::{
    fs,
    path::{Path, PathBuf},
};

use medusa_core::MedusaResult;
use serde::{Deserialize, Serialize};

const MINIMUM_CONFIDENCE_MILLI: u16 = 700;
const MAX_SECTION_ITEMS: usize = 12;

#[derive(Clone, Debug, Deserialize)]
struct LessonProposal {
    id: String,
    source_session_id: String,
    repository_fingerprint: String,
    kind: String,
    title: String,
    summary: String,
    procedure: Vec<String>,
    evidence: Vec<String>,
    tools: Vec<String>,
    confidence_milli: u16,
    status: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct SkillDraftManifest {
    schema_version: u8,
    name: String,
    status: &'static str,
    source_lesson_id: String,
    source_session_id: String,
    repository_fingerprint: String,
    confidence_milli: u16,
    proposed_install_path: String,
    skill_file: String,
    requires_approval: bool,
}

pub(super) fn create_from_lesson(lesson_path: &Path) -> MedusaResult<Option<PathBuf>> {
    let lesson: LessonProposal = serde_json::from_slice(&fs::read(lesson_path)?)?;
    let Some(name) = eligible_name(&lesson) else {
        return Ok(None);
    };

    let repo = lesson_path
        .ancestors()
        .nth(4)
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let directory = repo.join(".medusa/learning/skill-proposals").join(&name);
    fs::create_dir_all(&directory)?;

    let skill_path = directory.join("SKILL.md");
    atomic_write(&skill_path, render_skill(&lesson, &name).as_bytes())?;

    let manifest = SkillDraftManifest {
        schema_version: 1,
        name: name.clone(),
        status: "proposed",
        source_lesson_id: lesson.id,
        source_session_id: lesson.source_session_id,
        repository_fingerprint: lesson.repository_fingerprint,
        confidence_milli: lesson.confidence_milli,
        proposed_install_path: format!(".medusa/skills/{name}/SKILL.md"),
        skill_file: "SKILL.md".to_owned(),
        requires_approval: true,
    };
    atomic_write(
        &directory.join("manifest.json"),
        &serde_json::to_vec_pretty(&manifest)?,
    )?;
    Ok(Some(directory))
}

fn eligible_name(lesson: &LessonProposal) -> Option<String> {
    if lesson.status != "proposed"
        || lesson.confidence_milli < MINIMUM_CONFIDENCE_MILLI
        || lesson.procedure.is_empty()
        || lesson.evidence.is_empty()
        || !safe_text(&lesson.title)
        || !safe_text(&lesson.summary)
    {
        return None;
    }
    let name = slug(&lesson.title);
    (!name.is_empty()).then_some(name)
}

fn render_skill(lesson: &LessonProposal, name: &str) -> String {
    let mut output = format!(
        "---\nname: {name}\ndescription: {}\nstatus: proposed\nsource_session: {}\nconfidence_milli: {}\n---\n\n# {}\n\n{}\n\n## Procedure\n",
        yaml_value(&lesson.summary),
        yaml_value(&lesson.source_session_id),
        lesson.confidence_milli,
        heading(&lesson.title),
        lesson.summary.trim(),
    );
    for step in lesson
        .procedure
        .iter()
        .filter(|value| safe_text(value))
        .take(MAX_SECTION_ITEMS)
    {
        output.push_str("- ");
        output.push_str(step.trim());
        output.push('\n');
    }

    output.push_str("\n## Verification\n");
    for evidence in lesson
        .evidence
        .iter()
        .filter(|value| safe_text(value))
        .take(MAX_SECTION_ITEMS)
    {
        output.push_str("- ");
        output.push_str(evidence.trim());
        output.push('\n');
    }

    output.push_str("\n## Context\n");
    output.push_str(&format!("- Lesson type: {}\n", lesson.kind));
    if !lesson.tools.is_empty() {
        output.push_str(&format!("- Observed tools: {}\n", lesson.tools.join(", ")));
    }
    output.push_str("- This draft is inactive until explicitly reviewed and installed.\n");
    output
}

fn atomic_write(path: &Path, content: &[u8]) -> MedusaResult<()> {
    let temporary = path.with_extension("tmp");
    fs::write(&temporary, content)?;
    fs::rename(temporary, path)?;
    Ok(())
}

fn slug(value: &str) -> String {
    let mut output = String::new();
    let mut separator = false;
    for character in value.chars().flat_map(char::to_lowercase) {
        if character.is_ascii_alphanumeric() {
            output.push(character);
            separator = false;
        } else if !output.is_empty() && !separator {
            output.push('-');
            separator = true;
        }
    }
    output.trim_matches('-').chars().take(64).collect()
}

fn heading(value: &str) -> String {
    value
        .replace(['\n', '\r', '#'], " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn yaml_value(value: &str) -> String {
    format!(
        "\"{}\"",
        value
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace(['\n', '\r'], " ")
    )
}

fn safe_text(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    !value.trim().is_empty()
        && ![
            "api_key",
            "apikey",
            "authorization:",
            "bearer ",
            "secret=",
            "token=",
        ]
        .iter()
        .any(|marker| lower.contains(marker))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn lesson(directory: &Path, confidence: u16) -> PathBuf {
        let path = directory.join(".medusa/learning/proposals/lesson.json");
        fs::create_dir_all(path.parent().expect("parent")).expect("directory");
        fs::write(
            &path,
            serde_json::to_vec_pretty(&json!({
                "id": "lesson-1",
                "source_session_id": "ses-1",
                "created_at": "2026-07-21T00:00:00Z",
                "repository_fingerprint": format!("path:{}", directory.display()),
                "kind": "verification",
                "title": "Reusable workflow: Verify workspace package",
                "summary": "Run the verified workspace and package checks.",
                "procedure": ["Run cargo test --workspace", "Build the release package"],
                "evidence": ["Workspace tests passed", "Package smoke passed"],
                "tools": ["shell"],
                "confidence_milli": confidence,
                "status": "proposed"
            }))
            .expect("json"),
        )
        .expect("lesson");
        path
    }

    #[test]
    fn verified_lesson_creates_inactive_skill_draft() {
        let directory = tempfile::tempdir().expect("tempdir");
        let draft = create_from_lesson(&lesson(directory.path(), 900))
            .expect("create")
            .expect("draft");
        let skill = fs::read_to_string(draft.join("SKILL.md")).expect("skill");
        let manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(draft.join("manifest.json")).expect("manifest"))
                .expect("manifest json");
        assert!(skill.contains("status: proposed"));
        assert!(skill.contains("cargo test --workspace"));
        assert_eq!(manifest["requires_approval"], true);
        assert!(
            manifest["proposed_install_path"]
                .as_str()
                .is_some_and(|path| path.starts_with(".medusa/skills/"))
        );
    }

    #[test]
    fn low_confidence_lesson_does_not_create_skill() {
        let directory = tempfile::tempdir().expect("tempdir");
        assert!(
            create_from_lesson(&lesson(directory.path(), 699))
                .expect("create")
                .is_none()
        );
    }

    #[test]
    fn generated_skill_is_outside_active_skill_root() {
        let directory = tempfile::tempdir().expect("tempdir");
        let draft = create_from_lesson(&lesson(directory.path(), 900))
            .expect("create")
            .expect("draft");
        assert!(draft.starts_with(directory.path().join(".medusa/learning/skill-proposals")));
        assert!(!directory.path().join(".medusa/skills").exists());
    }
}
