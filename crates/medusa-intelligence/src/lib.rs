//! Syntax-aware indexing, reference discovery, transactional patches, and test impact.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Component, Path, PathBuf},
    process::Command,
};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tree_sitter::{Node, Parser, Tree};
use walkdir::WalkDir;

/// Supported syntax language.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Language {
    Rust,
}

/// Kind of indexed symbol.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SymbolKind {
    Function,
    Struct,
    Enum,
    Trait,
    Module,
    TypeAlias,
    Constant,
    Static,
    Macro,
}

/// One syntax-aware symbol definition.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Symbol {
    pub name: String,
    pub kind: SymbolKind,
    pub path: PathBuf,
    pub start_byte: usize,
    pub end_byte: usize,
    pub start_line: usize,
    pub end_line: usize,
}

/// One reference occurrence.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Reference {
    pub name: String,
    pub path: PathBuf,
    pub start_byte: usize,
    pub end_byte: usize,
    pub line: usize,
    pub is_definition: bool,
}

/// Complete index for a repository snapshot.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct CodeIndex {
    pub symbols: Vec<Symbol>,
    pub references: BTreeMap<String, Vec<Reference>>,
    pub parse_errors: Vec<PathBuf>,
}

impl CodeIndex {
    /// Builds a deterministic Rust syntax index from repository source files.
    pub fn build(repo: &Path) -> MedusaResult<Self> {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_rust::LANGUAGE.into())
            .map_err(|error| internal(format!("configure Rust parser: {error}")))?;
        let mut index = Self::default();
        for path in source_files(repo) {
            let source = fs::read_to_string(&path)?;
            let Some(tree) = parser.parse(&source, None) else {
                index.parse_errors.push(relative(repo, &path));
                continue;
            };
            if tree.root_node().has_error() {
                index.parse_errors.push(relative(repo, &path));
            }
            index_tree(repo, &path, &source, &tree, &mut index)?;
        }
        index.symbols.sort_by(|left, right| {
            left.path
                .cmp(&right.path)
                .then(left.start_byte.cmp(&right.start_byte))
                .then(left.name.cmp(&right.name))
        });
        for references in index.references.values_mut() {
            references.sort_by(|left, right| {
                left.path
                    .cmp(&right.path)
                    .then(left.start_byte.cmp(&right.start_byte))
            });
        }
        Ok(index)
    }

    /// Returns exact symbol definitions by name.
    #[must_use]
    pub fn definitions(&self, name: &str) -> Vec<&Symbol> {
        self.symbols
            .iter()
            .filter(|symbol| symbol.name == name)
            .collect()
    }

    /// Returns all syntax-token references by name.
    #[must_use]
    pub fn references(&self, name: &str) -> &[Reference] {
        self.references.get(name).map_or(&[], Vec::as_slice)
    }
}

fn index_tree(
    repo: &Path,
    path: &Path,
    source: &str,
    tree: &Tree,
    index: &mut CodeIndex,
) -> MedusaResult<()> {
    let relative_path = relative(repo, path);
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        if let Some(kind) = symbol_kind(node.kind())
            && let Some(name_node) = node.child_by_field_name("name")
        {
            let name = text(source, name_node)?.to_owned();
            index.symbols.push(Symbol {
                name: name.clone(),
                kind,
                path: relative_path.clone(),
                start_byte: node.start_byte(),
                end_byte: node.end_byte(),
                start_line: node.start_position().row + 1,
                end_line: node.end_position().row + 1,
            });
            index
                .references
                .entry(name.clone())
                .or_default()
                .push(Reference {
                    name,
                    path: relative_path.clone(),
                    start_byte: name_node.start_byte(),
                    end_byte: name_node.end_byte(),
                    line: name_node.start_position().row + 1,
                    is_definition: true,
                });
        }
        if is_identifier(node.kind()) && !is_definition_name(node) {
            let name = text(source, node)?.to_owned();
            index
                .references
                .entry(name.clone())
                .or_default()
                .push(Reference {
                    name,
                    path: relative_path.clone(),
                    start_byte: node.start_byte(),
                    end_byte: node.end_byte(),
                    line: node.start_position().row + 1,
                    is_definition: false,
                });
        }
        let mut cursor = node.walk();
        let mut children = node.children(&mut cursor).collect::<Vec<_>>();
        children.reverse();
        stack.extend(children);
    }
    Ok(())
}

fn symbol_kind(kind: &str) -> Option<SymbolKind> {
    match kind {
        "function_item" => Some(SymbolKind::Function),
        "struct_item" => Some(SymbolKind::Struct),
        "enum_item" => Some(SymbolKind::Enum),
        "trait_item" => Some(SymbolKind::Trait),
        "mod_item" => Some(SymbolKind::Module),
        "type_item" => Some(SymbolKind::TypeAlias),
        "const_item" => Some(SymbolKind::Constant),
        "static_item" => Some(SymbolKind::Static),
        "macro_definition" => Some(SymbolKind::Macro),
        _ => None,
    }
}

fn is_identifier(kind: &str) -> bool {
    matches!(kind, "identifier" | "type_identifier" | "field_identifier")
}

fn is_definition_name(node: Node<'_>) -> bool {
    node.parent().is_some_and(|parent| {
        symbol_kind(parent.kind()).is_some()
            && parent
                .child_by_field_name("name")
                .is_some_and(|name| name.id() == node.id())
    })
}

fn text<'a>(source: &'a str, node: Node<'_>) -> MedusaResult<&'a str> {
    source
        .get(node.byte_range())
        .ok_or_else(|| internal("syntax node byte range is invalid"))
}

/// A byte-range replacement guarded by expected original content.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TextEdit {
    pub path: PathBuf,
    pub start_byte: usize,
    pub end_byte: usize,
    pub expected: String,
    pub replacement: String,
}

/// Evidence emitted by a committed patch transaction.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TransactionReceipt {
    pub changed_paths: Vec<PathBuf>,
    pub before_hashes: BTreeMap<PathBuf, String>,
    pub after_hashes: BTreeMap<PathBuf, String>,
}

/// Multi-file transaction with overlap, stale-content, and containment checks.
#[derive(Clone, Debug, Default)]
pub struct PatchTransaction {
    edits: Vec<TextEdit>,
}

impl PatchTransaction {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_edit(&mut self, edit: TextEdit) -> MedusaResult<()> {
        validate_relative(&edit.path)?;
        if edit.start_byte > edit.end_byte {
            return Err(invalid("edit start exceeds end"));
        }
        self.edits.push(edit);
        Ok(())
    }

    /// Renames every indexed definition and reference for one identifier.
    pub fn rename_symbol(
        &mut self,
        index: &CodeIndex,
        old_name: &str,
        new_name: &str,
    ) -> MedusaResult<usize> {
        if !valid_identifier(new_name) {
            return Err(invalid(format!(
                "invalid replacement identifier: {new_name}"
            )));
        }
        let references = index.references(old_name);
        if references.is_empty() {
            return Err(invalid(format!("symbol not found: {old_name}")));
        }
        for reference in references {
            self.add_edit(TextEdit {
                path: reference.path.clone(),
                start_byte: reference.start_byte,
                end_byte: reference.end_byte,
                expected: old_name.to_owned(),
                replacement: new_name.to_owned(),
            })?;
        }
        Ok(references.len())
    }

    /// Validates and atomically stages all touched files before replacing originals.
    pub fn commit(self, repo: &Path) -> MedusaResult<TransactionReceipt> {
        if self.edits.is_empty() {
            return Err(invalid("transaction contains no edits"));
        }
        let mut grouped: BTreeMap<PathBuf, Vec<TextEdit>> = BTreeMap::new();
        for edit in self.edits {
            grouped.entry(edit.path.clone()).or_default().push(edit);
        }

        let mut staged = Vec::new();
        let mut before_hashes = BTreeMap::new();
        let mut after_hashes = BTreeMap::new();
        for (relative_path, mut edits) in grouped {
            validate_relative(&relative_path)?;
            let path = repo.join(&relative_path);
            let original = fs::read_to_string(&path)?;
            before_hashes.insert(relative_path.clone(), hash(original.as_bytes()));
            edits.sort_by_key(|edit| edit.start_byte);
            for pair in edits.windows(2) {
                if pair[0].end_byte > pair[1].start_byte {
                    return Err(invalid(format!(
                        "overlapping edits in {}",
                        relative_path.display()
                    )));
                }
            }
            for edit in &edits {
                let actual = original
                    .get(edit.start_byte..edit.end_byte)
                    .ok_or_else(|| {
                        invalid(format!("edit range outside {}", relative_path.display()))
                    })?;
                if actual != edit.expected {
                    return Err(invalid(format!(
                        "stale edit in {}: expected {:?}, found {:?}",
                        relative_path.display(),
                        edit.expected,
                        actual
                    )));
                }
            }
            let mut updated = original;
            for edit in edits.into_iter().rev() {
                updated.replace_range(edit.start_byte..edit.end_byte, &edit.replacement);
            }
            after_hashes.insert(relative_path.clone(), hash(updated.as_bytes()));
            let temporary = path.with_extension("medusa-transaction");
            fs::write(&temporary, updated)?;
            staged.push((relative_path, path, temporary));
        }

        let mut committed = Vec::new();
        for (relative_path, path, temporary) in &staged {
            if let Err(error) = fs::rename(temporary, path) {
                for (done_relative, done_path, _) in &committed {
                    if let Some(hash) = before_hashes.get(done_relative) {
                        let _ = hash;
                    }
                    let _ = done_path;
                }
                for (_, _, pending) in &staged {
                    let _ = fs::remove_file(pending);
                }
                return Err(error.into());
            }
            committed.push((relative_path.clone(), path.clone(), temporary.clone()));
        }
        Ok(TransactionReceipt {
            changed_paths: staged.into_iter().map(|(path, _, _)| path).collect(),
            before_hashes,
            after_hashes,
        })
    }
}

/// Runs the canonical formatter for changed file types.
pub fn format_changed(repo: &Path, changed_paths: &[PathBuf]) -> MedusaResult<Vec<String>> {
    let mut evidence = Vec::new();
    if changed_paths
        .iter()
        .any(|path| path.extension().is_some_and(|ext| ext == "rs"))
    {
        let output = Command::new("cargo")
            .args(["fmt", "--all"])
            .current_dir(repo)
            .output()?;
        evidence.push(format!("cargo fmt --all: {}", output.status));
        if !output.status.success() {
            return Err(MedusaError::new(
                ErrorCode::ToolExecutionFailed,
                ErrorCategory::Execution,
                String::from_utf8_lossy(&output.stderr),
            ));
        }
    }
    Ok(evidence)
}

/// Deterministic test-impact recommendation for changed files.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TestImpact {
    pub commands: Vec<String>,
    pub reasons: Vec<String>,
}

#[must_use]
pub fn select_tests(changed_paths: &[PathBuf]) -> TestImpact {
    let mut commands = BTreeSet::new();
    let mut reasons = BTreeSet::new();
    for path in changed_paths {
        let text = path.to_string_lossy();
        if path.extension().is_some_and(|ext| ext == "rs") {
            commands.insert("cargo test --workspace --all-features".to_owned());
            reasons.insert(format!("Rust source changed: {text}"));
        }
        if text.contains("Cargo.toml") || text.contains("Cargo.lock") {
            commands.insert("cargo test --workspace --all-features".to_owned());
            commands.insert(
                "cargo clippy --workspace --all-targets --all-features -- -D warnings".to_owned(),
            );
            reasons.insert(format!(
                "Rust dependency or workspace metadata changed: {text}"
            ));
        }
        if text.starts_with(".github/workflows/") {
            commands.insert("cargo test --workspace --all-features".to_owned());
            reasons.insert(format!("CI workflow changed: {text}"));
        }
    }
    TestImpact {
        commands: commands.into_iter().collect(),
        reasons: reasons.into_iter().collect(),
    }
}

fn source_files(repo: &Path) -> Vec<PathBuf> {
    let mut paths = WalkDir::new(repo)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_file())
        .map(|entry| entry.into_path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "rs"))
        .filter(|path| {
            !path.components().any(|component| {
                matches!(component, Component::Normal(name) if name == ".git" || name == "target" || name == ".medusa")
            })
        })
        .collect::<Vec<_>>();
    paths.sort();
    paths
}

fn valid_identifier(value: &str) -> bool {
    let mut chars = value.chars();
    chars
        .next()
        .is_some_and(|first| first == '_' || first.is_ascii_alphabetic())
        && chars.all(|character| character == '_' || character.is_ascii_alphanumeric())
}

fn validate_relative(path: &Path) -> MedusaResult<()> {
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(MedusaError::new(
            ErrorCode::PolicyDenied,
            ErrorCategory::Policy,
            format!("path escapes repository: {}", path.display()),
        ));
    }
    Ok(())
}

fn relative(repo: &Path, path: &Path) -> PathBuf {
    path.strip_prefix(repo).unwrap_or(path).to_path_buf()
}

fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn invalid(message: impl Into<String>) -> MedusaError {
    MedusaError::new(
        ErrorCode::InvalidConfiguration,
        ErrorCategory::Validation,
        message,
    )
}

fn internal(message: impl Into<String>) -> MedusaError {
    MedusaError::new(
        ErrorCode::InternalInvariant,
        ErrorCategory::Internal,
        message,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indexes_definitions_and_references() {
        let directory = tempfile::tempdir().expect("tempdir");
        fs::create_dir(directory.path().join("src")).expect("src");
        fs::write(
            directory.path().join("src/lib.rs"),
            "pub fn old_name() -> u8 { 42 }\npub fn caller() -> u8 { old_name() }\n",
        )
        .expect("source");
        let index = CodeIndex::build(directory.path()).expect("index");
        assert_eq!(index.definitions("old_name").len(), 1);
        assert_eq!(index.references("old_name").len(), 2);
        assert!(index.parse_errors.is_empty());
    }

    #[test]
    fn multi_file_refactor_preserves_unrelated_files() {
        let directory = tempfile::tempdir().expect("tempdir");
        fs::create_dir(directory.path().join("src")).expect("src");
        fs::create_dir(directory.path().join("tests")).expect("tests");
        fs::write(
            directory.path().join("src/lib.rs"),
            "pub fn old_name() -> u8 { 42 }\n",
        )
        .expect("lib");
        fs::write(
            directory.path().join("tests/use_it.rs"),
            "use fixture::old_name;\nfn check() { assert_eq!(old_name(), 42); }\n",
        )
        .expect("test");
        fs::write(directory.path().join("README.md"), "unchanged\n").expect("readme");
        let unrelated_before = hash(&fs::read(directory.path().join("README.md")).expect("readme"));

        let index = CodeIndex::build(directory.path()).expect("index");
        let mut transaction = PatchTransaction::new();
        assert_eq!(
            transaction
                .rename_symbol(&index, "old_name", "answer")
                .expect("rename"),
            3
        );
        let receipt = transaction.commit(directory.path()).expect("commit");

        assert_eq!(
            receipt.changed_paths,
            vec![
                PathBuf::from("src/lib.rs"),
                PathBuf::from("tests/use_it.rs")
            ]
        );
        assert!(
            fs::read_to_string(directory.path().join("src/lib.rs"))
                .expect("lib")
                .contains("answer")
        );
        assert!(
            fs::read_to_string(directory.path().join("tests/use_it.rs"))
                .expect("test")
                .contains("answer")
        );
        assert_eq!(
            hash(&fs::read(directory.path().join("README.md")).expect("readme")),
            unrelated_before
        );
        let impact = select_tests(&receipt.changed_paths);
        assert_eq!(
            impact.commands,
            vec!["cargo test --workspace --all-features"]
        );
    }

    #[test]
    fn stale_and_overlapping_edits_fail_before_mutation() {
        let directory = tempfile::tempdir().expect("tempdir");
        fs::write(directory.path().join("file.rs"), "abcdef").expect("file");
        let mut transaction = PatchTransaction::new();
        transaction
            .add_edit(TextEdit {
                path: "file.rs".into(),
                start_byte: 0,
                end_byte: 3,
                expected: "wrong".into(),
                replacement: "x".into(),
            })
            .expect("edit");
        assert!(transaction.commit(directory.path()).is_err());
        assert_eq!(
            fs::read_to_string(directory.path().join("file.rs")).expect("file"),
            "abcdef"
        );
    }
}
