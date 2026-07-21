use std::{
    fs,
    path::{Component, Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use serde_json::Value;

const ACTIVE_ROOT: &str = ".medusa/skills";
const QUARANTINE_ROOT: &str = ".medusa/learning/skill-quarantine";
const REVIEW_PATH: &str = ".medusa/learning/skill-reviews/recommendations.json";
const LIFECYCLE_FILE: &str = "lifecycle.json";

#[derive(Clone, Debug, Deserialize, Serialize)]
struct ReviewRecommendation {
    skill: String,
    observed_sessions: usize,
    verification_rate_milli: u16,
    confidence_milli: u16,
    reason: String,
    latest_recorded_at: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct ReviewSummary {
    #[serde(default = "schema_one")]
    schema_version: u8,
    #[serde(default)]
    recommendations: Vec<ReviewRecommendation>,
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
    recommendation: ReviewRecommendation,
    restored_at_epoch_seconds: Option<u64>,
}

pub(super) fn try_run(root: &Path, args: &[String]) -> Option<Result<(), String>> {
    let command = args.first().map(String::as_str)?;
    match command {
        "reviews" => Some(reviews(root, &args[1..])),
        "quarantine" => Some(quarantine(root, &args[1..])),
        "restore" => Some(restore(root, &args[1..])),
        _ => None,
    }
}

pub(super) fn usage_lines() -> &'static str {
    "  medusa [--repo PATH] skills reviews [--json]\n  medusa [--repo PATH] skills quarantine NAME --confirm [--reason TEXT]\n  medusa [--repo PATH] skills restore NAME --confirm"
}

fn reviews(root: &Path, args: &[String]) -> Result<(), String> {
    let json_output = match args {
        [] => false,
        [flag] if flag == "--json" => true,
        _ => return Err(usage()),
    };
    let summary = read_review_summary(root)?;
    if json_output {
        println!(
            "{}",
            serde_json::to_string_pretty(&summary)
                .map_err(|error| format!("serialize skill reviews: {error}"))?
        );
        return Ok(());
    }
    if summary.recommendations.is_empty() {
        println!("No approved skills currently require lifecycle review.");
        return Ok(());
    }
    println!("skill\tsessions\trate\tconfidence\treason");
    for recommendation in summary.recommendations {
        println!(
            "{}\t{}\t{:.1}%\t{:.1}%\t{}",
            recommendation.skill,
            recommendation.observed_sessions,
            f64::from(recommendation.verification_rate_milli) / 10.0,
            f64::from(recommendation.confidence_milli) / 10.0,
            recommendation.reason
        );
    }
    Ok(())
}

fn quarantine(root: &Path, args: &[String]) -> Result<(), String> {
    let parsed = parse_quarantine_args(args)?;
    validate_name(&parsed.name)?;
    if !parsed.confirmed {
        return Err(format!(
            "quarantine requires explicit approval; rerun with `skills quarantine {} --confirm`",
            parsed.name
        ));
    }
    let recommendation = recommendation_for(root, &parsed.name)?;
    let active = root.join(ACTIVE_ROOT).join(&parsed.name);
    let skill_file = active.join("SKILL.md");
    if !skill_file.is_file() {
        return Err(format!("active skill not found: {}", skill_file.display()));
    }
    let quarantined = root.join(QUARANTINE_ROOT).join(&parsed.name);
    if quarantined.exists() {
        return Err(format!(
            "quarantine destination already exists; refusing overwrite: {}",
            quarantined.display()
        ));
    }
    let parent = quarantined
        .parent()
        .ok_or_else(|| "quarantine destination has no parent".to_owned())?;
    fs::create_dir_all(parent)
        .map_err(|error| format!("create {}: {error}", parent.display()))?;
    fs::rename(&active, &quarantined).map_err(|error| {
        format!(
            "move active skill {} to {}: {error}",
            active.display(),
            quarantined.display()
        )
    })?;
    let record = LifecycleRecord {
        schema_version: 1,
        skill: parsed.name.clone(),
        status: "quarantined".to_owned(),
        original_path: format!("{ACTIVE_ROOT}/{}", parsed.name),
        quarantine_path: format!("{QUARANTINE_ROOT}/{}", parsed.name),
        quarantined_at_epoch_seconds: now_epoch_seconds()?,
        quarantine_reason: parsed
            .reason
            .unwrap_or_else(|| recommendation.reason.clone()),
        recommendation,
        restored_at_epoch_seconds: None,
    };
    if let Err(error) = write_json(&quarantined.join(LIFECYCLE_FILE), &record) {
        let rollback = fs::rename(&quarantined, &active);
        return Err(match rollback {
            Ok(()) => format!("write quarantine lifecycle record: {error}; move rolled back"),
            Err(rollback_error) => format!(
                "write quarantine lifecycle record: {error}; rollback also failed: {rollback_error}"
            ),
        });
    }
    println!(
        "Quarantined `{}` at {}. It will no longer be loaded automatically.",
        parsed.name,
        quarantined.display()
    );
    Ok(())
}

fn restore(root: &Path, args: &[String]) -> Result<(), String> {
    let (name, confirmed) = parse_restore_args(args)?;
    validate_name(name)?;
    if !confirmed {
        return Err(format!(
            "restore requires explicit approval; rerun with `skills restore {name} --confirm`"
        ));
    }
    let quarantined = root.join(QUARANTINE_ROOT).join(name);
    let lifecycle_path = quarantined.join(LIFECYCLE_FILE);
    let mut record: LifecycleRecord = read_json(&lifecycle_path)?;
    if record.skill != name || record.status != "quarantined" {
        return Err(format!(
            "invalid quarantine lifecycle record for `{name}` (skill={}, status={})",
            record.skill, record.status
        ));
    }
    let active = root.join(ACTIVE_ROOT).join(name);
    if active.exists() {
        return Err(format!(
            "active destination already exists; refusing overwrite: {}",
            active.display()
        ));
    }
    let parent = active
        .parent()
        .ok_or_else(|| "active destination has no parent".to_owned())?;
    fs::create_dir_all(parent)
        .map_err(|error| format!("create {}: {error}", parent.display()))?;
    record.status = "restored".to_owned();
    record.restored_at_epoch_seconds = Some(now_epoch_seconds()?);
    write_json(&lifecycle_path, &record)?;
    fs::rename(&quarantined, &active).map_err(|error| {
        format!(
            "restore quarantined skill {} to {}: {error}",
            quarantined.display(),
            active.display()
        )
    })?;
    println!("Restored `{name}` to {}.", active.display());
    Ok(())
}

fn recommendation_for(root: &Path, name: &str) -> Result<ReviewRecommendation, String> {
    read_review_summary(root)?
        .recommendations
        .into_iter()
        .find(|recommendation| recommendation.skill == name)
        .ok_or_else(|| {
            format!(
                "skill `{name}` has no active review recommendation; refusing quarantine"
            )
        })
}

fn read_review_summary(root: &Path) -> Result<ReviewSummary, String> {
    let path = root.join(REVIEW_PATH);
    if !path.is_file() {
        return Ok(ReviewSummary {
            schema_version: 1,
            recommendations: Vec::new(),
        });
    }
    let mut summary: ReviewSummary = read_json(&path)?;
    summary
        .recommendations
        .sort_by(|left, right| left.skill.cmp(&right.skill));
    Ok(summary)
}

fn parse_quarantine_args(args: &[String]) -> Result<QuarantineArgs, String> {
    let Some(name) = args.first() else {
        return Err(usage());
    };
    let mut confirmed = false;
    let mut reason = None;
    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--confirm" => {
                confirmed = true;
                index += 1;
            }
            "--reason" => {
                let Some(value) = args.get(index + 1) else {
                    return Err("--reason requires text".to_owned());
                };
                if value.trim().is_empty() {
                    return Err("--reason must not be empty".to_owned());
                }
                reason = Some(value.trim().to_owned());
                index += 2;
            }
            value if value.starts_with("--reason=") => {
                let value = value.trim_start_matches("--reason=").trim();
                if value.is_empty() {
                    return Err("--reason must not be empty".to_owned());
                }
                reason = Some(value.to_owned());
                index += 1;
            }
            _ => return Err(usage()),
        }
    }
    Ok(QuarantineArgs {
        name: name.clone(),
        confirmed,
        reason,
    })
}

fn parse_restore_args(args: &[String]) -> Result<(&str, bool), String> {
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
    let content = serde_json::to_vec_pretty(value)
        .map_err(|error| format!("serialize {}: {error}", path.display()))?;
    let temporary = path.with_extension("json.tmp");
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
        .map_err(|error| format!("resolve lifecycle timestamp: {error}"))
}

fn usage() -> String {
    format!("Usage:\n{}", usage_lines())
}

fn schema_one() -> u8 {
    1
}

struct QuarantineArgs {
    name: String,
    confirmed: bool,
    reason: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn recommendation(root: &Path, name: &str) {
        let path = root.join(REVIEW_PATH);
        fs::create_dir_all(path.parent().expect("review parent")).expect("create reviews");
        fs::write(
            path,
            serde_json::to_vec_pretty(&serde_json::json!({
                "schema_version": 1,
                "recommendations": [{
                    "skill": name,
                    "observed_sessions": 5,
                    "verification_rate_milli": 200,
                    "confidence_milli": 333,
                    "reason": "verification rate is weak",
                    "latest_recorded_at": "2026-07-21T15:00:00Z"
                }]
            }))
            .expect("review json"),
        )
        .expect("write review");
    }

    fn active_skill(root: &Path, name: &str, content: &str) {
        let path = root.join(ACTIVE_ROOT).join(name).join("SKILL.md");
        fs::create_dir_all(path.parent().expect("skill parent")).expect("create skill");
        fs::write(path, content).expect("write skill");
    }

    #[test]
    fn quarantine_requires_recommendation_and_explicit_confirmation() {
        let temp = tempfile::tempdir().expect("tempdir");
        active_skill(temp.path(), "verify", "# Verify\n");
        let unconfirmed = vec!["verify".to_owned()];
        assert!(quarantine(temp.path(), &unconfirmed).is_err());
        let confirmed = vec!["verify".to_owned(), "--confirm".to_owned()];
        assert!(quarantine(temp.path(), &confirmed).is_err());
        assert!(temp.path().join(ACTIVE_ROOT).join("verify/SKILL.md").is_file());
    }

    #[test]
    fn quarantine_and_restore_are_byte_exact_and_collision_safe() {
        let temp = tempfile::tempdir().expect("tempdir");
        recommendation(temp.path(), "verify");
        active_skill(temp.path(), "verify", "# Verify\nRun cargo test.\n");
        let quarantine_args = vec!["verify".to_owned(), "--confirm".to_owned()];
        quarantine(temp.path(), &quarantine_args).expect("quarantine");
        assert!(!temp.path().join(ACTIVE_ROOT).join("verify").exists());
        let quarantined = temp.path().join(QUARANTINE_ROOT).join("verify");
        assert!(quarantined.join("SKILL.md").is_file());
        assert!(quarantined.join(LIFECYCLE_FILE).is_file());

        active_skill(temp.path(), "verify", "collision");
        let restore_args = vec!["verify".to_owned(), "--confirm".to_owned()];
        assert!(restore(temp.path(), &restore_args).is_err());
        fs::remove_dir_all(temp.path().join(ACTIVE_ROOT).join("verify")).expect("remove collision");
        restore(temp.path(), &restore_args).expect("restore");
        assert_eq!(
            fs::read_to_string(temp.path().join(ACTIVE_ROOT).join("verify/SKILL.md"))
                .expect("restored skill"),
            "# Verify\nRun cargo test.\n"
        );
    }

    #[test]
    fn names_cannot_escape_lifecycle_roots() {
        assert!(validate_name("../escape").is_err());
        assert!(validate_name("nested/name").is_err());
    }

    #[test]
    fn reviews_are_sorted_deterministically() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join(REVIEW_PATH);
        fs::create_dir_all(path.parent().expect("review parent")).expect("create reviews");
        fs::write(
            &path,
            serde_json::to_vec(&serde_json::json!({
                "recommendations": [
                    {"skill":"zeta","observed_sessions":5,"verification_rate_milli":100,"confidence_milli":300,"reason":"weak","latest_recorded_at":"b"},
                    {"skill":"alpha","observed_sessions":5,"verification_rate_milli":100,"confidence_milli":300,"reason":"weak","latest_recorded_at":"a"}
                ]
            }))
            .expect("json"),
        )
        .expect("write");
        let summary = read_review_summary(temp.path()).expect("reviews");
        assert_eq!(summary.recommendations[0].skill, "alpha");
    }
}
