use std::{collections::BTreeMap, path::PathBuf};

use serde::{Deserialize, Serialize};

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
