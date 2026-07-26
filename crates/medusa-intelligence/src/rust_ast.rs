use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use medusa_core::MedusaResult;
use serde::{Deserialize, Serialize};
use tree_sitter::{Node, Parser};

use crate::{
    snapshot::SnapshotDelta,
    support::{internal, relative},
};

/// Stable identifier for a node within one indexed Rust file revision.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct RustAstNodeId(pub u32);

/// A source position using zero-based rows and columns, matching Tree-sitter.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SourcePosition {
    pub row: usize,
    pub column: usize,
}

/// Exact byte and row/column range occupied by a syntax node.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SourceRange {
    pub start_byte: usize,
    pub end_byte: usize,
    pub start: SourcePosition,
    pub end: SourcePosition,
}

/// One named node in a parsed Rust syntax tree.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RustAstNode {
    pub id: RustAstNodeId,
    pub parent: Option<RustAstNodeId>,
    pub kind: String,
    pub field_name: Option<String>,
    pub name: Option<String>,
    pub range: SourceRange,
    pub has_error: bool,
    pub is_missing: bool,
}

/// Parsed AST data for one Rust file.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RustAstFile {
    pub path: PathBuf,
    pub nodes: Vec<RustAstNode>,
    pub root: RustAstNodeId,
    pub has_errors: bool,
}

impl RustAstFile {
    /// Returns a node by its stable file-local identifier.
    #[must_use]
    pub fn node(&self, id: RustAstNodeId) -> Option<&RustAstNode> {
        self.nodes.get(id.0 as usize)
    }

    /// Returns direct children in deterministic source order.
    #[must_use]
    pub fn children(&self, parent: RustAstNodeId) -> Vec<&RustAstNode> {
        self.nodes
            .iter()
            .filter(|node| node.parent == Some(parent))
            .collect()
    }

    /// Returns all nodes of an exact Tree-sitter kind.
    #[must_use]
    pub fn nodes_of_kind(&self, kind: &str) -> Vec<&RustAstNode> {
        self.nodes.iter().filter(|node| node.kind == kind).collect()
    }
}

/// Incremental repository-wide Rust AST index.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct RustAstIndex {
    files: BTreeMap<PathBuf, RustAstFile>,
    parse_errors: Vec<PathBuf>,
}

impl RustAstIndex {
    /// Builds a deterministic AST index for all Rust files in a repository.
    pub fn build(repo: &Path) -> MedusaResult<Self> {
        let mut index = Self::default();
        for entry in walkdir::WalkDir::new(repo)
            .follow_links(false)
            .into_iter()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_file())
        {
            let path = entry.path();
            if path.extension().and_then(|value| value.to_str()) == Some("rs") {
                index.index_file(repo, path)?;
            }
        }
        index.normalize();
        Ok(index)
    }

    /// Re-indexes only added or modified Rust files and removes deleted files.
    pub fn refresh(&mut self, repo: &Path, delta: &SnapshotDelta) -> MedusaResult<Vec<PathBuf>> {
        let invalidated = delta.invalidated_paths();
        for path in &invalidated {
            self.files.remove(path);
            self.parse_errors.retain(|candidate| candidate != path);
        }

        let mut reindexed = Vec::new();
        for path in delta.added.iter().chain(&delta.modified) {
            let absolute = repo.join(path);
            if absolute.is_file()
                && absolute.extension().and_then(|value| value.to_str()) == Some("rs")
            {
                self.index_file(repo, &absolute)?;
                reindexed.push(path.clone());
            }
        }
        self.normalize();
        reindexed.sort();
        Ok(reindexed)
    }

    /// Returns one parsed Rust file by repository-relative path.
    #[must_use]
    pub fn file(&self, path: &Path) -> Option<&RustAstFile> {
        self.files.get(path)
    }

    /// Returns all parsed files in deterministic path order.
    pub fn files(&self) -> impl Iterator<Item = (&Path, &RustAstFile)> {
        self.files.iter().map(|(path, file)| (path.as_path(), file))
    }

    /// Returns files containing parser errors while retaining their valid nodes.
    #[must_use]
    pub fn parse_errors(&self) -> &[PathBuf] {
        &self.parse_errors
    }

    fn index_file(&mut self, repo: &Path, path: &Path) -> MedusaResult<()> {
        let source = fs::read_to_string(path)?;
        let relative_path = relative(repo, path);
        let file = parse_rust_file(relative_path.clone(), &source)?;
        if file.has_errors {
            self.parse_errors.push(relative_path.clone());
        }
        self.files.insert(relative_path, file);
        Ok(())
    }

    fn normalize(&mut self) {
        self.parse_errors.sort();
        self.parse_errors.dedup();
    }
}

/// Parses one Rust source file and preserves valid nodes even when syntax errors exist.
pub fn parse_rust_file(path: PathBuf, source: &str) -> MedusaResult<RustAstFile> {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_rust::LANGUAGE.into())
        .map_err(|error| internal(format!("configure Rust parser: {error}")))?;
    let tree = parser
        .parse(source, None)
        .ok_or_else(|| internal("Rust parser returned no tree"))?;
    let root_node = tree.root_node();
    let has_errors = root_node.has_error();
    let mut nodes = Vec::new();
    collect_named_nodes(root_node, None, None, source, &mut nodes)?;

    Ok(RustAstFile {
        path,
        nodes,
        root: RustAstNodeId(0),
        has_errors,
    })
}

fn collect_named_nodes(
    node: Node<'_>,
    parent: Option<RustAstNodeId>,
    field_name: Option<String>,
    source: &str,
    nodes: &mut Vec<RustAstNode>,
) -> MedusaResult<()> {
    if !node.is_named() {
        return Ok(());
    }

    let id = RustAstNodeId(
        u32::try_from(nodes.len()).map_err(|_| internal("Rust AST contains too many nodes"))?,
    );
    let start = node.start_position();
    let end = node.end_position();
    nodes.push(RustAstNode {
        id,
        parent,
        kind: node.kind().to_owned(),
        field_name,
        name: semantic_name(node, source)?,
        range: SourceRange {
            start_byte: node.start_byte(),
            end_byte: node.end_byte(),
            start: SourcePosition {
                row: start.row,
                column: start.column,
            },
            end: SourcePosition {
                row: end.row,
                column: end.column,
            },
        },
        has_error: node.has_error() || node.is_error(),
        is_missing: node.is_missing(),
    });

    let mut cursor = node.walk();
    for (index, child) in node.children(&mut cursor).enumerate() {
        if child.is_named() {
            let child_field = node.field_name_for_child(index as u32).map(str::to_owned);
            collect_named_nodes(child, Some(id), child_field, source, nodes)?;
        }
    }
    Ok(())
}

fn semantic_name(node: Node<'_>, source: &str) -> MedusaResult<Option<String>> {
    let Some(name) = node.child_by_field_name("name") else {
        return Ok(None);
    };
    let value = source
        .get(name.byte_range())
        .ok_or_else(|| internal("Rust AST node range is outside source text"))?;
    Ok(Some(value.to_owned()))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use crate::{IndexSnapshot, snapshot::SnapshotDelta};

    use super::*;

    #[test]
    fn captures_rust_items_ranges_and_parent_relationships() {
        let source = r#"
mod nested {
    pub struct User<T> { value: T }
    pub trait Read { fn read(&self); }
    impl<T> User<T> { pub fn new(value: T) -> Self { Self { value } } }
}
"#;
        let file = parse_rust_file("src/lib.rs".into(), source).expect("ast");
        assert!(!file.has_errors);
        assert_eq!(file.node(file.root).expect("root").kind, "source_file");
        assert!(!file.children(file.root).is_empty());
        assert_eq!(
            file.nodes_of_kind("struct_item")[0].name.as_deref(),
            Some("User")
        );
        assert_eq!(
            file.nodes_of_kind("trait_item")[0].name.as_deref(),
            Some("Read")
        );
        let method = file
            .nodes_of_kind("function_item")
            .into_iter()
            .find(|node| node.name.as_deref() == Some("new"))
            .expect("method");
        assert!(method.parent.is_some());
        assert!(method.range.end_byte > method.range.start_byte);
    }

    #[test]
    fn retains_partial_ast_for_malformed_rust() {
        let file = parse_rust_file(
            "src/lib.rs".into(),
            "pub struct Good;\npub fn broken( {\npub enum StillVisible { A }\n",
        )
        .expect("partial ast");
        assert!(file.has_errors);
        assert_eq!(
            file.nodes_of_kind("struct_item")[0].name.as_deref(),
            Some("Good")
        );
        assert!(
            file.nodes
                .iter()
                .any(|node| node.has_error || node.is_missing)
        );
    }

    #[test]
    fn incremental_refresh_matches_clean_rebuild() {
        let repository = tempfile::tempdir().expect("repository");
        fs::create_dir_all(repository.path().join("src")).expect("src");
        fs::write(repository.path().join("src/lib.rs"), "pub fn first() {}\n").expect("lib");
        let before = IndexSnapshot::capture(repository.path()).expect("before");
        let mut incremental = RustAstIndex::build(repository.path()).expect("index");
        assert!(incremental.file(Path::new("src/lib.rs")).is_some());
        assert!(incremental.parse_errors().is_empty());

        fs::write(
            repository.path().join("src/lib.rs"),
            "pub fn first() {}\npub struct Added;\n",
        )
        .expect("lib");
        let after = IndexSnapshot::capture(repository.path()).expect("after");
        let delta = SnapshotDelta::between(&before, &after);
        incremental
            .refresh(repository.path(), &delta)
            .expect("refresh");
        let rebuilt = RustAstIndex::build(repository.path()).expect("rebuilt");
        assert_eq!(incremental, rebuilt);
    }
}
