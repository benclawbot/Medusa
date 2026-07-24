use std::{
    collections::BTreeSet,
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

fn index_file(repo: &Path, path: &Path, index: &mut CodeIndex) -> MedusaResult<()> {
    let Some(language) = language_for_path(path) else {
        return Ok(());
    };
    let source = fs::read_to_string(path)?;
    match language {
        Language::Rust => index_rust_file(repo, path, &source, index),
        Language::Python => {
            index_python_file(repo, path, &source, index);
            Ok(())
        }
    }
}

fn index_rust_file(
    repo: &Path,
    path: &Path,
    source: &str,
    index: &mut CodeIndex,
) -> MedusaResult<()> {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_rust::LANGUAGE.into())
        .map_err(|error| internal(format!("configure Rust parser: {error}")))?;
    let Some(tree) = parser.parse(source, None) else {
        index.parse_errors.push(relative(repo, path));
        return Ok(());
    };
    if tree.root_node().has_error() {
        index.parse_errors.push(relative(repo, path));
    }
    index_rust_tree(repo, path, source, &tree, index)
}

fn index_rust_tree(
    repo: &Path,
    path: &Path,
    source: &str,
    tree: &Tree,
    index: &mut CodeIndex,
) -> MedusaResult<()> {
    let relative_path = relative(repo, path);
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        if let Some(kind) = rust_symbol_kind(node.kind())
            && let Some(name_node) = node.child_by_field_name("name")
        {
            let name = text(source, name_node)?.to_owned();
            push_definition(
                index,
                &relative_path,
                name,
                kind,
                node.start_byte(),
                node.end_byte(),
                node.start_position().row + 1,
                node.end_position().row + 1,
                name_node.start_byte(),
                name_node.end_byte(),
            );
        }
        if is_rust_identifier(node.kind()) && !is_rust_definition_name(node) {
            let name = text(source, node)?.to_owned();
            push_reference(
                index,
                &relative_path,
                name,
                node.start_byte(),
                node.end_byte(),
                node.start_position().row + 1,
                false,
            );
        }
        let mut cursor = node.walk();
        let mut children = node.children(&mut cursor).collect::<Vec<_>>();
        children.reverse();
        stack.extend(children);
    }
    Ok(())
}

fn index_python_file(repo: &Path, path: &Path, source: &str, index: &mut CodeIndex) {
    let relative_path = relative(repo, path);
    let mut definition_ranges = BTreeSet::new();
    let mut offset = 0usize;

    for (line_index, raw_line) in source.split_inclusive('\n').enumerate() {
        let line = raw_line.strip_suffix('\n').unwrap_or(raw_line);
        let indentation = line.len() - line.trim_start().len();
        let trimmed = &line[indentation..];
        let declaration = trimmed
            .strip_prefix("async def ")
            .map(|rest| (rest, SymbolKind::Function, "async def ".len()))
            .or_else(|| {
                trimmed
                    .strip_prefix("def ")
                    .map(|rest| (rest, SymbolKind::Function, "def ".len()))
            })
            .or_else(|| {
                trimmed
                    .strip_prefix("class ")
                    .map(|rest| (rest, SymbolKind::Class, "class ".len()))
            });

        if let Some((rest, kind, prefix_len)) = declaration
            && let Some(name) = leading_identifier(rest)
        {
            let name_start = offset + indentation + prefix_len;
            let name_end = name_start + name.len();
            definition_ranges.insert((name_start, name_end));
            push_definition(
                index,
                &relative_path,
                name.to_owned(),
                kind,
                offset + indentation,
                offset + line.len(),
                line_index + 1,
                line_index + 1,
                name_start,
                name_end,
            );
        }
        offset += raw_line.len();
    }

    for token in python_identifiers(source) {
        if !definition_ranges.contains(&(token.start, token.end)) {
            push_reference(
                index,
                &relative_path,
                token.name.to_owned(),
                token.start,
                token.end,
                token.line,
                false,
            );
        }
    }
}

struct PythonIdentifier<'a> {
    name: &'a str,
    start: usize,
    end: usize,
    line: usize,
}

fn python_identifiers(source: &str) -> Vec<PythonIdentifier<'_>> {
    let bytes = source.as_bytes();
    let mut identifiers = Vec::new();
    let mut index = 0usize;
    let mut line = 1usize;
    let mut quote: Option<u8> = None;
    let mut triple = false;

    while index < bytes.len() {
        let byte = bytes[index];
        if let Some(delimiter) = quote {
            if byte == b'\\' {
                index = (index + 2).min(bytes.len());
                continue;
            }
            if byte == b'\n' {
                line += 1;
            }
            if triple {
                if index + 2 < bytes.len()
                    && bytes[index] == delimiter
                    && bytes[index + 1] == delimiter
                    && bytes[index + 2] == delimiter
                {
                    quote = None;
                    triple = false;
                    index += 3;
                    continue;
                }
            } else if byte == delimiter {
                quote = None;
            }
            index += 1;
            continue;
        }

        if byte == b'#' {
            while index < bytes.len() && bytes[index] != b'\n' {
                index += 1;
            }
            continue;
        }
        if matches!(byte, b'\'' | b'"') {
            triple =
                index + 2 < bytes.len() && bytes[index + 1] == byte && bytes[index + 2] == byte;
            quote = Some(byte);
            index += if triple { 3 } else { 1 };
            continue;
        }
        if byte == b'\n' {
            line += 1;
            index += 1;
            continue;
        }
        if is_identifier_start(byte) {
            let start = index;
            index += 1;
            while index < bytes.len() && is_identifier_continue(bytes[index]) {
                index += 1;
            }
            identifiers.push(PythonIdentifier {
                name: &source[start..index],
                start,
                end: index,
                line,
            });
            continue;
        }
        index += 1;
    }
    identifiers
}

fn leading_identifier(value: &str) -> Option<&str> {
    let bytes = value.as_bytes();
    if bytes
        .first()
        .copied()
        .is_none_or(|byte| !is_identifier_start(byte))
    {
        return None;
    }
    let end = bytes
        .iter()
        .position(|byte| !is_identifier_continue(*byte))
        .unwrap_or(bytes.len());
    Some(&value[..end])
}

fn is_identifier_start(byte: u8) -> bool {
    byte == b'_' || byte.is_ascii_alphabetic()
}

fn is_identifier_continue(byte: u8) -> bool {
    is_identifier_start(byte) || byte.is_ascii_digit()
}

#[allow(clippy::too_many_arguments)]
fn push_definition(
    index: &mut CodeIndex,
    path: &Path,
    name: String,
    kind: SymbolKind,
    start_byte: usize,
    end_byte: usize,
    start_line: usize,
    end_line: usize,
    name_start: usize,
    name_end: usize,
) {
    index.symbols.push(Symbol {
        name: name.clone(),
        kind,
        path: path.to_path_buf(),
        start_byte,
        end_byte,
        start_line,
        end_line,
    });
    push_reference(index, path, name, name_start, name_end, start_line, true);
}

fn push_reference(
    index: &mut CodeIndex,
    path: &Path,
    name: String,
    start_byte: usize,
    end_byte: usize,
    line: usize,
    is_definition: bool,
) {
    index
        .references
        .entry(name.clone())
        .or_default()
        .push(Reference {
            name,
            path: path.to_path_buf(),
            start_byte,
            end_byte,
            line,
            is_definition,
        });
}

fn rust_symbol_kind(kind: &str) -> Option<SymbolKind> {
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

fn is_rust_identifier(kind: &str) -> bool {
    matches!(kind, "identifier" | "type_identifier" | "field_identifier")
}

fn is_rust_definition_name(node: Node<'_>) -> bool {
    node.parent().is_some_and(|parent| {
        rust_symbol_kind(parent.kind()).is_some()
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
    fn python_scanner_ignores_comments_and_strings() {
        let directory = tempfile::tempdir().expect("tempdir");
        fs::write(
            directory.path().join("service.py"),
            "# fake_reference\ntext = \"fake_reference\"\ndef real():\n    return real\n",
        )
        .expect("python");

        let index = CodeIndex::build(directory.path()).expect("index");
        assert!(index.references("fake_reference").is_empty());
        assert_eq!(index.references("real").len(), 2);
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
