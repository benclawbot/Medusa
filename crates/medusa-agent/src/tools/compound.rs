use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

use medusa_core::MedusaResult;
use medusa_intelligence::{
    EditMetadata, EditPreconditions, EditRange, FileSnapshot, RepositoryGraph,
    RepositoryGraphFreshness, ReviewImpact, StructuredEditPlan, StructuredFileOperation,
    StructuredTextEdit, apply_structured_transaction,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::policy::safe_path;

const MAX_EXCERPT_FILES: usize = 8;
const MAX_EXCERPT_LINES: usize = 80;
const MAX_POLICY_FILES: usize = 8;

pub(crate) fn inspect_target(repo: &Path, input: &Value) -> MedusaResult<String> {
    let path = input
        .get("path")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let symbol = input
        .get("symbol")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if path.is_some() == symbol.is_some() {
        return Err(crate::tools::invalid_tool(
            "inspect_target requires exactly one of path or symbol",
        ));
    }

    let graph = RepositoryGraph::open(repo)?;
    if graph.freshness() != RepositoryGraphFreshness::Current {
        return Err(crate::tools::invalid_tool(
            "inspect_target refused stale repository-graph evidence",
        ));
    }

    let definitions = symbol
        .map(|name| graph.definitions(name).value)
        .unwrap_or_default();
    let references = symbol
        .map(|name| graph.references(name).value)
        .unwrap_or_default();
    let mut target_paths = BTreeSet::<PathBuf>::new();
    if let Some(relative) = path {
        let absolute = safe_path(repo, relative)?;
        let root = repo.canonicalize()?;
        let relative = absolute.strip_prefix(root).map_err(|_| {
            crate::tools::invalid_tool("inspect_target path escaped repository scope")
        })?;
        target_paths.insert(relative.to_path_buf());
    }
    target_paths.extend(definitions.iter().map(|definition| definition.path.clone()));
    target_paths.extend(references.iter().map(|reference| reference.path.clone()));
    if target_paths.is_empty() {
        return Err(crate::tools::invalid_tool(
            "inspect_target could not resolve the requested target",
        ));
    }
    let target_paths = target_paths.into_iter().collect::<Vec<_>>();

    let affected = graph.affected_files(&target_paths);
    let tests = graph.related_tests(&target_paths);
    if affected.freshness != RepositoryGraphFreshness::Current
        || tests.freshness != RepositoryGraphFreshness::Current
    {
        return Err(crate::tools::invalid_tool(
            "inspect_target refused stale dependency or test-impact evidence",
        ));
    }
    let review = ReviewImpact::analyze(&graph.snapshot().index, &target_paths);

    let files = target_paths
        .iter()
        .filter_map(|path| graph.snapshot().files.get(path).cloned())
        .collect::<Vec<_>>();
    let excerpts = target_paths
        .iter()
        .take(MAX_EXCERPT_FILES)
        .filter_map(|path| {
            source_excerpt(repo, path)
                .ok()
                .map(|text| json!({"path": path, "text": text}))
        })
        .collect::<Vec<_>>();
    let policy_paths = graph
        .snapshot()
        .files
        .values()
        .filter(|file| file.policy_file)
        .map(|file| file.path.clone())
        .take(MAX_POLICY_FILES)
        .collect::<Vec<_>>();
    let repository_instructions = policy_paths
        .iter()
        .filter_map(|path| {
            source_excerpt(repo, path)
                .ok()
                .map(|text| json!({"path": path, "text": text}))
        })
        .collect::<Vec<_>>();

    let mut evidence_refs = affected.evidence_refs.clone();
    evidence_refs.extend(tests.evidence_refs.iter().cloned());
    evidence_refs.sort();
    evidence_refs.dedup();

    let omissions = [
        "recent diagnostic and failure history is not yet persisted by repository-graph schema v1",
        "inspect_target is read-only; mutation scope remains controlled by policy, approval, and transaction authorities",
    ];
    let response = json!({
        "target": {"path": path, "symbol": symbol},
        "repository_revision": graph.snapshot().repository_revision,
        "graph_revision": graph.snapshot().graph_revision,
        "freshness": RepositoryGraphFreshness::Current,
        "confidence_milli": affected.confidence_milli.min(tests.confidence_milli),
        "source_adapter": "repository-graph-compound-inspection",
        "evidence_refs": evidence_refs,
        "definitions": definitions,
        "references": references,
        "files": files,
        "source_excerpts": excerpts,
        "affected_paths": affected.value,
        "public_api_risk": review.public_api_risk,
        "related_tests": tests.value,
        "repository_policy_paths": policy_paths,
        "repository_instructions": repository_instructions,
        "protected_path_constraints": [
            "all subsequent filesystem paths must remain repository-scoped",
            "write operations still require the existing approval and mutation authorities"
        ],
        "omissions": omissions,
    });
    serde_json::to_string_pretty(&response).map_err(Into::into)
}

pub(crate) fn apply_structured_patch(repo: &Path, input: &Value) -> MedusaResult<String> {
    let expected_revision = input
        .get("repository_revision")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            crate::tools::invalid_tool("repository_revision must be a non-empty string")
        })?;
    let plan_id = input
        .get("plan_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| crate::tools::invalid_tool("plan_id must be a non-empty string"))?;

    let mut graph = RepositoryGraph::open(repo)?;
    if graph.freshness() != RepositoryGraphFreshness::Current
        || graph.snapshot().repository_revision != expected_revision
    {
        return Err(crate::tools::invalid_tool(
            "apply_structured_patch refused repository revision drift",
        ));
    }

    let mut plan = StructuredEditPlan::new(plan_id);
    if let Some(edits) = input.get("edits") {
        let edits = edits
            .as_array()
            .ok_or_else(|| crate::tools::invalid_tool("edits must be an array"))?;
        for edit in edits {
            let relative = required_string(edit, "path")?;
            let _ = safe_path(repo, relative)?;
            let file_hash = required_string(edit, "file_hash")?.to_owned();
            let start_byte = required_usize(edit, "start_byte")?;
            let end_byte = required_usize(edit, "end_byte")?;
            let replacement = required_string(edit, "replacement")?.to_owned();
            let expected_content = edit
                .get("expected_content")
                .and_then(Value::as_str)
                .map(str::to_owned);
            plan.add_text_edit(StructuredTextEdit {
                path: PathBuf::from(relative),
                file_hash: Some(file_hash),
                file_version: None,
                range: EditRange {
                    start_byte,
                    end_byte,
                },
                replacement,
                metadata: metadata(edit),
                preconditions: EditPreconditions {
                    expected_content,
                    expected_symbol: None,
                    expected_ast_node: None,
                },
            });
        }
    }

    if let Some(operations) = input.get("file_operations") {
        let operations = operations
            .as_array()
            .ok_or_else(|| crate::tools::invalid_tool("file_operations must be an array"))?;
        for operation in operations {
            let kind = required_string(operation, "kind")?;
            let structured = match kind {
                "create" => {
                    let relative = required_string(operation, "path")?;
                    let _ = safe_path(repo, relative)?;
                    StructuredFileOperation::Create {
                        path: PathBuf::from(relative),
                        content: required_string(operation, "content")?.as_bytes().to_vec(),
                        overwrite: operation
                            .get("overwrite")
                            .and_then(Value::as_bool)
                            .unwrap_or(false),
                        metadata: metadata(operation),
                    }
                }
                "delete" => {
                    let relative = required_string(operation, "path")?;
                    let _ = safe_path(repo, relative)?;
                    StructuredFileOperation::Delete {
                        path: PathBuf::from(relative),
                        expected_hash: Some(
                            required_string(operation, "expected_hash")?.to_owned(),
                        ),
                        metadata: metadata(operation),
                    }
                }
                "rename" | "move" => {
                    let from = required_string(operation, "from")?;
                    let to = required_string(operation, "to")?;
                    let _ = safe_path(repo, from)?;
                    let _ = safe_path(repo, to)?;
                    let fields = (
                        PathBuf::from(from),
                        PathBuf::from(to),
                        Some(required_string(operation, "expected_hash")?.to_owned()),
                        operation
                            .get("overwrite")
                            .and_then(Value::as_bool)
                            .unwrap_or(false),
                        metadata(operation),
                    );
                    if kind == "rename" {
                        StructuredFileOperation::Rename {
                            from: fields.0,
                            to: fields.1,
                            expected_hash: fields.2,
                            overwrite: fields.3,
                            metadata: fields.4,
                        }
                    } else {
                        StructuredFileOperation::Move {
                            from: fields.0,
                            to: fields.1,
                            expected_hash: fields.2,
                            overwrite: fields.3,
                            metadata: fields.4,
                        }
                    }
                }
                _ => {
                    return Err(crate::tools::invalid_tool(
                        "file operation kind must be create, delete, rename, or move",
                    ));
                }
            };
            plan.add_file_operation(structured);
        }
    }
    if plan.text_edits.is_empty() && plan.file_operations.is_empty() {
        return Err(crate::tools::invalid_tool(
            "apply_structured_patch requires at least one edit or file operation",
        ));
    }

    let touched_paths = plan.touched_paths();
    let snapshots = snapshots_for(repo, &touched_paths)?;
    let pre_impact = graph.affected_files(&touched_paths);
    if pre_impact.freshness != RepositoryGraphFreshness::Current {
        return Err(crate::tools::invalid_tool(
            "apply_structured_patch refused stale dependency evidence",
        ));
    }
    let public_api_risk =
        ReviewImpact::analyze(&graph.snapshot().index, &touched_paths).public_api_risk;
    let receipt = apply_structured_transaction(repo, plan, &snapshots, None)
        .map_err(|error| crate::tools::invalid_tool(error.to_string()))?;
    let invalidated_graph_nodes = pre_impact.value;
    let refreshed_paths = graph.refresh()?;

    serde_json::to_string_pretty(&json!({
        "repository_revision": expected_revision,
        "transaction": receipt,
        "changed_paths": receipt.changed_paths,
        "before_hashes": receipt.before_hashes,
        "after_hashes": receipt.after_hashes,
        "public_api_risk": public_api_risk,
        "invalidated_graph_nodes": invalidated_graph_nodes,
        "graph_refresh_changed_paths": refreshed_paths,
        "graph_revision": graph.snapshot().graph_revision,
        "freshness": graph.freshness(),
        "evidence_refs": [format!(
            "structured-transaction:{}:{}",
            expected_revision, receipt.transaction_id
        )],
    }))
    .map_err(Into::into)
}

fn snapshots_for(repo: &Path, paths: &[PathBuf]) -> MedusaResult<BTreeMap<PathBuf, FileSnapshot>> {
    let mut snapshots = BTreeMap::new();
    for relative in paths {
        let absolute = safe_path(repo, relative.to_string_lossy().as_ref())?;
        if !absolute.is_file() {
            continue;
        }
        let bytes = fs::read(&absolute)?;
        let content = String::from_utf8(bytes.clone()).map_err(|_| {
            crate::tools::invalid_tool(format!(
                "structured text target is not UTF-8: {}",
                relative.display()
            ))
        })?;
        snapshots.insert(
            relative.clone(),
            FileSnapshot {
                hash: sha256(&bytes),
                version: None,
                content,
                symbols: BTreeSet::new(),
                ast_nodes: BTreeSet::new(),
            },
        );
    }
    Ok(snapshots)
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn required_string<'a>(value: &'a Value, key: &str) -> MedusaResult<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| crate::tools::invalid_tool(format!("{key} must be a string")))
}

fn required_usize(value: &Value, key: &str) -> MedusaResult<usize> {
    value
        .get(key)
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .ok_or_else(|| crate::tools::invalid_tool(format!("{key} must be a non-negative integer")))
}

fn metadata(value: &Value) -> EditMetadata {
    EditMetadata {
        intent: value
            .get("intent")
            .and_then(Value::as_str)
            .unwrap_or("structured repository mutation")
            .to_owned(),
        provenance: "model:apply_structured_patch".to_owned(),
        annotation: value
            .get("annotation")
            .and_then(Value::as_str)
            .map(str::to_owned),
    }
}

fn source_excerpt(repo: &Path, relative: &Path) -> MedusaResult<String> {
    let absolute = safe_path(repo, relative.to_string_lossy().as_ref())?;
    let text = fs::read_to_string(absolute)?;
    Ok(text
        .lines()
        .take(MAX_EXCERPT_LINES)
        .collect::<Vec<_>>()
        .join("\n"))
}

#[cfg(test)]
mod tests {
    use std::{fs, process::Command};

    use serde_json::json;

    use super::*;

    fn fixture() -> tempfile::TempDir {
        let directory = tempfile::tempdir().expect("tempdir");
        fs::create_dir(directory.path().join("src")).expect("src");
        fs::write(
            directory.path().join("Cargo.toml"),
            "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        )
        .expect("manifest");
        fs::write(
            directory.path().join("src/lib.rs"),
            "pub fn answer() -> u8 { 42 }\npub fn caller() -> u8 { answer() }\n",
        )
        .expect("source");
        fs::write(directory.path().join("AGENTS.md"), "Keep changes scoped.\n").expect("policy");
        for args in [
            vec!["init"],
            vec!["config", "user.email", "fixture@example.com"],
            vec!["config", "user.name", "Fixture"],
            vec!["add", "."],
            vec!["commit", "-m", "fixture"],
        ] {
            let output = Command::new("git")
                .args(&args)
                .current_dir(directory.path())
                .output()
                .expect("git");
            assert!(
                output.status.success(),
                "git {:?}: {}",
                args,
                String::from_utf8_lossy(&output.stderr)
            );
        }
        directory
    }

    fn head_revision(directory: &tempfile::TempDir) -> String {
        let output = Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(directory.path())
            .output()
            .expect("revision");
        assert!(output.status.success());
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    }

    fn file_hash(path: &Path) -> String {
        sha256(&fs::read(path).expect("file bytes"))
    }

    #[test]
    fn structured_patch_applies_atomically_with_revision_and_hash_guards() {
        let directory = fixture();
        let source = directory.path().join("src/lib.rs");
        let before = fs::read_to_string(&source).expect("source");
        let start = before.find("42").expect("42");
        let output = apply_structured_patch(
            directory.path(),
            &json!({
                "repository_revision": head_revision(&directory),
                "plan_id": "change-answer",
                "edits": [{
                    "path": "src/lib.rs",
                    "file_hash": file_hash(&source),
                    "start_byte": start,
                    "end_byte": start + 2,
                    "expected_content": "42",
                    "replacement": "43",
                    "intent": "update fixture answer"
                }]
            }),
        )
        .expect("structured patch");
        let value: Value = serde_json::from_str(&output).expect("json");
        assert_eq!(value["transaction"]["state"], "committed");
        assert!(
            value["changed_paths"]
                .as_array()
                .is_some_and(|paths| !paths.is_empty())
        );
        assert!(fs::read_to_string(source).expect("updated").contains("43"));
    }

    #[test]
    fn structured_patch_rejects_stale_revision_hash_overlap_and_symlink_escape() {
        let directory = fixture();
        let source = directory.path().join("src/lib.rs");
        let revision = head_revision(&directory);
        let hash = file_hash(&source);

        let stale_revision = apply_structured_patch(
            directory.path(),
            &json!({
                "repository_revision": "not-the-current-revision",
                "plan_id": "stale-revision",
                "edits": [{"path":"src/lib.rs","file_hash":hash,"start_byte":0,"end_byte":0,"replacement":"// x\n"}]
            }),
        )
        .expect_err("stale revision");
        assert!(stale_revision.to_string().contains("revision drift"));

        let stale_hash = apply_structured_patch(
            directory.path(),
            &json!({
                "repository_revision": revision,
                "plan_id": "stale-hash",
                "edits": [{"path":"src/lib.rs","file_hash":"deadbeef","start_byte":0,"end_byte":0,"replacement":"// x\n"}]
            }),
        )
        .expect_err("stale hash");
        assert!(stale_hash.to_string().contains("StaleHash"));

        let overlap = apply_structured_patch(
            directory.path(),
            &json!({
                "repository_revision": head_revision(&directory),
                "plan_id": "overlap",
                "edits": [
                    {"path":"src/lib.rs","file_hash":file_hash(&source),"start_byte":0,"end_byte":4,"replacement":"pub"},
                    {"path":"src/lib.rs","file_hash":file_hash(&source),"start_byte":2,"end_byte":6,"replacement":"x"}
                ]
            }),
        )
        .expect_err("overlap");
        assert!(overlap.to_string().contains("OverlappingEdits"));

        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            symlink("/tmp", directory.path().join("escape")).expect("symlink");
            let escaped = apply_structured_patch(
                directory.path(),
                &json!({
                    "repository_revision": head_revision(&directory),
                    "plan_id": "escape",
                    "file_operations": [{"kind":"create","path":"escape/pwned.txt","content":"no"}]
                }),
            )
            .expect_err("symlink escape");
            assert!(escaped.to_string().contains("symlink"));
        }
    }

    #[test]
    fn structured_patch_supports_create_rename_and_delete() {
        let directory = fixture();
        let revision = head_revision(&directory);
        apply_structured_patch(
            directory.path(),
            &json!({
                "repository_revision": revision,
                "plan_id": "create-file",
                "file_operations": [{"kind":"create","path":"src/new.rs","content":"pub fn new_item() {}\n"}]
            }),
        )
        .expect("create");
        let created = directory.path().join("src/new.rs");
        assert!(created.is_file());
        let created_hash = file_hash(&created);
        apply_structured_patch(
            directory.path(),
            &json!({
                "repository_revision": head_revision(&directory),
                "plan_id": "rename-file",
                "file_operations": [{"kind":"rename","from":"src/new.rs","to":"src/renamed.rs","expected_hash":created_hash}]
            }),
        )
        .expect("rename");
        let renamed = directory.path().join("src/renamed.rs");
        assert!(renamed.is_file());
        apply_structured_patch(
            directory.path(),
            &json!({
                "repository_revision": head_revision(&directory),
                "plan_id": "delete-file",
                "file_operations": [{"kind":"delete","path":"src/renamed.rs","expected_hash":file_hash(&renamed)}]
            }),
        )
        .expect("delete");
        assert!(!renamed.exists());
    }

    #[test]
    fn path_inspection_is_revision_bound_and_bounded() {
        let directory = fixture();
        let output =
            inspect_target(directory.path(), &json!({"path": "src/lib.rs"})).expect("inspect");
        let value: Value = serde_json::from_str(&output).expect("json");
        assert_eq!(value["freshness"], "current");
        assert_eq!(value["confidence_milli"], 1000);
        assert_eq!(value["files"][0]["path"], "src/lib.rs");
        assert!(
            value["repository_instructions"]
                .as_array()
                .is_some_and(|items| !items.is_empty())
        );
        assert!(
            value["source_excerpts"][0]["text"]
                .as_str()
                .is_some_and(|text| text.contains("answer"))
        );
    }

    #[test]
    fn symbol_inspection_coalesces_navigation_and_test_impact() {
        let directory = fixture();
        let output =
            inspect_target(directory.path(), &json!({"symbol": "answer"})).expect("inspect");
        let value: Value = serde_json::from_str(&output).expect("json");
        assert_eq!(value["definitions"].as_array().map(Vec::len), Some(1));
        assert!(
            value["references"]
                .as_array()
                .is_some_and(|items| items.len() >= 2)
        );
        assert!(
            value["affected_paths"]
                .as_array()
                .is_some_and(|items| !items.is_empty())
        );
        assert!(value["related_tests"]["commands"].is_array());
        assert!(
            value["evidence_refs"]
                .as_array()
                .is_some_and(|items| !items.is_empty())
        );
    }
}
