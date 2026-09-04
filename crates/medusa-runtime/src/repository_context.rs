use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    fs,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
    time::SystemTime,
};

use medusa_agent::AgentSession;
use medusa_core::hidden_command;
use medusa_intelligence::{CodeIndex, RetrievalBudget, Symbol, source_files};
use sha2::{Digest, Sha256};

use crate::RuntimeError;

const SOURCE_TOKEN_BUDGET: usize = 6_000;
const SOURCE_RESULT_BUDGET: usize = 16;
const SOURCE_RESULT_TOKEN_BUDGET: usize = 1_200;
const POLICY_BYTE_BUDGET: usize = 12 * 1024;
const RENDER_BYTE_BUDGET: usize = 36 * 1024;
/// Process-wide CodeIndex entries; a process normally serves one repository.
const INDEX_CACHE_CAPACITY: usize = 8;

#[derive(Clone, Debug, Eq, PartialEq)]
struct RankedEvidence {
    path: PathBuf,
    symbol: String,
    start_line: usize,
    end_line: usize,
    score: u32,
    content: String,
    reasons: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PolicyEvidence {
    path: PathBuf,
    content: String,
    digest: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RepositoryContextAssembly {
    query: String,
    repository_fingerprint: String,
    selected: Vec<RankedEvidence>,
    policies: Vec<PolicyEvidence>,
    omitted: Vec<String>,
    verification_commands: Vec<String>,
    verification_reasons: Vec<String>,
}

struct CachedIndex {
    state_key: String,
    index: CodeIndex,
}

fn index_cache() -> &'static Mutex<HashMap<PathBuf, CachedIndex>> {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, CachedIndex>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Cache key over exactly the indexed inputs: git HEAD (only when the target
/// is itself a repository root) plus the relative path, size, and mtime of
/// every indexed source file.
///
/// Deliberately not `git status --porcelain --untracked-files=all`: git
/// discovers repositories by walking up, so a temp dir under a home-dir repo
/// would inherit a multi-hundred-megabyte status scan that churns with every
/// unrelated file (session journals, caches) and never lets the cache hit.
fn repo_state_key(repo: &Path) -> String {
    let mut hasher = Sha256::new();
    if repo.join(".git").exists()
        && let Some(head) = git_output(repo, &["rev-parse", "HEAD"])
    {
        hasher.update(head.trim().as_bytes());
    }
    for path in source_files(repo) {
        let relative = path.strip_prefix(repo).unwrap_or(&path);
        hasher.update(relative.to_string_lossy().as_bytes());
        if let Ok(metadata) = fs::metadata(&path) {
            hasher.update(metadata.len().to_le_bytes());
            if let Ok(modified) = metadata.modified()
                && let Ok(age) = modified.duration_since(SystemTime::UNIX_EPOCH)
            {
                hasher.update(age.as_nanos().to_le_bytes());
            }
        }
    }
    hex::encode(hasher.finalize())
}

fn cached_code_index(repo: &Path, state_key: &str) -> Result<CodeIndex, RuntimeError> {
    if let Ok(cache) = index_cache().lock()
        && let Some(entry) = cache.get(repo)
        && entry.state_key == state_key
    {
        return Ok(entry.index.clone());
    }
    let index = CodeIndex::build(repo).map_err(|error| RuntimeError::agent(error.to_string()))?;
    if let Ok(mut cache) = index_cache().lock() {
        if cache.len() >= INDEX_CACHE_CAPACITY && !cache.contains_key(repo) {
            cache.clear();
        }
        cache.insert(
            repo.to_path_buf(),
            CachedIndex {
                state_key: state_key.to_owned(),
                index: index.clone(),
            },
        );
    }
    Ok(index)
}

pub(crate) fn assemble_and_render(
    repo: &Path,
    session: &AgentSession,
    turn_query: &str,
) -> Result<String, RuntimeError> {
    let assembly = assemble(repo, session, turn_query)?;
    Ok(assembly.render())
}

fn assemble(
    repo: &Path,
    session: &AgentSession,
    turn_query: &str,
) -> Result<RepositoryContextAssembly, RuntimeError> {
    let state_key = repo_state_key(repo);
    let index = cached_code_index(repo, &state_key)?;
    let query = effective_query(session, turn_query);
    let report = index.retrieve(
        repo,
        &query,
        RetrievalBudget {
            max_tokens: SOURCE_TOKEN_BUDGET,
            max_results: SOURCE_RESULT_BUDGET,
            max_tokens_per_result: SOURCE_RESULT_TOKEN_BUDGET,
        },
    );

    let mut selected = Vec::new();
    let mut ranges: BTreeMap<PathBuf, Vec<(usize, usize)>> = BTreeMap::new();
    let mut omitted = report
        .exclusions
        .iter()
        .map(|entry| {
            format!(
                "{}::{} omitted: {}",
                entry.path.display(),
                entry.symbol,
                entry.reason
            )
        })
        .collect::<Vec<_>>();

    for result in &report.results {
        if overlaps(&ranges, &result.path, result.start_line, result.end_line) {
            omitted.push(format!(
                "{}::{} omitted: overlaps a higher-ranked exact source range",
                result.path.display(),
                result.symbol
            ));
            continue;
        }
        remember_range(
            &mut ranges,
            &result.path,
            result.start_line,
            result.end_line,
        );
        selected.push(RankedEvidence {
            path: result.path.clone(),
            symbol: result.symbol.clone(),
            start_line: result.start_line,
            end_line: result.end_line,
            score: result.score,
            content: result.content.clone(),
            reasons: result.reasons.clone(),
        });
    }

    let seeds = selected.clone();
    for seed in &seeds {
        for reference in index.references(&seed.symbol) {
            if reference.is_definition {
                continue;
            }
            let Some(container) = containing_symbol(&index, &reference.path, reference.line) else {
                continue;
            };
            if overlaps(
                &ranges,
                &container.path,
                container.start_line,
                container.end_line,
            ) {
                continue;
            }
            let Some(content) = exact_symbol_content(repo, container) else {
                omitted.push(format!(
                    "{}::{} omitted: exact caller range became stale",
                    container.path.display(),
                    container.name
                ));
                continue;
            };
            remember_range(
                &mut ranges,
                &container.path,
                container.start_line,
                container.end_line,
            );
            selected.push(RankedEvidence {
                path: container.path.clone(),
                symbol: container.name.clone(),
                start_line: container.start_line,
                end_line: container.end_line,
                score: seed.score.saturating_sub(1),
                content,
                reasons: vec![format!("caller/reference to {}", seed.symbol)],
            });
            if selected.len() >= SOURCE_RESULT_BUDGET {
                break;
            }
        }
        if selected.len() >= SOURCE_RESULT_BUDGET {
            break;
        }
    }

    selected.sort_by(|left, right| {
        right
            .score
            .cmp(&left.score)
            .then_with(|| left.path.cmp(&right.path))
            .then_with(|| left.start_line.cmp(&right.start_line))
            .then_with(|| left.symbol.cmp(&right.symbol))
    });

    let selected_paths = selected
        .iter()
        .map(|entry| entry.path.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let impact = medusa_intelligence::select_tests_with_index(&index, &selected_paths);

    let policies = load_policy_evidence(repo);
    let repository_fingerprint = repository_fingerprint(&state_key, &selected, &policies);

    Ok(RepositoryContextAssembly {
        query,
        repository_fingerprint,
        selected,
        policies,
        omitted,
        verification_commands: impact.commands,
        verification_reasons: impact.reasons,
    })
}

impl RepositoryContextAssembly {
    fn render(&self) -> String {
        let mut output = String::new();
        push_bounded(
            &mut output,
            &format!(
                "Evidence-ranked repository context (snapshot={}):\nquery={}\nBudgets: source={} tokens / {} exact ranges / {} tokens per range; policy={} bytes. Rebuilt from the current repository before this model turn; source ranges must be re-fetched after mutation rather than treated as mutation authority.\n",
                self.repository_fingerprint,
                self.query,
                SOURCE_TOKEN_BUDGET,
                SOURCE_RESULT_BUDGET,
                SOURCE_RESULT_TOKEN_BUDGET,
                POLICY_BYTE_BUDGET,
            ),
        );

        if !self.policies.is_empty() {
            push_bounded(
                &mut output,
                "\nProtected repository policy (non-prunable):\n",
            );
            for policy in &self.policies {
                push_bounded(
                    &mut output,
                    &format!(
                        "[policy:{} sha256={}]\n{}\n",
                        policy.path.display(),
                        policy.digest,
                        policy.content
                    ),
                );
            }
        }

        push_bounded(&mut output, "\nSelected exact implementation evidence:\n");
        for item in &self.selected {
            let digest = digest(item.content.as_bytes());
            let package = package_boundary(&item.path).unwrap_or("repository-root");
            push_bounded(
                &mut output,
                &format!(
                    "[source:{}#L{}-L{} symbol={} package={} score={} sha256={}] reasons={}\n{}\n",
                    item.path.display(),
                    item.start_line,
                    item.end_line,
                    item.symbol,
                    package,
                    item.score,
                    digest,
                    item.reasons.join("; "),
                    item.content
                ),
            );
        }

        if !self.verification_commands.is_empty() {
            push_bounded(&mut output, "\nRelated verification/test evidence:\n");
            for command in &self.verification_commands {
                push_bounded(&mut output, &format!("- command: {command}\n"));
            }
            for reason in &self.verification_reasons {
                push_bounded(&mut output, &format!("- reason: {reason}\n"));
            }
        }

        if !self.omitted.is_empty() {
            push_bounded(
                &mut output,
                "\nContext selection audit (omitted/compressed):\n",
            );
            for reason in self.omitted.iter().take(24) {
                push_bounded(&mut output, &format!("- {reason}\n"));
            }
        }
        output
    }
}

fn effective_query(session: &AgentSession, turn_query: &str) -> String {
    let mut parts = vec![
        turn_query.trim().to_owned(),
        session.objective.trim().to_owned(),
    ];
    for step in &session.plan {
        parts.push(step.title.trim().to_owned());
    }
    parts
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>()
        .join(" ")
}

fn overlaps(
    ranges: &BTreeMap<PathBuf, Vec<(usize, usize)>>,
    path: &Path,
    start: usize,
    end: usize,
) -> bool {
    ranges.get(path).is_some_and(|existing| {
        existing
            .iter()
            .any(|(left, right)| start <= *right && *left <= end)
    })
}

fn remember_range(
    ranges: &mut BTreeMap<PathBuf, Vec<(usize, usize)>>,
    path: &Path,
    start: usize,
    end: usize,
) {
    ranges
        .entry(path.to_path_buf())
        .or_default()
        .push((start, end));
}

fn containing_symbol<'a>(index: &'a CodeIndex, path: &Path, line: usize) -> Option<&'a Symbol> {
    index
        .symbols
        .iter()
        .filter(|candidate| {
            candidate.path == path && candidate.start_line <= line && line <= candidate.end_line
        })
        .min_by_key(|candidate| candidate.end_line.saturating_sub(candidate.start_line))
}

fn exact_symbol_content(repo: &Path, symbol: &Symbol) -> Option<String> {
    let source = fs::read_to_string(repo.join(&symbol.path)).ok()?;
    source
        .get(symbol.start_byte..symbol.end_byte)
        .map(str::to_owned)
}

fn load_policy_evidence(repo: &Path) -> Vec<PolicyEvidence> {
    let candidates = [
        "AGENTS.md",
        ".github/copilot-instructions.md",
        "CONTRIBUTING.md",
        ".medusa/config.toml",
    ];
    let mut used = 0usize;
    let mut policies = Vec::new();
    for candidate in candidates {
        let path = repo.join(candidate);
        let Ok(content) = fs::read_to_string(&path) else {
            continue;
        };
        if content.trim().is_empty() {
            continue;
        }
        let remaining = POLICY_BYTE_BUDGET.saturating_sub(used);
        if remaining == 0 {
            break;
        }
        let bounded = if content.len() <= remaining {
            content
        } else {
            safe_prefix(&content, remaining).to_owned()
        };
        used = used.saturating_add(bounded.len());
        policies.push(PolicyEvidence {
            path: PathBuf::from(candidate),
            digest: digest(bounded.as_bytes()),
            content: bounded,
        });
    }
    policies
}

fn repository_fingerprint(
    state_key: &str,
    selected: &[RankedEvidence],
    policies: &[PolicyEvidence],
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(state_key.as_bytes());
    for item in selected {
        hasher.update(item.path.to_string_lossy().as_bytes());
        hasher.update(item.start_line.to_le_bytes());
        hasher.update(item.end_line.to_le_bytes());
        hasher.update(item.content.as_bytes());
    }
    for policy in policies {
        hasher.update(policy.path.to_string_lossy().as_bytes());
        hasher.update(policy.digest.as_bytes());
    }
    hex::encode(hasher.finalize())
}

fn git_output(repo: &Path, arguments: &[&str]) -> Option<String> {
    let output = hidden_command("git")
        .args(arguments)
        .current_dir(repo)
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

fn package_boundary(path: &Path) -> Option<&str> {
    let mut components = path.components();
    while let Some(component) = components.next() {
        if component.as_os_str() == "crates" {
            return components.next()?.as_os_str().to_str();
        }
    }
    None
}

fn push_bounded(output: &mut String, value: &str) {
    let remaining = RENDER_BYTE_BUDGET.saturating_sub(output.len());
    if remaining == 0 {
        return;
    }
    output.push_str(safe_prefix(value, remaining));
}

fn safe_prefix(value: &str, maximum_bytes: usize) -> &str {
    if value.len() <= maximum_bytes {
        return value;
    }
    let mut end = maximum_bytes;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
}

fn digest(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use medusa_agent::AgentEngine;
    use medusa_config::Config;
    use medusa_core::MedusaResult;
    use medusa_provider::{ModelProvider, ModelRequest, ModelResponse};

    use super::*;

    struct UnusedProvider;

    impl ModelProvider for UnusedProvider {
        fn complete(&self, _: &ModelRequest) -> MedusaResult<ModelResponse> {
            unreachable!("not used")
        }
    }

    fn session(repo: &Path, objective: &str) -> AgentSession {
        let engine = AgentEngine::new(UnusedProvider, Config::default());
        engine
            .create_session(repo, objective.to_owned())
            .expect("session")
    }

    #[test]
    fn localized_context_includes_symbol_caller_and_related_test_scope() {
        let directory = tempfile::tempdir().expect("tempdir");
        fs::create_dir_all(directory.path().join("crates/sample/src")).expect("src");
        fs::create_dir_all(directory.path().join("crates/sample/tests")).expect("tests");
        fs::write(
            directory.path().join("crates/sample/src/lib.rs"),
            "pub fn target_value() -> usize { 1 }\npub fn caller() -> usize { target_value() }\n",
        )
        .expect("source");
        fs::write(
            directory.path().join("crates/sample/tests/target.rs"),
            "fn target_value_contract() { assert_eq!(1, 1); }\n",
        )
        .expect("test");
        let session = session(directory.path(), "fix target_value behavior");
        let assembly = assemble(directory.path(), &session, "target_value").expect("assembly");
        assert!(
            assembly
                .selected
                .iter()
                .any(|item| item.symbol == "target_value")
        );
        assert!(assembly.selected.iter().any(|item| item.symbol == "caller"));
        assert!(
            assembly
                .selected
                .iter()
                .all(|item| item.path.to_string_lossy().contains("sample"))
        );
    }

    #[test]
    fn repository_policy_is_protected_and_rendering_is_bounded() {
        let directory = tempfile::tempdir().expect("tempdir");
        fs::write(
            directory.path().join("AGENTS.md"),
            "Never change public API without tests.\n",
        )
        .expect("policy");
        fs::write(directory.path().join("lib.rs"), "pub fn api_guard() {}\n").expect("source");
        let session = session(directory.path(), "change api_guard");
        let assembly = assemble(directory.path(), &session, "api_guard").expect("assembly");
        let rendered = assembly.render();
        assert!(rendered.contains("Never change public API without tests."));
        assert!(rendered.contains("Protected repository policy (non-prunable)"));
        assert!(rendered.len() <= RENDER_BYTE_BUDGET);
    }

    #[test]
    fn repository_drift_changes_snapshot_and_forces_fresh_source() {
        let directory = tempfile::tempdir().expect("tempdir");
        let source = directory.path().join("lib.rs");
        fs::write(&source, "pub fn drift_target() -> usize { 1 }\n").expect("source");
        let session = session(directory.path(), "fix drift_target");
        let first = assemble(directory.path(), &session, "drift_target").expect("first");
        fs::write(&source, "pub fn drift_target() -> usize { 2 }\n").expect("mutate");
        let second = assemble(directory.path(), &session, "drift_target").expect("second");
        assert_ne!(first.repository_fingerprint, second.repository_fingerprint);
        assert!(
            second
                .selected
                .iter()
                .any(|item| item.content.contains("{ 2 }"))
        );
    }

    #[test]
    fn cached_index_reuses_snapshot_without_rebuild() {
        let directory = tempfile::tempdir().expect("tempdir");
        fs::write(
            directory.path().join("lib.rs"),
            "pub fn cached_target() -> usize { 1 }
",
        )
        .expect("source");
        let session = session(directory.path(), "fix cached_target");
        let first = assemble(directory.path(), &session, "cached_target").expect("first");
        let second = assemble(directory.path(), &session, "cached_target").expect("second");
        assert_eq!(first.repository_fingerprint, second.repository_fingerprint);
        assert_eq!(first.selected, second.selected);
    }

    #[test]
    fn overlapping_exact_ranges_are_not_reinjected() {
        let directory = tempfile::tempdir().expect("tempdir");
        fs::write(
            directory.path().join("lib.rs"),
            "pub fn duplicate_term() -> usize { duplicate_term_helper() }\npub fn duplicate_term_helper() -> usize { 1 }\n",
        )
        .expect("source");
        let session = session(directory.path(), "duplicate_term");
        let assembly = assemble(directory.path(), &session, "duplicate_term").expect("assembly");
        let mut ranges = BTreeSet::new();
        for item in &assembly.selected {
            assert!(ranges.insert((item.path.clone(), item.start_line, item.end_line)));
        }
    }
}
