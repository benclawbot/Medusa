use std::{collections::BTreeMap, fs, path::Path};

use serde::{Deserialize, Serialize};

const PROBATION_PATH: &str = ".medusa/learning/skill-probation/summary.json";

#[derive(Clone, Debug, Deserialize, Serialize)]
struct ProbationSummary {
    #[serde(default = "schema_one")]
    schema_version: u8,
    policy: ProbationPolicy,
    #[serde(default)]
    skills: BTreeMap<String, ProbationReport>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct ProbationPolicy {
    required_post_restore_samples: usize,
    healthy_rate_milli: u16,
    failure_rate_milli: u16,
    automatic_quarantine: bool,
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
    latest_recorded_at: String,
    recommendation: String,
}

pub(super) fn try_run(root: &Path, args: &[String]) -> Option<Result<(), String>> {
    (args.first().map(String::as_str) == Some("probation"))
        .then(|| run(root, &args[1..]))
}

pub(super) fn usage_line() -> &'static str {
    "  medusa [--repo PATH] skills probation [NAME] [--json]"
}

fn run(root: &Path, args: &[String]) -> Result<(), String> {
    let (name, json_output) = parse_args(args)?;
    let summary = read_summary(root)?;
    match name {
        Some(name) => show_one(&summary, name, json_output),
        None => show_all(&summary, json_output),
    }
}

fn parse_args(args: &[String]) -> Result<(Option<&str>, bool), String> {
    match args {
        [] => Ok((None, false)),
        [flag] if flag == "--json" => Ok((None, true)),
        [name] if !name.starts_with('-') => Ok((Some(name), false)),
        [name, flag] if !name.starts_with('-') && flag == "--json" => Ok((Some(name), true)),
        _ => Err(usage()),
    }
}

fn read_summary(root: &Path) -> Result<ProbationSummary, String> {
    let path = root.join(PROBATION_PATH);
    if !path.is_file() {
        return Ok(ProbationSummary {
            schema_version: 1,
            policy: ProbationPolicy::default(),
            skills: BTreeMap::new(),
        });
    }
    let bytes = fs::read(&path).map_err(|error| format!("read {}: {error}", path.display()))?;
    serde_json::from_slice(&bytes).map_err(|error| format!("parse {}: {error}", path.display()))
}

fn show_all(summary: &ProbationSummary, json_output: bool) -> Result<(), String> {
    if json_output {
        println!(
            "{}",
            serde_json::to_string_pretty(summary)
                .map_err(|error| format!("serialize probation summary: {error}"))?
        );
        return Ok(());
    }
    if summary.skills.is_empty() {
        println!("No restored skills are currently on probation.");
        return Ok(());
    }
    println!("skill\tstate\tsessions\tverified\trate\tchange\tremaining\trecommendation");
    for (name, report) in &summary.skills {
        print_report(name, report);
    }
    Ok(())
}

fn show_one(summary: &ProbationSummary, name: &str, json_output: bool) -> Result<(), String> {
    let report = summary
        .skills
        .get(name)
        .ok_or_else(|| format!("restored skill `{name}` is not on probation"))?;
    if json_output {
        println!(
            "{}",
            serde_json::to_string_pretty(report)
                .map_err(|error| format!("serialize probation report: {error}"))?
        );
    } else {
        println!("skill\tstate\tsessions\tverified\trate\tchange\tremaining\trecommendation");
        print_report(name, report);
    }
    Ok(())
}

fn print_report(name: &str, report: &ProbationReport) {
    println!(
        "{}\t{}\t{}\t{}\t{:.1}%\t{:+.1}%\t{}\t{}",
        name,
        report.state,
        report.post_restore_sessions,
        report.post_restore_verified_sessions,
        f64::from(report.post_restore_verification_rate_milli) / 10.0,
        f64::from(report.verification_rate_change_milli) / 10.0,
        report.remaining_samples,
        report.recommendation
    );
}

fn usage() -> String {
    format!("Usage:\n{}", usage_line())
}

fn schema_one() -> u8 {
    1
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary(root: &Path) {
        let path = root.join(PROBATION_PATH);
        fs::create_dir_all(path.parent().expect("summary parent")).expect("summary directory");
        fs::write(
            path,
            serde_json::to_vec_pretty(&serde_json::json!({
                "schema_version": 1,
                "policy": {
                    "required_post_restore_samples": 3,
                    "healthy_rate_milli": 750,
                    "failure_rate_milli": 500,
                    "automatic_quarantine": false
                },
                "skills": {
                    "verify": {
                        "state": "collecting",
                        "baseline_observed_sessions": 5,
                        "baseline_verification_rate_milli": 200,
                        "baseline_confidence_milli": 333,
                        "post_restore_sessions": 1,
                        "post_restore_verified_sessions": 1,
                        "post_restore_verification_rate_milli": 1000,
                        "post_restore_confidence_milli": 600,
                        "verification_rate_change_milli": 800,
                        "remaining_samples": 2,
                        "restored_at_epoch_seconds": 100,
                        "latest_recorded_at": "2026-07-21T16:00:00Z",
                        "recommendation": "Collect more evidence."
                    }
                }
            }))
            .expect("summary json"),
        )
        .expect("summary");
    }

    #[test]
    fn parser_supports_listing_named_and_json_views() {
        assert_eq!(parse_args(&[]).expect("list"), (None, false));
        assert_eq!(
            parse_args(&["verify".to_owned(), "--json".to_owned()]).expect("named json"),
            (Some("verify"), true)
        );
        assert!(parse_args(&["--unknown".to_owned()]).is_err());
    }

    #[test]
    fn summary_loader_supports_empty_and_populated_state() {
        let repo = tempfile::tempdir().expect("repo");
        assert!(read_summary(repo.path()).expect("empty").skills.is_empty());
        summary(repo.path());
        let loaded = read_summary(repo.path()).expect("loaded");
        assert_eq!(loaded.skills["verify"].state, "collecting");
        assert_eq!(loaded.skills["verify"].remaining_samples, 2);
    }

    #[test]
    fn probation_router_only_claims_its_command() {
        let repo = tempfile::tempdir().expect("repo");
        assert!(try_run(repo.path(), &["metrics".to_owned()]).is_none());
        assert!(try_run(repo.path(), &["probation".to_owned()]).is_some());
    }
}
