use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use medusa_core::MedusaResult;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RiskClass {
    ReadOnly,
    RepositoryMutation,
    External,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum EscalationPolicy {
    SearchBeforeRead,
    FocusedRangeBeforeFullFile,
    TargetedTestBeforeBroadSuite,
    Direct,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct ToolCapability {
    pub name: String,
    pub capability: String,
    pub risk: RiskClass,
    pub latency_weight: u16,
    pub token_weight: u16,
    pub cacheable: bool,
    pub concurrent: bool,
    pub fallback: Option<String>,
    pub expected_output_bytes: usize,
    pub escalation: EscalationPolicy,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct RankedRecommendation {
    pub selected: String,
    pub score: i64,
    pub rationale: Vec<String>,
    pub alternatives: Vec<(String, i64)>,
    pub explicit_override: bool,
    pub expected_output_bytes: usize,
    pub escalation: EscalationPolicy,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct CacheEvidence {
    pub status: String,
    pub key: String,
    pub reason: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct CacheEntry {
    schema_version: u16,
    tool: String,
    input_digest: String,
    repository_fingerprint: String,
    created_unix_ms: u128,
    output: String,
}

const CACHE_SCHEMA_VERSION: u16 = 1;

pub(crate) fn registry() -> BTreeMap<&'static str, ToolCapability> {
    let mut registry = BTreeMap::new();
    registry.insert(
        "search_text",
        ToolCapability {
            name: "search_text".into(),
            capability: "bounded_repository_search".into(),
            risk: RiskClass::ReadOnly,
            latency_weight: 10,
            token_weight: 8,
            cacheable: true,
            concurrent: true,
            fallback: Some("code_index".into()),
            expected_output_bytes: 8 * 1024,
            escalation: EscalationPolicy::SearchBeforeRead,
        },
    );
    registry.insert(
        "fs_read",
        ToolCapability {
            name: "fs_read".into(),
            capability: "repository_file_read".into(),
            risk: RiskClass::ReadOnly,
            latency_weight: 6,
            token_weight: 24,
            cacheable: true,
            concurrent: true,
            fallback: Some("search_text".into()),
            expected_output_bytes: 32 * 1024,
            escalation: EscalationPolicy::FocusedRangeBeforeFullFile,
        },
    );
    registry.insert(
        "semantic_capabilities",
        ToolCapability {
            name: "semantic_capabilities".into(),
            capability: "language_capability_report".into(),
            risk: RiskClass::ReadOnly,
            latency_weight: 1,
            token_weight: 4,
            cacheable: true,
            concurrent: true,
            fallback: None,
            expected_output_bytes: 8 * 1024,
            escalation: EscalationPolicy::Direct,
        },
    );
    registry.insert(
        "code_index",
        ToolCapability {
            name: "code_index".into(),
            capability: "parsed_repository_index".into(),
            risk: RiskClass::ReadOnly,
            latency_weight: 24,
            token_weight: 10,
            cacheable: true,
            concurrent: true,
            fallback: Some("search_text".into()),
            expected_output_bytes: 12 * 1024,
            escalation: EscalationPolicy::SearchBeforeRead,
        },
    );
    registry.insert(
        "shell_run",
        ToolCapability {
            name: "shell_run".into(),
            capability: "contained_command_execution".into(),
            risk: RiskClass::ReadOnly,
            latency_weight: 35,
            token_weight: 18,
            cacheable: false,
            concurrent: false,
            fallback: None,
            expected_output_bytes: 24 * 1024,
            escalation: EscalationPolicy::TargetedTestBeforeBroadSuite,
        },
    );
    for name in ["fs_write", "fs_create_dir", "patch_apply", "symbol_rename"] {
        registry.insert(
            name,
            ToolCapability {
                name: name.into(),
                capability: "repository_mutation".into(),
                risk: RiskClass::RepositoryMutation,
                latency_weight: 18,
                token_weight: 6,
                cacheable: false,
                concurrent: false,
                fallback: None,
                expected_output_bytes: 2 * 1024,
                escalation: EscalationPolicy::Direct,
            },
        );
    }
    registry
}

pub(crate) fn recommend(requested: &str, input_summary: &str) -> RankedRecommendation {
    let registry = registry();
    let selected = registry.get(requested).cloned().unwrap_or(ToolCapability {
        name: requested.into(),
        capability: "unclassified".into(),
        risk: RiskClass::External,
        latency_weight: 50,
        token_weight: 50,
        cacheable: false,
        concurrent: false,
        fallback: None,
        expected_output_bytes: 32 * 1024,
        escalation: EscalationPolicy::Direct,
    });
    let score = utility_score(&selected);
    let mut alternatives = registry
        .values()
        .filter(|candidate| {
            candidate.capability == selected.capability && candidate.name != selected.name
        })
        .map(|candidate| (candidate.name.clone(), utility_score(candidate)))
        .collect::<Vec<_>>();
    alternatives.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));

    let mut rationale = vec![
        format!("capability={}", selected.capability),
        format!("risk={:?}", selected.risk),
        format!("latency_weight={}", selected.latency_weight),
        format!("token_weight={}", selected.token_weight),
        format!("cacheable={}", is_cacheable(requested, input_summary)),
        format!("input={input_summary}"),
    ];
    if requested == "shell_run" && looks_like_broad_test(input_summary) {
        rationale.push("escalation=prefer targeted test before broad suite".into());
    }

    RankedRecommendation {
        selected: requested.into(),
        score,
        rationale,
        alternatives,
        explicit_override: false,
        expected_output_bytes: selected.expected_output_bytes,
        escalation: selected.escalation,
    }
}

fn utility_score(capability: &ToolCapability) -> i64 {
    let risk_penalty = match capability.risk {
        RiskClass::ReadOnly => 0,
        RiskClass::RepositoryMutation => 25,
        RiskClass::External => 40,
    };
    1_000
        - i64::from(capability.latency_weight) * 4
        - i64::from(capability.token_weight) * 3
        - risk_penalty
        + if capability.cacheable { 35 } else { 0 }
}

pub(crate) fn looks_like_broad_test(summary: &str) -> bool {
    let normalized = summary.to_ascii_lowercase();
    normalized.contains("cargo test --workspace")
        || normalized.contains("cargo test --all")
        || normalized.contains("pytest") && !normalized.contains("::")
        || normalized.contains("npm test") && !normalized.contains("--")
}

fn is_cacheable(tool: &str, input: &str) -> bool {
    if registry().get(tool).is_some_and(|entry| entry.cacheable) {
        return true;
    }
    if tool != "shell_run" {
        return false;
    }
    let normalized = input.trim().to_ascii_lowercase();
    [
        "cargo metadata",
        "cargo locate-project",
        "git rev-parse",
        "git status --porcelain",
        "git ls-files",
        "rustc --version",
        "cargo --version",
        "node --version",
        "python --version",
        "python3 --version",
    ]
    .iter()
    .any(|prefix| normalized.starts_with(prefix))
}

pub(crate) fn cache_lookup(
    repo: &Path,
    tool: &str,
    input: &str,
) -> MedusaResult<(Option<String>, CacheEvidence)> {
    if !is_cacheable(tool, input) {
        return Ok((
            None,
            CacheEvidence {
                status: "bypass".into(),
                key: String::new(),
                reason: "tool invocation is not safely cacheable".into(),
            },
        ));
    }
    let fingerprint = repository_fingerprint(repo)?;
    let key = cache_key(tool, input, &fingerprint);
    let path = cache_path(repo, &key);
    if !path.exists() {
        return Ok((
            None,
            CacheEvidence {
                status: "miss".into(),
                key,
                reason: "entry does not exist".into(),
            },
        ));
    }
    let entry: CacheEntry = serde_json::from_slice(&fs::read(&path)?)?;
    if entry.schema_version != CACHE_SCHEMA_VERSION || entry.repository_fingerprint != fingerprint {
        let invalidation = invalidate(repo, None)?;
        return Ok((
            None,
            CacheEvidence {
                status: "miss".into(),
                key,
                reason: format!(
                    "{}; {}",
                    "repository fingerprint changed", invalidation.reason
                ),
            },
        ));
    }
    Ok((
        Some(entry.output),
        CacheEvidence {
            status: "hit".into(),
            key,
            reason: "unchanged repository state".into(),
        },
    ))
}

pub(crate) fn cache_store(
    repo: &Path,
    tool: &str,
    input: &str,
    output: &str,
) -> MedusaResult<CacheEvidence> {
    if !is_cacheable(tool, input) {
        return Ok(CacheEvidence {
            status: "bypass".into(),
            key: String::new(),
            reason: "tool invocation is not safely cacheable".into(),
        });
    }
    let fingerprint = repository_fingerprint(repo)?;
    let key = cache_key(tool, input, &fingerprint);
    let path = cache_path(repo, &key);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let entry = CacheEntry {
        schema_version: CACHE_SCHEMA_VERSION,
        tool: tool.into(),
        input_digest: short_digest(input.as_bytes()),
        repository_fingerprint: fingerprint,
        created_unix_ms: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
        output: output.into(),
    };
    fs::write(path, serde_json::to_vec(&entry)?)?;
    Ok(CacheEvidence {
        status: "stored".into(),
        key,
        reason: "successful deterministic discovery command".into(),
    })
}

pub(crate) fn invalidate(repo: &Path, relative: Option<&str>) -> MedusaResult<CacheEvidence> {
    let root = repo.join(".medusa/tool-cache");
    if root.exists() {
        fs::remove_dir_all(&root)?;
    }
    Ok(CacheEvidence {
        status: "invalidated".into(),
        key: relative.unwrap_or("repository").into(),
        reason: "repository mutation changed observable state".into(),
    })
}

pub(crate) fn format_evidence(
    recommendation: &RankedRecommendation,
    cache: &CacheEvidence,
) -> String {
    format!(
        "[tool-orchestration selected={}; score={}; expected_output_bytes={}; escalation={:?}; override={}; cache_status={}; cache_key={}; cache_reason={}; rationale={}]",
        recommendation.selected,
        recommendation.score,
        recommendation.expected_output_bytes,
        recommendation.escalation,
        recommendation.explicit_override,
        cache.status,
        cache.key,
        cache.reason,
        recommendation.rationale.join("|")
    )
}

fn cache_path(repo: &Path, key: &str) -> PathBuf {
    repo.join(".medusa/tool-cache").join(format!("{key}.json"))
}

fn cache_key(tool: &str, input: &str, fingerprint: &str) -> String {
    short_digest(format!("{tool}\0{input}\0{fingerprint}").as_bytes())
}

fn repository_fingerprint(repo: &Path) -> MedusaResult<String> {
    let mut hasher = Sha256::new();
    for name in [
        "Cargo.toml",
        "Cargo.lock",
        "package.json",
        "pyproject.toml",
        ".git/HEAD",
        ".git/index",
    ] {
        let path = repo.join(name);
        if let Ok(metadata) = fs::metadata(&path) {
            hasher.update(name.as_bytes());
            hasher.update(metadata.len().to_le_bytes());
            if let Ok(modified) = metadata.modified().and_then(|value| {
                value
                    .duration_since(UNIX_EPOCH)
                    .map_err(std::io::Error::other)
            }) {
                hasher.update(modified.as_nanos().to_le_bytes());
            }
        }
    }
    Ok(hex::encode(&hasher.finalize()[..12]))
}

fn short_digest(value: &[u8]) -> String {
    let digest = Sha256::digest(value);
    hex::encode(&digest[..12])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranking_is_deterministic_and_exposes_inputs() {
        let first = recommend("search_text", "query=ToolManager");
        let second = recommend("search_text", "query=ToolManager");
        assert_eq!(first.selected, second.selected);
        assert_eq!(first.score, second.score);
        assert!(first.rationale.iter().any(|item| item == "cacheable=true"));
    }

    #[test]
    fn broad_test_detection_prefers_narrow_scope() {
        assert!(looks_like_broad_test("cargo test --workspace"));
        assert!(!looks_like_broad_test(
            "cargo test -p medusa-agent ranking_is_deterministic"
        ));
    }

    #[test]
    fn safe_discovery_commands_are_cacheable_but_test_runs_are_not() {
        assert!(is_cacheable("shell_run", "cargo metadata --no-deps"));
        assert!(is_cacheable("shell_run", "git rev-parse HEAD"));
        assert!(!is_cacheable("shell_run", "cargo test --workspace"));
    }

    #[test]
    fn cache_reuses_unchanged_repository_and_invalidates_after_mutation() {
        let repo = tempfile::tempdir().expect("repository");
        fs::write(repo.path().join("Cargo.toml"), "[workspace]\n").expect("fixture");
        let input = "cargo metadata --no-deps";
        let miss = cache_lookup(repo.path(), "shell_run", input)
            .expect("lookup")
            .1;
        assert_eq!(miss.status, "miss");
        cache_store(repo.path(), "shell_run", input, "{\"packages\":[]}").expect("store");
        let (hit, evidence) = cache_lookup(repo.path(), "shell_run", input).expect("lookup");
        assert_eq!(evidence.status, "hit");
        assert_eq!(hit.as_deref(), Some("{\"packages\":[]}"));
        invalidate(repo.path(), Some("Cargo.toml")).expect("invalidate");
        assert_eq!(
            cache_lookup(repo.path(), "shell_run", input)
                .expect("lookup")
                .1
                .status,
            "miss"
        );
    }
}
