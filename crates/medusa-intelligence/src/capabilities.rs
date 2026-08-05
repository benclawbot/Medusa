use serde::{Deserialize, Serialize};

/// One independently reportable language-intelligence capability.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LanguageCapabilityLevel {
    TextOnly,
    ParsedSymbols,
    Definitions,
    References,
    Diagnostics,
    WorkspaceSymbols,
    GuardedRefactoring,
}

/// Truth status for one capability through a production entrypoint.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LanguageCapabilityStatus {
    Production,
    Partial,
    Unavailable,
}

/// Exact claim for one language capability level.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LanguageCapabilityClaim {
    pub level: LanguageCapabilityLevel,
    pub status: LanguageCapabilityStatus,
    pub detail: String,
    pub production_entrypoint: Option<String>,
}

/// Truthful production profile for one language adapter.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LanguageCapabilityProfile {
    pub language: String,
    pub adapter: String,
    pub claims: Vec<LanguageCapabilityClaim>,
    pub evidence: Vec<String>,
}

/// Returns the exact language-intelligence levels currently reachable through production tools.
#[must_use]
pub fn language_capability_profiles() -> Vec<LanguageCapabilityProfile> {
    vec![
        rust_profile(),
        python_profile(),
        typescript_javascript_profile(),
    ]
}

fn claim(
    level: LanguageCapabilityLevel,
    status: LanguageCapabilityStatus,
    detail: &str,
    production_entrypoint: Option<&str>,
) -> LanguageCapabilityClaim {
    LanguageCapabilityClaim {
        level,
        status,
        detail: detail.to_owned(),
        production_entrypoint: production_entrypoint.map(str::to_owned),
    }
}

fn rust_profile() -> LanguageCapabilityProfile {
    LanguageCapabilityProfile {
        language: "rust".into(),
        adapter: "tree-sitter-rust static index".into(),
        claims: vec![
            claim(
                LanguageCapabilityLevel::TextOnly,
                LanguageCapabilityStatus::Production,
                "bounded UTF-8 repository search",
                Some("medusa-agent::tools::filesystem::search"),
            ),
            claim(
                LanguageCapabilityLevel::ParsedSymbols,
                LanguageCapabilityStatus::Production,
                "tree-sitter declarations with deterministic repository paths and byte ranges",
                Some("medusa-agent::tools::intelligence::code_index"),
            ),
            claim(
                LanguageCapabilityLevel::Definitions,
                LanguageCapabilityStatus::Partial,
                "exact-name syntax declarations; no type-directed target resolution",
                Some("medusa-agent::tools::intelligence::code_index"),
            ),
            claim(
                LanguageCapabilityLevel::References,
                LanguageCapabilityStatus::Partial,
                "syntax-token references; same-name symbols in different scopes are not resolved",
                Some("medusa-agent::tools::intelligence::code_index"),
            ),
            claim(
                LanguageCapabilityLevel::Diagnostics,
                LanguageCapabilityStatus::Unavailable,
                "parse-error paths are reported, but compiler diagnostics are not dispatched",
                None,
            ),
            claim(
                LanguageCapabilityLevel::WorkspaceSymbols,
                LanguageCapabilityStatus::Partial,
                "repository-wide exact-name symbol queries are available through code_index",
                Some("medusa-agent::tools::intelligence::code_index"),
            ),
            claim(
                LanguageCapabilityLevel::GuardedRefactoring,
                LanguageCapabilityStatus::Partial,
                "single-definition lexical rename only; ambiguity, parse errors, stale bytes, and cross-language matches fail closed",
                Some("medusa-agent::tools::intelligence::symbol_rename"),
            ),
        ],
        evidence: vec![
            "crates/medusa-intelligence/src/index.rs".into(),
            "crates/medusa-intelligence/src/patch.rs".into(),
            "crates/medusa-agent/src/tools/intelligence.rs".into(),
        ],
    }
}

fn python_profile() -> LanguageCapabilityProfile {
    LanguageCapabilityProfile {
        language: "python".into(),
        adapter: "bounded lexical scanner".into(),
        claims: vec![
            claim(
                LanguageCapabilityLevel::TextOnly,
                LanguageCapabilityStatus::Production,
                "bounded UTF-8 repository search",
                Some("medusa-agent::tools::filesystem::search"),
            ),
            claim(
                LanguageCapabilityLevel::ParsedSymbols,
                LanguageCapabilityStatus::Partial,
                "def, async def, and class declarations are recognized lexically rather than by a Python parser",
                Some("medusa-agent::tools::intelligence::code_index"),
            ),
            claim(
                LanguageCapabilityLevel::Definitions,
                LanguageCapabilityStatus::Partial,
                "lexically recognized declarations only",
                Some("medusa-agent::tools::intelligence::code_index"),
            ),
            claim(
                LanguageCapabilityLevel::References,
                LanguageCapabilityStatus::Partial,
                "comment- and string-aware identifier occurrences without semantic resolution",
                Some("medusa-agent::tools::intelligence::code_index"),
            ),
            claim(
                LanguageCapabilityLevel::Diagnostics,
                LanguageCapabilityStatus::Unavailable,
                "no Python parser or language-server diagnostics are dispatched",
                None,
            ),
            claim(
                LanguageCapabilityLevel::WorkspaceSymbols,
                LanguageCapabilityStatus::Partial,
                "repository-wide exact-name queries over lexical declarations",
                Some("medusa-agent::tools::intelligence::code_index"),
            ),
            claim(
                LanguageCapabilityLevel::GuardedRefactoring,
                LanguageCapabilityStatus::Unavailable,
                "Python rename is withheld because lexical occurrences cannot prove semantic identity",
                None,
            ),
        ],
        evidence: vec![
            "crates/medusa-intelligence/src/index.rs".into(),
            "crates/medusa-agent/src/tools/intelligence.rs".into(),
        ],
    }
}

fn typescript_javascript_profile() -> LanguageCapabilityProfile {
    let unavailable =
        |level, detail| claim(level, LanguageCapabilityStatus::Unavailable, detail, None);
    LanguageCapabilityProfile {
        language: "typescript_javascript".into(),
        adapter: "LSP primitives without a certified production dispatcher".into(),
        claims: vec![
            claim(
                LanguageCapabilityLevel::TextOnly,
                LanguageCapabilityStatus::Production,
                "bounded UTF-8 repository search",
                Some("medusa-agent::tools::filesystem::search"),
            ),
            unavailable(
                LanguageCapabilityLevel::ParsedSymbols,
                "no TypeScript/JavaScript parser-backed production tool is registered",
            ),
            unavailable(
                LanguageCapabilityLevel::Definitions,
                "LSP normalization exists in medusa-intelligence but has no production lifecycle owner",
            ),
            unavailable(
                LanguageCapabilityLevel::References,
                "LSP normalization exists in medusa-intelligence but has no production lifecycle owner",
            ),
            unavailable(
                LanguageCapabilityLevel::Diagnostics,
                "diagnostic normalization exists but no server process is discovered or dispatched",
            ),
            unavailable(
                LanguageCapabilityLevel::WorkspaceSymbols,
                "workspace-symbol normalization exists but no production handler is registered",
            ),
            unavailable(
                LanguageCapabilityLevel::GuardedRefactoring,
                "workspace-edit normalization is not connected to the v2 review and verification transaction",
            ),
        ],
        evidence: vec![
            "crates/medusa-intelligence/src/lsp.rs".into(),
            "crates/medusa-intelligence/src/lsp_navigation.rs".into(),
            "crates/medusa-intelligence/src/lsp_actions.rs".into(),
            "crates/medusa-intelligence/src/lsp_semantics.rs".into(),
            "no medusa-agent TypeScript/JavaScript dispatch route".into(),
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profiles_are_deterministic_and_do_not_overclaim_typescript() {
        let profiles = language_capability_profiles();
        assert_eq!(
            profiles
                .iter()
                .map(|profile| profile.language.as_str())
                .collect::<Vec<_>>(),
            vec!["rust", "python", "typescript_javascript"]
        );
        let typescript = profiles
            .iter()
            .find(|profile| profile.language == "typescript_javascript")
            .expect("typescript profile");
        assert!(typescript.claims.iter().all(|claim| {
            claim.level == LanguageCapabilityLevel::TextOnly
                || claim.status == LanguageCapabilityStatus::Unavailable
        }));
    }
}
