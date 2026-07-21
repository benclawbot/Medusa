use std::{
    collections::BTreeSet,
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

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct SkillDraftManifest {
    schema_version: u8,
    name: String,
    status: String,
    source_lesson_id: String,
    source_session_id: String,
    repository_fingerprint: String,
    confidence_milli: u16,
    proposed_install_path: String,
    skill_file: String,
    requires_approval: bool,
    #[serde(default)]
    revision: u32,
    #[serde(default)]
    source_lesson_ids: Vec<String>,
    #[serde(default)]
    source_session_ids: Vec<String>,
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
    let manifest_path = directory.join("manifest.json");
    if skill_path.is_file() && manifest_path.is_file() {
        refine_existing(&directory, &lesson, &name)?;
    } else {
        create_new(&directory, &lesson, &name)?;
    }
    Ok(Some(directory))
}

fn create_new(directory: &Path, lesson: &LessonProposal, name: &str) -> MedusaResult<()> {
    atomic_write(
        &directory.join("SKILL.md"),
        render_skill(
            lesson,
            name,
            &lesson.procedure,
            &lesson.evidence,
            &lesson.tools,
        )
        .as_bytes(),
    )?;
    let manifest = SkillDraftManifest {
        schema_version: 2,
        name: name.to_owned(),
        status: "proposed".to_owned(),
        source_lesson_id: lesson.id.clone(),
        source_session_id: lesson.source_session_id.clone(),
        repository_fingerprint: lesson.repository_fingerprint.clone(),
        confidence_milli: lesson.confidence_milli,
        proposed_install_path: format!(".medusa/skills/{name}/SKILL.md"),
        skill_file: "SKILL.md".to_owned(),
        requires_approval: true,
        revision: 1,
        source_lesson_ids: vec![lesson.id.clone()],
        source_session_ids: vec![lesson.source_session_id.clone()],
    };
    write_manifest(directory, &manifest)
}

fn refine_existing(directory: &Path, lesson: &LessonProposal, name: &str) -> MedusaResult<()> {
    let manifest_path = directory.join("manifest.json");
    let mut manifest: SkillDraftManifest = serde_json::from_slice(&fs::read(&manifest_path)?)?;
    if manifest.status != "proposed"
        || manifest.repository_fingerprint != lesson.repository_fingerprint
        || manifest.source_lesson_ids.iter().any(|id| id == &lesson.id)
    {
        return Ok(());
    }

    let skill_path = directory.join("SKILL.md");
    let previous = fs::read_to_string(&skill_path)?;
    let revision = manifest.revision.max(1);
    let revision_dir = directory.join("revisions");
    fs::create_dir_all(&revision_dir)?;
    atomic_write(
        &revision_dir.join(format!("{revision:04}.md")),
        previous.as_bytes(),
    )?;

    let procedure = merge_items(
        section_items(&previous, "## Procedure"),
        lesson.procedure.iter().filter(|value| safe_text(value)),
    );
    let evidence = merge_items(
        section_items(&previous, "## Verification"),
        lesson.evidence.iter().filter(|value| safe_text(value)),
    );
    let tools = merge_items(
        context_tools(&previous),
        lesson.tools.iter().filter(|value| safe_text(value)),
    );
    atomic_write(
        &skill_path,
        render_skill(lesson, name, &procedure, &evidence, &tools).as_bytes(),
    )?;

    manifest.schema_version = 2;
    manifest.source_lesson_id = lesson.id.clone();
    manifest.source_session_id = lesson.source_session_id.clone();
    manifest.confidence_milli = manifest
        .confidence_milli
        .max(lesson.confidence_milli)
        .min(1_000);
    manifest.revision = revision.saturating_add(1);
    push_unique(&mut manifest.source_lesson_ids, lesson.id.clone());
    push_unique(
        &mut manifest.source_session_ids,
        lesson.source_session_id.clone(),
    );
    manifest.requires_approval = true;
    write_manifest(directory, &manifest)
}

fn write_manifest(directory: &Path, manifest: &SkillDraftManifest) -> MedusaResult<()> {
    atomic_write(
        &directory.join("manifest.json"),
        &serde_json::to_vec_pretty(manifest)?,
    )
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

fn render_skill(
    lesson: &LessonProposal,
    name: &str,
    procedure: &[String],
    evidence: &[String],
    tools: &[String],
) -> String {
    let mut output = format!(
        "---\nname: {name}\ndescription: {}\nstatus: proposed\nsource_session: {}\nconfidence_milli: {}\n---\n\n# {}\n\n{}\n\n## Procedure\n",
        yaml_value(&lesson.summary),
        yaml_value(&lesson.source_session_id),
        lesson.confidence_milli,
        heading(&lesson.title),
        lesson.summary.trim(),
    );
    append_items(&mut output, procedure);
    output.push_str("\n## Verification\n");
    append_items(&mut output, evidence);
    output.push_str("\n## Context\n");
    output.push_str(&format!("- Lesson type: {}\n", lesson.kind));
    if !tools.is_empty() {
        output.push_str(&format!("- Observed tools: {}\n", tools.join(", ")));
    }
    output.push_str("- This draft is inactive until explicitly reviewed and installed.\n");
    output
}

fn append_items(output: &mut String, items: &[String]) {
    for item in items
        .iter()
        .filter(|value| safe_text(value))
        .take(MAX_SECTION_ITEMS)
    {
        output.push_str("- ");
        output.push_str(item.trim());
        output.push('\n');
    }
}

fn section_items(content: &str, heading: &str) -> Vec<String> {
    let Some((_, remainder)) = content.split_once(heading) else {
        return Vec::new();
    };
    remainder
        .lines()
        .skip(1)
        .take_while(|line| !line.starts_with("## "))
        .filter_map(|line| line.strip_prefix("- "))
        .map(str::trim)
        .filter(|value| safe_text(value))
        .map(str::to_owned)
        .collect()
}

fn context_tools(content: &str) -> Vec<String> {
    content
        .lines()
        .find_map(|line| line.strip_prefix("- Observed tools: "))
        .map(|value| {
            value
                .split(',')
                .map(str::trim)
                .filter(|item| safe_text(item))
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn merge_items<'a>(
    current: Vec<String>,
    incoming: impl IntoIterator<Item = &'a String>,
) -> Vec<String> {
    let mut seen = BTreeSet::new();
    current
        .into_iter()
        .chain(incoming.into_iter().cloned())
        .filter(|item| safe_text(item))
        .filter(|item| seen.insert(normalized(item)))
        .take(MAX_SECTION_ITEMS)
        .collect()
}

fn push_unique(items: &mut Vec<String>, value: String) {
    if !items.iter().any(|item| item == &value) {
        items.push(value);
    }
}

fn normalized(value: &str) -> String {
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
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

    fn lesson(directory: &Path, id: &str, confidence: u16, extra_step: &str) -> PathBuf {
        let path = directory.join(format!(".medusa/learning/proposals/{id}.json"));
        fs::create_dir_all(path.parent().expect("parent")).expect("directory");
        fs::write(
            &path,
            serde_json::to_vec_pretty(&json!({
                "id": id,
                "source_session_id": format!("ses-{id}"),
                "created_at": "2026-07-21T00:00:00Z",
                "repository_fingerprint": format!("path:{}", directory.display()),
                "kind": "verification",
                "title": "Reusable workflow: Verify workspace package",
                "summary": "Run the verified workspace and package checks.",
                "procedure": ["Run cargo test --workspace", extra_step],
                "evidence": ["Workspace tests passed", format!("Evidence {id}")],
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
        let draft = create_from_lesson(&lesson(directory.path(), "lesson-1", 900, "Build package"))
            .expect("create")
            .expect("draft");
        let manifest: SkillDraftManifest =
            serde_json::from_slice(&fs::read(draft.join("manifest.json")).expect("manifest"))
                .expect("manifest json");
        assert_eq!(manifest.revision, 1);
        assert!(manifest.requires_approval);
    }

    #[test]
    fn later_lesson_refines_draft_and_preserves_previous_revision() {
        let directory = tempfile::tempdir().expect("tempdir");
        let first = lesson(directory.path(), "lesson-1", 800, "Build package");
        let draft = create_from_lesson(&first).expect("create").expect("draft");
        let second = lesson(directory.path(), "lesson-2", 950, "Run package smoke");
        create_from_lesson(&second).expect("refine");

        let skill = fs::read_to_string(draft.join("SKILL.md")).expect("skill");
        let manifest: SkillDraftManifest =
            serde_json::from_slice(&fs::read(draft.join("manifest.json")).expect("manifest"))
                .expect("manifest json");
        assert!(skill.contains("Build package"));
        assert!(skill.contains("Run package smoke"));
        assert!(draft.join("revisions/0001.md").is_file());
        assert_eq!(manifest.revision, 2);
        assert_eq!(manifest.confidence_milli, 950);
        assert_eq!(manifest.source_lesson_ids.len(), 2);
        assert!(manifest.requires_approval);
    }

    #[test]
    fn duplicate_lesson_is_idempotent() {
        let directory = tempfile::tempdir().expect("tempdir");
        let source = lesson(directory.path(), "lesson-1", 900, "Build package");
        let draft = create_from_lesson(&source).expect("create").expect("draft");
        create_from_lesson(&source).expect("repeat");
        let manifest: SkillDraftManifest =
            serde_json::from_slice(&fs::read(draft.join("manifest.json")).expect("manifest"))
                .expect("manifest json");
        assert_eq!(manifest.revision, 1);
        assert!(!draft.join("revisions").exists());
    }

    #[test]
    fn low_confidence_lesson_does_not_create_skill() {
        let directory = tempfile::tempdir().expect("tempdir");
        assert!(
            create_from_lesson(&lesson(
                directory.path(),
                "lesson-low",
                699,
                "Build package"
            ))
            .expect("create")
            .is_none()
        );
    }

    #[test]
    fn generated_skill_is_outside_active_skill_root() {
        let directory = tempfile::tempdir().expect("tempdir");
        let draft = create_from_lesson(&lesson(directory.path(), "lesson-1", 900, "Build package"))
            .expect("create")
            .expect("draft");
        assert!(draft.starts_with(directory.path().join(".medusa/learning/skill-proposals")));
        assert!(!directory.path().join(".medusa/skills").exists());
    }
}
