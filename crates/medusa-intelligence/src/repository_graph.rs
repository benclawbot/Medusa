use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use medusa_core::MedusaResult;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    CodeIndex, Reference, SemanticGraph, Symbol, TestImpact, select_tests_with_index,
    support::internal,
};

const SCHEMA_VERSION: u32 = 1;
const CACHE_RELATIVE_PATH: &str = ".medusa/cache/repository-graph-v1.json";

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RepositoryGraphFreshness {
    Current,
    Stale,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RepositoryGraphCapability {
    Semantic,
    WorkspaceMetadata,
    TextFallback,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RepositoryGraphFile {
    pub path: PathBuf,
    pub sha256: String,
    pub bytes: u64,
    pub language: String,
    pub package: Option<String>,
    pub generated: bool,
    pub policy_file: bool,
    pub capability: RepositoryGraphCapability,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RepositoryGraphSnapshot {
    pub schema_version: u32,
    pub repository_revision: String,
    pub graph_revision: String,
    pub files: BTreeMap<PathBuf, RepositoryGraphFile>,
    pub index: CodeIndex,
    pub semantic: SemanticGraph,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RevisionBound<T> {
    pub repository_revision: String,
    pub graph_revision: String,
    pub freshness: RepositoryGraphFreshness,
    pub confidence_milli: u16,
    pub source_adapter: String,
    pub evidence_refs: Vec<String>,
    pub value: T,
}

#[derive(Clone, Debug)]
pub struct RepositoryGraph {
    repo: PathBuf,
    snapshot: RepositoryGraphSnapshot,
}

impl RepositoryGraph {
    pub fn open(repo: &Path) -> MedusaResult<Self> {
        let repo = repo.canonicalize()?;
        let cache_path = repo.join(CACHE_RELATIVE_PATH);
        if let Ok(bytes) = fs::read(&cache_path)
            && let Ok(cached) = serde_json::from_slice::<RepositoryGraphSnapshot>(&bytes)
            && cached.schema_version == SCHEMA_VERSION
            && let Ok((repository_revision, graph_revision)) = graph_identity(&repo)
            && cached.repository_revision == repository_revision
            && cached.graph_revision == graph_revision
        {
            return Ok(Self {
                repo,
                snapshot: cached,
            });
        }
        let current = capture(&repo)?;
        persist(&cache_path, &current)?;
        Ok(Self {
            repo,
            snapshot: current,
        })
    }

    #[must_use]
    pub fn snapshot(&self) -> &RepositoryGraphSnapshot {
        &self.snapshot
    }

    pub fn refresh(&mut self) -> MedusaResult<Vec<PathBuf>> {
        let next = capture(&self.repo)?;
        let changed = changed_paths(&self.snapshot.files, &next.files);
        if next.graph_revision != self.snapshot.graph_revision
            || next.repository_revision != self.snapshot.repository_revision
        {
            persist(&self.repo.join(CACHE_RELATIVE_PATH), &next)?;
            self.snapshot = next;
        }
        Ok(changed)
    }

    #[must_use]
    pub fn freshness(&self) -> RepositoryGraphFreshness {
        match graph_identity(&self.repo) {
            Ok((revision, graph_revision))
                if revision == self.snapshot.repository_revision
                    && graph_revision == self.snapshot.graph_revision =>
            {
                RepositoryGraphFreshness::Current
            }
            _ => RepositoryGraphFreshness::Stale,
        }
    }

    pub fn file(&self, path: &Path) -> RevisionBound<Option<RepositoryGraphFile>> {
        self.bound(self.snapshot.files.get(path).cloned(), "git-tracked-file")
    }

    pub fn definitions(&self, name: &str) -> RevisionBound<Vec<Symbol>> {
        self.bound(
            self.snapshot
                .index
                .definitions(name)
                .into_iter()
                .cloned()
                .collect(),
            "syntax-index",
        )
    }

    pub fn references(&self, name: &str) -> RevisionBound<Vec<Reference>> {
        self.bound(
            self.snapshot.index.references(name).to_vec(),
            "syntax-index",
        )
    }

    pub fn affected_files(&self, changed: &[PathBuf]) -> RevisionBound<Vec<PathBuf>> {
        self.bound(
            self.snapshot.semantic.affected_files(changed),
            "semantic-dependency-graph",
        )
    }

    pub fn related_tests(&self, changed: &[PathBuf]) -> RevisionBound<TestImpact> {
        let mut impact = select_tests_with_index(&self.snapshot.index, changed);
        let excluded = impact
            .commands
            .iter()
            .filter(|command| !test_command_applicable(command, &self.snapshot.files))
            .cloned()
            .collect::<Vec<_>>();
        impact
            .commands
            .retain(|command| test_command_applicable(command, &self.snapshot.files));
        impact.reasons.extend(excluded.into_iter().map(|command| {
            format!("Excluded graph test recommendation without matching manifest: {command}")
        }));
        impact.reasons.sort();
        impact.reasons.dedup();
        self.bound(impact, "semantic-test-impact")
    }

    fn bound<T>(&self, value: T, adapter: &str) -> RevisionBound<T> {
        let freshness = self.freshness();
        RevisionBound {
            repository_revision: self.snapshot.repository_revision.clone(),
            graph_revision: self.snapshot.graph_revision.clone(),
            freshness,
            confidence_milli: if freshness == RepositoryGraphFreshness::Current {
                1_000
            } else {
                0
            },
            source_adapter: adapter.to_owned(),
            evidence_refs: vec![format!(
                "repository-graph:{}:{}",
                self.snapshot.repository_revision, self.snapshot.graph_revision
            )],
            value,
        }
    }
}

fn test_command_applicable(command: &str, files: &BTreeMap<PathBuf, RepositoryGraphFile>) -> bool {
    if !command.starts_with("cargo ") {
        return true;
    }
    if let Some(package) = command
        .split_whitespace()
        .collect::<Vec<_>>()
        .windows(2)
        .find_map(|pair| (pair[0] == "-p").then_some(pair[1]))
    {
        return files.contains_key(Path::new(&format!("crates/{package}/Cargo.toml")));
    }
    files.contains_key(Path::new("Cargo.toml"))
}

fn capture(repo: &Path) -> MedusaResult<RepositoryGraphSnapshot> {
    let repository_revision = git(repo, &["rev-parse", "HEAD"])?;
    let paths = tracked_paths(repo)?;
    let files = file_metadata(repo, &paths)?;
    let graph_revision = fingerprint(&repository_revision, &files);
    let index = CodeIndex::build_from_paths(repo, paths)?;
    let semantic = SemanticGraph::build(&index);
    Ok(RepositoryGraphSnapshot {
        schema_version: SCHEMA_VERSION,
        repository_revision,
        graph_revision,
        files,
        index,
        semantic,
    })
}

fn graph_identity(repo: &Path) -> MedusaResult<(String, String)> {
    let repository_revision = git(repo, &["rev-parse", "HEAD"])?;
    let paths = tracked_paths(repo)?;
    let files = file_metadata(repo, &paths)?;
    let graph_revision = fingerprint(&repository_revision, &files);
    Ok((repository_revision, graph_revision))
}

fn tracked_paths(repo: &Path) -> MedusaResult<Vec<PathBuf>> {
    let output = Command::new("git")
        .args(["ls-files", "-z", "--cached"])
        .current_dir(repo)
        .output()?;
    if !output.status.success() {
        return Err(internal(format!(
            "git ls-files failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    let mut paths = output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
        .map(|entry| PathBuf::from(String::from_utf8_lossy(entry).into_owned()))
        .filter(|path| !is_internal_runtime_path(path))
        .collect::<Vec<_>>();
    paths.sort();
    paths.dedup();
    Ok(paths)
}

fn is_internal_runtime_path(path: &Path) -> bool {
    path.components()
        .next()
        .is_some_and(|component| component.as_os_str() == ".medusa")
}

fn file_metadata(
    repo: &Path,
    paths: &[PathBuf],
) -> MedusaResult<BTreeMap<PathBuf, RepositoryGraphFile>> {
    let mut files = BTreeMap::new();
    for relative in paths {
        let absolute = repo.join(relative);
        if !absolute.is_file() {
            continue;
        }
        let bytes = fs::read(&absolute)?;
        let text = relative.to_string_lossy().replace('\\', "/");
        let language = language(&text).to_owned();
        let capability = match language.as_str() {
            "rust" | "python" => RepositoryGraphCapability::Semantic,
            "typescript" | "javascript" => RepositoryGraphCapability::WorkspaceMetadata,
            _ => RepositoryGraphCapability::TextFallback,
        };
        files.insert(
            relative.clone(),
            RepositoryGraphFile {
                path: relative.clone(),
                sha256: format!("{:x}", Sha256::digest(&bytes)),
                bytes: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
                language,
                package: package_owner(relative),
                generated: is_generated(&text),
                policy_file: is_policy_file(&text),
                capability,
            },
        );
    }
    Ok(files)
}

fn git(repo: &Path, args: &[&str]) -> MedusaResult<String> {
    let output = Command::new("git").args(args).current_dir(repo).output()?;
    if !output.status.success() {
        return Err(internal(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn fingerprint(revision: &str, files: &BTreeMap<PathBuf, RepositoryGraphFile>) -> String {
    let mut digest = Sha256::new();
    digest.update(revision.as_bytes());
    for file in files.values() {
        digest.update(file.path.to_string_lossy().as_bytes());
        digest.update(file.sha256.as_bytes());
    }
    format!("{:x}", digest.finalize())
}

fn changed_paths(
    before: &BTreeMap<PathBuf, RepositoryGraphFile>,
    after: &BTreeMap<PathBuf, RepositoryGraphFile>,
) -> Vec<PathBuf> {
    let mut changed = before
        .keys()
        .chain(after.keys())
        .filter(|path| before.get(*path) != after.get(*path))
        .cloned()
        .collect::<Vec<_>>();
    changed.sort();
    changed.dedup();
    changed
}

fn persist(path: &Path, snapshot: &RepositoryGraphSnapshot) -> MedusaResult<()> {
    let parent = path
        .parent()
        .ok_or_else(|| internal("repository graph cache path has no parent"))?;
    fs::create_dir_all(parent)?;
    let temporary = path.with_extension("json.tmp");
    let bytes = serde_json::to_vec_pretty(snapshot)
        .map_err(|error| internal(format!("serialize repository graph: {error}")))?;
    fs::write(&temporary, bytes)?;
    #[cfg(windows)]
    if path.exists() {
        fs::remove_file(path)?;
    }
    fs::rename(temporary, path)?;
    Ok(())
}

fn language(path: &str) -> &'static str {
    if path.ends_with(".rs") {
        "rust"
    } else if matches!(path.rsplit('.').next(), Some("ts" | "tsx" | "mts" | "cts")) {
        "typescript"
    } else if matches!(path.rsplit('.').next(), Some("js" | "jsx" | "mjs" | "cjs")) {
        "javascript"
    } else if path.ends_with(".py") {
        "python"
    } else {
        "text"
    }
}

fn package_owner(path: &Path) -> Option<String> {
    let components = path
        .components()
        .filter_map(|component| component.as_os_str().to_str())
        .collect::<Vec<_>>();
    match components.as_slice() {
        ["crates" | "packages" | "apps", package, ..] => Some((*package).to_owned()),
        _ => None,
    }
}

fn is_generated(path: &str) -> bool {
    path.starts_with("generated/")
        || path.contains("/generated/")
        || path.ends_with(".generated.rs")
        || path.ends_with(".generated.ts")
        || path.ends_with(".snap")
}

fn is_policy_file(path: &str) -> bool {
    matches!(
        path,
        "AGENTS.md"
            | "SECURITY.md"
            | "Cargo.toml"
            | "Cargo.lock"
            | "package.json"
            | "pnpm-lock.yaml"
            | "yarn.lock"
            | "package-lock.json"
    ) || path.ends_with("/AGENTS.md")
        || path.ends_with("/Cargo.toml")
        || path.ends_with("/package.json")
}

#[cfg(test)]
mod tests {
    use std::{fs, process::Command};

    use super::*;

    fn git_ok(repo: &Path, args: &[&str]) {
        let status = Command::new("git")
            .args(args)
            .current_dir(repo)
            .status()
            .expect("git command");
        assert!(status.success(), "git {args:?} failed");
    }

    fn repository() -> tempfile::TempDir {
        let repo = tempfile::tempdir().expect("repository");
        git_ok(repo.path(), &["init", "-q"]);
        git_ok(
            repo.path(),
            &["config", "user.email", "medusa@example.invalid"],
        );
        git_ok(repo.path(), &["config", "user.name", "Medusa Test"]);
        fs::create_dir_all(repo.path().join("src")).expect("src");
        fs::create_dir_all(repo.path().join("tests")).expect("tests");
        fs::write(
            repo.path().join("Cargo.toml"),
            "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        )
        .expect("manifest");
        fs::write(
            repo.path().join("src/lib.rs"),
            "pub fn answer() -> u8 { 42 }\n",
        )
        .expect("lib");
        fs::write(
            repo.path().join("tests/answer.rs"),
            "use fixture::answer; fn check() { assert_eq!(answer(), 42); }\n",
        )
        .expect("test");
        fs::write(repo.path().join("README.md"), "fixture\n").expect("readme");
        git_ok(repo.path(), &["add", "."]);
        git_ok(repo.path(), &["commit", "-qm", "fixture"]);
        repo
    }

    #[test]
    fn cold_open_persists_and_warm_open_reuses_revision_bound_snapshot() {
        let repo = repository();
        let first = RepositoryGraph::open(repo.path()).expect("cold graph");
        assert_eq!(first.freshness(), RepositoryGraphFreshness::Current);
        assert!(repo.path().join(CACHE_RELATIVE_PATH).is_file());
        let second = RepositoryGraph::open(repo.path()).expect("warm graph");
        assert_eq!(first.snapshot(), second.snapshot());
        assert_eq!(
            second.definitions("answer").freshness,
            RepositoryGraphFreshness::Current
        );
    }

    #[test]
    fn external_change_marks_queries_stale_until_refresh() {
        let repo = repository();
        let mut graph = RepositoryGraph::open(repo.path()).expect("graph");
        fs::write(
            repo.path().join("src/lib.rs"),
            "pub fn answer() -> u8 { 43 }\n",
        )
        .expect("change");
        assert_eq!(graph.freshness(), RepositoryGraphFreshness::Stale);
        assert_eq!(
            graph.definitions("answer").confidence_milli,
            0,
            "stale graph evidence must never silently grant confidence"
        );
        let changed = graph.refresh().expect("refresh");
        assert_eq!(changed, vec![PathBuf::from("src/lib.rs")]);
        assert_eq!(graph.freshness(), RepositoryGraphFreshness::Current);
    }

    #[test]
    fn graph_is_bound_to_branch_revision_and_test_impact() {
        let repo = repository();
        let main = RepositoryGraph::open(repo.path()).expect("main graph");
        let original_revision = main.snapshot().repository_revision.clone();
        git_ok(repo.path(), &["checkout", "-qb", "feature"]);
        fs::write(repo.path().join("src/new.rs"), "pub fn added() {}\n").expect("new");
        git_ok(repo.path(), &["add", "."]);
        git_ok(repo.path(), &["commit", "-qm", "feature"]);
        let feature = RepositoryGraph::open(repo.path()).expect("feature graph");
        assert_ne!(feature.snapshot().repository_revision, original_revision);
        assert!(
            feature
                .snapshot()
                .files
                .contains_key(Path::new("src/new.rs"))
        );
        assert!(
            !feature
                .snapshot()
                .files
                .contains_key(Path::new(CACHE_RELATIVE_PATH)),
            "Medusa runtime state must never become repository graph evidence"
        );
        let impact = feature.related_tests(&[PathBuf::from("src/lib.rs")]);
        assert_eq!(impact.freshness, RepositoryGraphFreshness::Current);
        assert!(!impact.value.commands.is_empty());
    }
    #[test]
    fn test_impact_does_not_invent_cargo_without_a_manifest() {
        let repo = repository();
        fs::remove_file(repo.path().join("Cargo.toml")).expect("remove manifest");
        git_ok(repo.path(), &["add", "-u"]);
        git_ok(repo.path(), &["commit", "-qm", "remove manifest"]);
        let graph = RepositoryGraph::open(repo.path()).expect("graph");
        let impact = graph.related_tests(&[PathBuf::from("src/lib.rs")]);
        assert!(
            impact
                .value
                .commands
                .iter()
                .all(|command| !command.starts_with("cargo ")),
            "graph must not recommend Cargo without a matching Cargo.toml"
        );
    }
}
