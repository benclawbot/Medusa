use std::{
    collections::BTreeMap,
    fs,
    path::{Component, Path},
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};

const ACTIVE_ROOT: &str = ".medusa/skills";
const PROBATION_PATH: &str = ".medusa/learning/skill-probation/summary.json";
const GRADUATION_ROOT: &str = ".medusa/learning/skill-graduations";
const LIFECYCLE_FILE: &str = "lifecycle.json";

#[derive(Clone, Debug, Deserialize, Serialize)]
struct ProbationSummary {
    #[serde(default)]
    skills: BTreeMap<String, ProbationReport>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct ProbationReport {
    state: String,
    baseline_observed_sessions: usize,
    baseline_verification_rate_milli: u16,
    baseline_confidence_milli: u16,
    post_restore_sessions: usize,
    post_restore_verified_sessions: usize,
    post_restore_verification_rate_milli: u16,
    post_restore_confidence_milli: u16,
    verification_rate_change_milli: i32,
    remaining_samples: usize,
    restored_at_epoch_seconds: Option<u64>,
    #[serde(default)]
    dependency_graph_sha256: Option<String>,
    latest_recorded_at: String,
    recommendation: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct LifecycleRecord {
    schema_version: u8,
    skill: String,
    status: String,
    original_path: String,
    quarantine_path: String,
    quarantined_at_epoch_seconds: u64,
    quarantine_reason: String,
    recommendation: serde_json::Value,
    restored_at_epoch_seconds: Option<u64>,
    #[serde(default)]
    graduated_at_epoch_seconds: Option<u64>,
}

#[derive(Debug, Serialize)]
struct GraduationReceipt {
    schema_version: u8,
    skill: String,
    graduated_at_epoch_seconds: u64,
    lifecycle_path: String,
    probation: ProbationReport,
}

pub(super) fn try_run(root: &Path, args: &[String]) -> Option<Result<(), String>> {
    (args.first().map(String::as_str) == Some("graduate")).then(|| graduate(root, &args[1..]))
}

pub(super) fn usage_line() -> &'static str {
    "  medusa [--repo PATH] skills graduate NAME --confirm"
}

fn graduate(root: &Path, args: &[String]) -> Result<(), String> {
    let (name, confirmed) = parse_args(args)?;
    validate_name(name)?;
    if !confirmed {
        return Err(format!(
            "graduation requires explicit approval; rerun with `skills graduate {name} --confirm`"
        ));
    }

    let probation_path = root.join(PROBATION_PATH);
    let mut summary: ProbationSummary = read_json(&probation_path)?;
    let report = summary
        .skills
        .get(name)
        .cloned()
        .ok_or_else(|| format!("restored skill `{name}` has no probation report"))?;
    if report.state != "passed" {
        return Err(format!(
            "skill `{name}` cannot graduate from probation state `{}`; only `passed` is eligible",
            report.state
        ));
    }

    let active_root = root.join(ACTIVE_ROOT);
    medusa_runtime::skill_dependencies::resolve_project_skill(&active_root, name, 64_000)?;
    verify_probation_digest(&active_root, name, &report)?;

    let lifecycle_path = active_root.join(name).join(LIFECYCLE_FILE);
    let mut lifecycle: LifecycleRecord = read_json(&lifecycle_path)?;
    if lifecycle.skill != name || lifecycle.status != "restored" {
        return Err(format!(
            "invalid restored lifecycle record for `{name}` (skill={}, status={})",
            lifecycle.skill, lifecycle.status
        ));
    }

    let graduated_at = now_epoch_seconds()?;
    lifecycle.status = "graduated".to_owned();
    lifecycle.graduated_at_epoch_seconds = Some(graduated_at);
    write_json(&lifecycle_path, &lifecycle)?;

    let receipt_path = root.join(GRADUATION_ROOT).join(format!("{name}.json"));
    if receipt_path.exists() {
        rollback_lifecycle(&lifecycle_path, &mut lifecycle);
        return Err(format!(
            "graduation receipt already exists; refusing overwrite: {}",
            receipt_path.display()
        ));
    }
    if let Some(parent) = receipt_path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("create {}: {error}", parent.display()))?;
    }
    let receipt = GraduationReceipt {
        schema_version: 1,
        skill: name.to_owned(),
        graduated_at_epoch_seconds: graduated_at,
        lifecycle_path: format!("{ACTIVE_ROOT}/{name}/{LIFECYCLE_FILE}"),
        probation: report,
    };
    if let Err(error) = write_json(&receipt_path, &receipt) {
        lifecycle.status = "restored".to_owned();
        lifecycle.graduated_at_epoch_seconds = None;
        let rollback = write_json(&lifecycle_path, &lifecycle);
        return Err(match rollback {
            Ok(()) => format!("write graduation receipt: {error}; lifecycle rolled back"),
            Err(rollback_error) => format!(
                "write graduation receipt: {error}; lifecycle rollback also failed: {rollback_error}"
            ),
        });
    }

    summary.skills.remove(name);
    write_json(&probation_path, &summary)?;
    println!(
        "Graduated `{name}` from probation. Receipt: {}",
        receipt_path.display()
    );
    Ok(())
}

fn verify_probation_digest(
    active_root: &Path,
    name: &str,
    report: &ProbationReport,
) -> Result<(), String> {
    let Some(expected) = report.dependency_graph_sha256.as_deref() else {
        return Ok(());
    };
    let current = medusa_runtime::skill_dependency_locks::verify_dependency_lock(active_root, name)?;
    if current.graph_sha256 != expected {
        return Err(format!(
            "skill `{name}` dependency graph drifted during probation: expected {expected}, found {}",
            current.graph_sha256
        ));
    }
    Ok(())
}

fn rollback_lifecycle(path: &Path, lifecycle: &mut LifecycleRecord) {
    lifecycle.status = "restored".to_owned();
    lifecycle.graduated_at_epoch_seconds = None;
    let _ = write_json(path, lifecycle);
}

fn parse_args(args: &[String]) -> Result<(&str, bool), String> {
    match args {
        [name] => Ok((name, false)),
        [name, flag] if flag == "--confirm" => Ok((name, true)),
        _ => Err(usage()),
    }
}

fn validate_name(name: &str) -> Result<(), String> {
    let path = Path::new(name);
    if name.is_empty()
        || path.components().count() != 1
        || !matches!(path.components().next(), Some(Component::Normal(_)))
        || name == "."
        || name == ".."
    {
        return Err(format!("invalid skill name `{name}`"));
    }
    Ok(())
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T, String> {
    let bytes = fs::read(path).map_err(|error| format!("read {}: {error}", path.display()))?;
    serde_json::from_slice(&bytes).map_err(|error| format!("parse {}: {error}", path.display()))
}

fn write_json(path: &Path, value: &impl Serialize) -> Result<(), String> {
    let temporary = path.with_extension("json.tmp");
    let content = serde_json::to_vec_pretty(value)
        .map_err(|error| format!("serialize {}: {error}", path.display()))?;
    fs::write(&temporary, content)
        .map_err(|error| format!("write {}: {error}", temporary.display()))?;
    fs::rename(&temporary, path).map_err(|error| {
        format!(
            "replace {} with {}: {error}",
            path.display(),
            temporary.display()
        )
    })
}

fn now_epoch_seconds() -> Result<u64, String> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .map_err(|error| format!("resolve graduation timestamp: {error}"))
}

fn usage() -> String {
    format!("Usage:\n{}", usage_line())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(root: &Path, state: &str, digest: Option<&str>) {
        let skill = root.join(ACTIVE_ROOT).join("verify");
        fs::create_dir_all(&skill).expect("skill directory");
        fs::write(skill.join("SKILL.md"), "# Verify\n").expect("skill");
        fs::write(
            skill.join(LIFECYCLE_FILE),
            serde_json::to_vec_pretty(&serde_json::json!({
                "schema_version": 1,
                "skill": "verify",
                "status": "restored",
                "original_path": ".medusa/skills/verify",
                "quarantine_path": ".medusa/learning/skill-quarantine/verify",
                "quarantined_at_epoch_seconds": 1,
                "quarantine_reason": "weak",
                "recommendation": {},
                "restored_at_epoch_seconds": 2
            }))
            .expect("lifecycle json"),
        )
        .expect("lifecycle");
        let probation = root.join(PROBATION_PATH);
        fs::create_dir_all(probation.parent().expect("probation parent")).expect("probation dir");
        fs::write(
            probation,
            serde_json::to_vec_pretty(&serde_json::json!({
                "skills": {"verify": {
                    "state": state,
                    "baseline_observed_sessions": 5,
                    "baseline_verification_rate_milli": 200,
                    "baseline_confidence_milli": 333,
                    "post_restore_sessions": 3,
                    "post_restore_verified_sessions": 3,
                    "post_restore_verification_rate_milli": 1000,
                    "post_restore_confidence_milli": 714,
                    "verification_rate_change_milli": 800,
                    "remaining_samples": 0,
                    "restored_at_epoch_seconds": 2,
                    "dependency_graph_sha256": digest,
                    "latest_recorded_at": "2026-07-21T16:00:00Z",
                    "recommendation": "remain active"
                }}
            }))
            .expect("probation json"),
        )
        .expect("probation");
    }

    #[test]
    fn graduation_requires_passed_state_and_confirmation() {
        let repo = tempfile::tempdir().expect("repo");
        fixture(repo.path(), "watch", None);
        assert!(graduate(repo.path(), &["verify".to_owned(), "--confirm".to_owned()]).is_err());
        fixture(repo.path(), "passed", None);
        assert!(graduate(repo.path(), &["verify".to_owned()]).is_err());
    }

    #[test]
    fn passed_legacy_skill_graduates_with_receipt() {
        let repo = tempfile::tempdir().expect("repo");
        fixture(repo.path(), "passed", None);
        graduate(repo.path(), &["verify".to_owned(), "--confirm".to_owned()]).expect("graduate");
        let lifecycle: serde_json::Value = serde_json::from_slice(
            &fs::read(repo.path().join(ACTIVE_ROOT).join("verify/lifecycle.json"))
                .expect("lifecycle"),
        )
        .expect("lifecycle json");
        assert_eq!(lifecycle["status"], "graduated");
        assert!(root_receipt(repo.path()).is_file());
    }

    #[test]
    fn probation_digest_requires_a_current_lock() {
        let repo = tempfile::tempdir().expect("repo");
        fixture(repo.path(), "passed", Some(&"a".repeat(64)));
        let error = graduate(repo.path(), &["verify".to_owned(), "--confirm".to_owned()])
            .expect_err("missing lock must block graduation");
        assert!(error.contains("dependency lock is missing"));
    }

    #[test]
    fn names_cannot_escape_skill_root() {
        assert!(validate_name("../escape").is_err());
        assert!(validate_name("nested/name").is_err());
    }

    fn root_receipt(root: &Path) -> std::path::PathBuf {
        root.join(GRADUATION_ROOT).join("verify.json")
    }
}
