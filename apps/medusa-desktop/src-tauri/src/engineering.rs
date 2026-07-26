use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use time::{format_description::well_known::Rfc3339, OffsetDateTime};
use ulid::Ulid;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EngineeringPoint {
    pub date: String,
    pub total: u32,
    pub successful: u32,
    pub success_rate: f64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FrictionItem {
    pub category: String,
    pub count: u32,
    pub sessions: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImprovementRecord {
    pub id: String,
    pub created_at: String,
    pub updated_at: String,
    pub title: String,
    pub problem: String,
    pub proposed_change: String,
    pub evidence: Vec<String>,
    pub source_sessions: Vec<String>,
    pub risk: String,
    pub status: String,
    pub benchmark_before: Option<f64>,
    pub benchmark_after: Option<f64>,
    pub rollback_note: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EngineeringDashboard {
    pub total_tasks: u32,
    pub successful_tasks: u32,
    pub success_rate: f64,
    pub verification_pass_rate: f64,
    pub average_retries: f64,
    pub human_intervention_rate: f64,
    pub rollback_rate: f64,
    pub average_duration_minutes: f64,
    pub trend: Vec<EngineeringPoint>,
    pub friction: Vec<FrictionItem>,
    pub improvements: Vec<ImprovementRecord>,
    pub generated_at: String,
}

#[tauri::command]
pub fn runtime_engineering_dashboard(
    repo: String,
    days: Option<u32>,
) -> Result<EngineeringDashboard, String> {
    let repo = canonical_repo(&repo)?;
    let cutoff = OffsetDateTime::now_utc()
        - time::Duration::days(i64::from(days.unwrap_or(90)));
    let mut by_day = BTreeMap::<String, (u32, u32)>::new();
    let mut friction = BTreeMap::<String, (u32, Vec<String>)>::new();
    let mut total = 0;
    let mut successful = 0;
    let mut verification_total = 0;
    let mut verification_passed = 0;
    let mut retries = 0;
    let mut interventions = 0;
    let mut durations = 0.0;

    for value in session_values(&repo)? {
        let updated = parse_time(value.get("updated_at").and_then(Value::as_str))
            .unwrap_or(OffsetDateTime::UNIX_EPOCH);
        if updated < cutoff {
            continue;
        }
        total += 1;
        let id = value
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_owned();
        let text = value.to_string().to_ascii_lowercase();
        let failed = text.contains("runtime failed")
            || text.contains("verification failed")
            || text.contains("status\":\"failed")
            || text.contains("tool error");
        let completed = value
            .get("completed")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let ok = completed && !failed;
        if ok {
            successful += 1;
        }
        let entry = by_day.entry(updated.date().to_string()).or_default();
        entry.0 += 1;
        if ok {
            entry.1 += 1;
        }
        let created = parse_time(value.get("created_at").and_then(Value::as_str))
            .unwrap_or(updated);
        durations += (updated - created).whole_seconds().max(0) as f64 / 60.0;
        let retry_count = text.matches("retry").count() as u32;
        retries += retry_count;
        if retry_count > 0 {
            add_friction(&mut friction, "retries", &id);
        }
        if failed {
            add_friction(&mut friction, "task failure", &id);
        }
        if value
            .get("pending_question")
            .is_some_and(|pending| !pending.is_null())
        {
            interventions += 1;
            add_friction(&mut friction, "human intervention", &id);
        }
        if text.contains("timeout") {
            add_friction(&mut friction, "timeouts", &id);
        }
        if text.contains("permission denied") || text.contains("policy denied") {
            add_friction(&mut friction, "permissions / policy", &id);
        }
        if text.contains("test failed") || text.contains("tests failed") {
            add_friction(&mut friction, "test failures", &id);
        }
        let verifications = text.matches("verification").count() as u32;
        let failures = text.matches("verification failed").count() as u32
            + text.matches("verification\":false").count() as u32;
        verification_total += verifications;
        verification_passed += verifications.saturating_sub(failures);
    }

    let trend = by_day
        .into_iter()
        .map(|(date, (total, successful))| EngineeringPoint {
            date,
            total,
            successful,
            success_rate: rate(successful, total),
        })
        .collect();
    let mut friction = friction
        .into_iter()
        .map(|(category, (count, sessions))| FrictionItem {
            category,
            count,
            sessions,
        })
        .collect::<Vec<_>>();
    friction.sort_by(|left, right| {
        right
            .count
            .cmp(&left.count)
            .then_with(|| left.category.cmp(&right.category))
    });
    let improvements = read_improvements(&repo)?;
    let rolled_back = improvements
        .iter()
        .filter(|item| item.status == "rolledBack")
        .count() as u32;

    Ok(EngineeringDashboard {
        total_tasks: total,
        successful_tasks: successful,
        success_rate: rate(successful, total),
        verification_pass_rate: rate(verification_passed, verification_total),
        average_retries: if total == 0 { 0.0 } else { retries as f64 / total as f64 },
        human_intervention_rate: rate(interventions, total),
        rollback_rate: rate(rolled_back, improvements.len() as u32),
        average_duration_minutes: if total == 0 { 0.0 } else { durations / total as f64 },
        trend,
        friction,
        improvements,
        generated_at: OffsetDateTime::now_utc()
            .format(&Rfc3339)
            .unwrap_or_default(),
    })
}

#[tauri::command]
pub fn runtime_generate_improvement(repo: String) -> Result<ImprovementRecord, String> {
    let dashboard = runtime_engineering_dashboard(repo.clone(), Some(90))?;
    let top = dashboard
        .friction
        .first()
        .cloned()
        .ok_or("not enough recorded friction to generate a proposal")?;
    let now = OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .map_err(|error| error.to_string())?;
    let record = ImprovementRecord {
        id: Ulid::new().to_string(),
        created_at: now.clone(),
        updated_at: now,
        title: format!("Reduce {}", top.category),
        problem: format!("{} appeared in {} recorded task(s).", top.category, top.count),
        proposed_change: format!(
            "Add a focused prevention and recovery path for {} and validate it against affected sessions.",
            top.category
        ),
        evidence: vec![format!("Observed {} occurrence(s) in the selected window", top.count)],
        source_sessions: top.sessions,
        risk: "low".into(),
        status: "pending".into(),
        benchmark_before: Some(dashboard.success_rate),
        benchmark_after: None,
        rollback_note: "Restore the previous configuration and re-run the frozen benchmark.".into(),
    };
    let repo = canonical_repo(&repo)?;
    let mut records = read_improvements(&repo)?;
    records.push(record.clone());
    write_improvements(&repo, &records)?;
    Ok(record)
}

#[tauri::command]
pub fn runtime_update_improvement(
    repo: String,
    id: String,
    action: String,
) -> Result<ImprovementRecord, String> {
    let repo = canonical_repo(&repo)?;
    let benchmark_after = if action == "benchmark" {
        Some(
            runtime_engineering_dashboard(repo.to_string_lossy().into_owned(), Some(30))?
                .success_rate,
        )
    } else {
        None
    };
    let mut records = read_improvements(&repo)?;
    let item = records
        .iter_mut()
        .find(|item| item.id == id)
        .ok_or("improvement not found")?;
    item.status = match action.as_str() {
        "approve" => "approved",
        "reject" => "rejected",
        "adopt" => "adopted",
        "rollback" => "rolledBack",
        "benchmark" => {
            item.benchmark_after = benchmark_after;
            "benchmarked"
        }
        _ => return Err("unsupported improvement action".into()),
    }
    .into();
    item.updated_at = OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_default();
    let result = item.clone();
    write_improvements(&repo, &records)?;
    Ok(result)
}

fn add_friction(
    map: &mut BTreeMap<String, (u32, Vec<String>)>,
    category: &str,
    id: &str,
) {
    let entry = map.entry(category.into()).or_default();
    entry.0 += 1;
    if entry.1.len() < 20 && !entry.1.iter().any(|value| value == id) {
        entry.1.push(id.into());
    }
}

fn rate(part: u32, total: u32) -> f64 {
    if total == 0 { 0.0 } else { part as f64 / total as f64 * 100.0 }
}

fn parse_time(value: Option<&str>) -> Option<OffsetDateTime> {
    value.and_then(|value| OffsetDateTime::parse(value, &Rfc3339).ok())
}

fn canonical_repo(repo: &str) -> Result<PathBuf, String> {
    let path = fs::canonicalize(Path::new(repo))
        .map_err(|error| format!("cannot open {repo}: {error}"))?;
    if !path.is_dir() {
        return Err("repository is not a directory".into());
    }
    Ok(path)
}

fn engineering_path(repo: &Path) -> PathBuf {
    repo.join(".medusa/engineering/improvements.json")
}

fn read_improvements(repo: &Path) -> Result<Vec<ImprovementRecord>, String> {
    let path = engineering_path(repo);
    if !path.exists() {
        return Ok(Vec::new());
    }
    serde_json::from_slice(&fs::read(path).map_err(|error| error.to_string())?)
        .map_err(|error| error.to_string())
}

fn write_improvements(repo: &Path, records: &[ImprovementRecord]) -> Result<(), String> {
    let path = engineering_path(repo);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    fs::write(
        path,
        serde_json::to_vec_pretty(records).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())
}

fn session_values(repo: &Path) -> Result<Vec<Value>, String> {
    let mut values = BTreeMap::new();
    for root in [repo.join(".medusa/sessions"), fallback_session_root(repo)] {
        let Ok(entries) = fs::read_dir(root) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            if let Ok(bytes) = fs::read(path) {
                if let Ok(value) = serde_json::from_slice::<Value>(&bytes) {
                    if let Some(id) = value.get("id").and_then(Value::as_str) {
                        values.entry(id.to_owned()).or_insert(value);
                    }
                }
            }
        }
    }
    Ok(values.into_values().collect())
}

fn fallback_session_root(repo: &Path) -> PathBuf {
    let root = std::env::var_os("LOCALAPPDATA")
        .or_else(|| std::env::var_os("APPDATA"))
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
        .unwrap_or_else(std::env::temp_dir);
    root.join("Medusa/sessions").join(repository_key(repo))
}

fn repository_key(repo: &Path) -> String {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in repo.to_string_lossy().as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}
