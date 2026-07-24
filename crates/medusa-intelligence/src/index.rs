use std::{
    fs,
    path::{Path, PathBuf},
};

use medusa_core::MedusaResult;
use serde::{Deserialize, Serialize};
use tree_sitter::{Node, Parser, Tree};

use crate::{
    language::{CodeIndex, Language, Reference, Symbol, SymbolKind},
    snapshot::SnapshotDelta,
    support::{internal, relative, source_files},
};

/// Summary of one incremental index refresh.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct IndexRefresh {
    pub reindexed: Vec<PathBuf>,
    pub removed: Vec<PathBuf>,
    pub parse_errors: Vec<PathBuf>,
}

impl CodeIndex {
    /// Builds a deterministic syntax index from supported repository source files.
    pub fn build(repo: &Path) -> MedusaResult<Self> {
        let mut index = Self::default();
        for path in source_files(repo) {
            index_file(repo, &path, &mut index)?;
        }
        index.normalize();
        Ok(index)
    }

    /// Refreshes only paths invalidated by a repository snapshot delta.
    pub fn refresh(&mut self, repo: &Path, delta: &SnapshotDelta) -> MedusaResult<IndexRefresh> {
        let invalidated = delta.invalidated_paths();
        self.remove_paths(&invalidated);

        let mut refresh = IndexRefresh {
            removed: delta.removed.clone(),
            ..IndexRefresh::default()
        };
        for relative_path in delta.added.iter().chain(&delta.modified) {
            let path = repo.join(relative_path);
            if path.is_file() && language_for_path(&path).is_some() {
                index_file(repo, &path, self)?;
                refresh.reindexed.push(relative_path.clone());
            }
        }
        self.normalize();
        refresh.reindexed.sort();
        refresh.removed.sort();
        refresh.parse_errors = self
            .parse_errors
            .iter()
            .filter(|path| invalidated.binary_search(path).is_ok())
            .cloned()
            .collect();
        Ok(refresh)
    }

    fn remove_paths(&mut self, paths: &[PathBuf]) {
        self.symbols.retain(|symbol| !paths.contains(&symbol.path));
        self.parse_errors.retain(|path| !paths.contains(path));
        for references in self.references.values_mut() {
            references.retain(|reference| !paths.contains(&reference.path));
        }
        self.references
            .retain(|_, references| !references.is_empty());
    }

    fn normalize(&mut self) {
        self.symbols.sort_by(|left, right| {
            left.path
                .cmp(&right.path)
                .then(left.start_byte.cmp(&right.start_byte))
                .then(left.name.cmp(&right.name))
        });
        self.parse_errors.sort();
        self.parse_errors.dedup();
        for references in self.references.values_mut() {
            references.sort_by(|left, right| {
                left.path
                    .cmp(&right.path)
                    .then(left.start_byte.cmp(&right.start_byte))
            });
        }
    }
}

fn language_for_path(path: &Path) -> Option<Language> {
    match path.extension().and_then(|extension| extension.to_str()) {
        Some("rs") => Some(Language::Rust),
        Some("py") => Some(Language::Python),
        _ => None,
    }
}

fn parser_for_language(language: Language) -> MedusaResult<Parser> {
    let mut parser = Parser::new();
    let grammar = match language {
        Language::Rust => tree_sitter_rust::LANGUAGE.into(),
        Language::Python => tree_sitter_python::LANGUAGE.into(),
    };
    parser
        .set_language(&grammar)
        .map_err(|error| internal(format!("configure {language:?} parser: {error}")))?;
    Ok(parser)
}

fn index_file(repo: &Path, path: &Path, index: &mut CodeIndex) -> MedusaResult<()> {
    let Some(language) = language_for_path(path) else {
        return Ok(());
    };
    let source = fs::read_to_string(path)?;
    let mut parser = parser_for_language(language)?;
    let Some(tree) = parser.parse(&source, None) else {
        index.parse_errors.push(relative(repo, path));
        return Ok(());
    };
    if tree.root_node().has_error() {
        index.parse_errors.push(relative(repo, path));
    }
    index_tree(language, repo, path, &source, &tree, index)
}

fn index_tree(
    language: Language,
    repo: &Path,
    path: &Path,
    source: &str,
    tree: &Tree,
    index: &mut CodeIndex,
) -> MedusaResult<()> {
    let relative_path = relative(repo, path);
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        if let Some(kind) = symbol_kind(language, node.kind())
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
        if is_identifier(language, node.kind()) && !is_definition_name(language, node) {
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

fn symbol_kind(language: Language, kind: &str) -> Option<SymbolKind> {
    match language {
        Language::Rust => match kind {
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
        },
        Language::Python => match kind {
            "function_definition" => Some(SymbolKind::Function),
            "class_definition" => Some(SymbolKind::Class),
            _ => None,
        },
    }
}

fn is_identifier(language: Language, kind: &str) -> bool {
    match language {
        Language::Rust => matches!(kind, "identifier" | "type_identifier" | "field_identifier"),
        Language::Python => kind == "identifier",
    }
}

fn is_definition_name(language: Language, node: Node<'_>) -> bool {
    node.parent().is_some_and(|parent| {
        symbol_kind(language, parent.kind()).is_some()
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

#[cfg(test)]
mod tests {
    use std::fs;

    use crate::IndexSnapshot;

    use super::*;

    #[test]
    fn incremental_refresh_matches_clean_rebuild() {
        let directory = tempfile::tempdir().expect("tempdir");
        fs::write(directory.path().join("a.rs"), "pub fn old() -> u8 { 1 }\n").expect("a");
        fs::write(
            directory.path().join("b.rs"),
            "pub fn stable() -> u8 { old() }\n",
        )
        .expect("b");
        let before = IndexSnapshot::capture(directory.path()).expect("before");
        let mut incremental = CodeIndex::build(directory.path()).expect("index");

        fs::write(directory.path().join("a.rs"), "pub fn new() -> u8 { 2 }\n").expect("modify");
        fs::remove_file(directory.path().join("b.rs")).expect("remove");
        fs::write(
            directory.path().join("c.rs"),
            "pub fn caller() -> u8 { new() }\n",
        )
        .expect("add");
        let after = IndexSnapshot::capture(directory.path()).expect("after");
        let delta = before.diff(&after);

        let refresh = incremental
            .refresh(directory.path(), &delta)
            .expect("refresh");
        let rebuilt = CodeIndex::build(directory.path()).expect("rebuilt");

        assert_eq!(incremental, rebuilt);
        assert_eq!(
            refresh.reindexed,
            vec![PathBuf::from("a.rs"), PathBuf::from("c.rs")]
        );
        assert_eq!(refresh.removed, vec![PathBuf::from("b.rs")]);
    }

    #[test]
    fn refresh_clears_stale_parse_errors_after_fix() {
        let directory = tempfile::tempdir().expect("tempdir");
        fs::write(directory.path().join("broken.rs"), "fn broken( {\n").expect("broken");
        let before = IndexSnapshot::capture(directory.path()).expect("before");
        let mut index = CodeIndex::build(directory.path()).expect("index");
        assert_eq!(index.parse_errors, vec![PathBuf::from("broken.rs")]);

        fs::write(directory.path().join("broken.rs"), "fn fixed() {}\n").expect("fixed");
        let after = IndexSnapshot::capture(directory.path()).expect("after");
        index
            .refresh(directory.path(), &before.diff(&after))
            .expect("refresh");

        assert!(index.parse_errors.is_empty());
        assert_eq!(index.definitions("fixed").len(), 1);
    }

    #[test]
    fn indexes_python_functions_classes_and_references() {
        let directory = tempfile::tempdir().expect("tempdir");
        fs::write(
            directory.path().join("service.py"),
            "class Worker:\n    def run(self):\n        return helper()\n\ndef helper():\n    return 1\n",
        )
        .expect("python");

        let index = CodeIndex::build(directory.path()).expect("index");

        assert_eq!(index.definitions("Worker")[0].kind, SymbolKind::Class);
        assert_eq!(index.definitions("run")[0].kind, SymbolKind::Function);
        assert_eq!(index.definitions("helper")[0].kind, SymbolKind::Function);
        assert_eq!(index.references("helper").len(), 2);
        assert!(index.parse_errors.is_empty());
    }

    #[test]
    fn python_incremental_refresh_matches_clean_rebuild() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("service.py");
        fs::write(&path, "def before():\n    return 1\n").expect("before");
        let before = IndexSnapshot::capture(directory.path()).expect("snapshot");
        let mut incremental = CodeIndex::build(directory.path()).expect("index");

        fs::write(&path, "def after():\n    return 2\n").expect("after");
        let after = IndexSnapshot::capture(directory.path()).expect("snapshot");
        let refresh = incremental
            .refresh(directory.path(), &before.diff(&after))
            .expect("refresh");

        assert_eq!(refresh.reindexed, vec![PathBuf::from("service.py")]);
        assert!(incremental.definitions("before").is_empty());
        assert_eq!(incremental.definitions("after").len(), 1);
        assert_eq!(
            incremental,
            CodeIndex::build(directory.path()).expect("rebuilt")
        );
    }
}
