use std::{collections::BTreeMap, path::PathBuf};

use medusa_core::MedusaResult;
use serde::{Deserialize, Serialize};
use time::{format_description::well_known::Rfc3339, OffsetDateTime};

use crate::support::invalid;

/// Validation provenance for a semantic memory claim.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Validation {
    UserStated,
    Observed,
    TestVerified,
    SourceVerified,
    Inferred,
    Unverified,
    Contradicted,
}

impl Validation {
    #[must_use]
    pub fn high_confidence(self) -> bool {
        matches!(
            self,
            Self::UserStated | Self::Observed | Self::TestVerified | Self::SourceVerified
        )
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::UserStated => "user-stated",
            Self::Observed => "observed",
            Self::TestVerified => "test-verified",
            Self::SourceVerified => "source-verified",
            Self::Inferred => "inferred",
            Self::Unverified => "unverified",
            Self::Contradicted => "contradicted",
        }
    }

    fn parse(value: &str) -> MedusaResult<Self> {
        match value {
            "user-stated" => Ok(Self::UserStated),
            "observed" => Ok(Self::Observed),
            "test-verified" => Ok(Self::TestVerified),
            "source-verified" => Ok(Self::SourceVerified),
            "inferred" => Ok(Self::Inferred),
            "unverified" => Ok(Self::Unverified),
            "contradicted" => Ok(Self::Contradicted),
            _ => Err(invalid(format!("unknown memory validation: {value}"))),
        }
    }
}

/// Scope of a memory document.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    Project,
    User,
}

impl Scope {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Project => "project",
            Self::User => "user",
        }
    }

    fn parse(value: &str) -> MedusaResult<Self> {
        match value {
            "project" => Ok(Self::Project),
            "user" => Ok(Self::User),
            _ => Err(invalid(format!("unknown memory scope: {value}"))),
        }
    }
}

/// Lifecycle state for durable memory.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Active,
    Superseded,
    Archived,
}

impl Status {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Superseded => "superseded",
            Self::Archived => "archived",
        }
    }

    fn parse(value: &str) -> MedusaResult<Self> {
        match value {
            "active" => Ok(Self::Active),
            "superseded" => Ok(Self::Superseded),
            "archived" => Ok(Self::Archived),
            _ => Err(invalid(format!("unknown memory status: {value}"))),
        }
    }
}

/// Untrusted model-authored proposal. Canonical memory is written only after validation.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct MemoryProposal {
    pub memory_type: String,
    pub title: String,
    pub claim: String,
    pub evidence: Vec<String>,
    pub confidence_milli: u16,
    pub validation: Validation,
    pub scope: Scope,
    pub project_id: Option<String>,
    pub session_id: Option<String>,
    pub tags: Vec<String>,
}

/// Canonical semantic memory metadata and Markdown body.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct MemoryDocument {
    pub id: String,
    pub memory_type: String,
    pub title: String,
    pub created_at: String,
    pub updated_at: String,
    pub scope: Scope,
    pub project_id: Option<String>,
    pub session_id: Option<String>,
    pub status: Status,
    pub confidence_milli: u16,
    pub validation: Validation,
    pub sources: Vec<String>,
    pub supersedes: Vec<String>,
    pub superseded_by: Vec<String>,
    pub tags: Vec<String>,
    pub expires_at: Option<String>,
    pub last_validated_at: String,
    pub successful_reuse_count: u32,
    pub body: String,
}

impl MemoryDocument {
    /// Renders the portable canonical Markdown representation.
    #[must_use]
    pub fn to_markdown(&self) -> String {
        let mut output = String::new();
        output.push_str("---\n");
        field(&mut output, "id", &self.id);
        field(&mut output, "type", &self.memory_type);
        field(&mut output, "title", &self.title);
        field(&mut output, "created_at", &self.created_at);
        field(&mut output, "updated_at", &self.updated_at);
        field(&mut output, "scope", self.scope.as_str());
        optional_field(&mut output, "project_id", self.project_id.as_deref());
        optional_field(&mut output, "session_id", self.session_id.as_deref());
        field(&mut output, "status", self.status.as_str());
        field(
            &mut output,
            "confidence_milli",
            &self.confidence_milli.to_string(),
        );
        field(&mut output, "validation", self.validation.as_str());
        list_field(&mut output, "sources", &self.sources);
        list_field(&mut output, "supersedes", &self.supersedes);
        list_field(&mut output, "superseded_by", &self.superseded_by);
        list_field(&mut output, "tags", &self.tags);
        optional_field(&mut output, "expires_at", self.expires_at.as_deref());
        field(&mut output, "last_validated_at", &self.last_validated_at);
        field(
            &mut output,
            "successful_reuse_count",
            &self.successful_reuse_count.to_string(),
        );
        output.push_str("---\n\n");
        output.push_str(&self.body);
        if !output.ends_with('\n') {
            output.push('\n');
        }
        output
    }

    /// Parses a Markdown file written by `to_markdown`.
    pub fn from_markdown(text: &str) -> MedusaResult<Self> {
        let rest = text
            .strip_prefix("---\n")
            .ok_or_else(|| invalid("memory document is missing frontmatter"))?;
        let (frontmatter, body) = rest
            .split_once("\n---\n")
            .ok_or_else(|| invalid("memory frontmatter is not terminated"))?;
        let fields = parse_fields(frontmatter)?;
        Ok(Self {
            id: required(&fields, "id")?.to_owned(),
            memory_type: required(&fields, "type")?.to_owned(),
            title: required(&fields, "title")?.to_owned(),
            created_at: required(&fields, "created_at")?.to_owned(),
            updated_at: required(&fields, "updated_at")?.to_owned(),
            scope: Scope::parse(required(&fields, "scope")?)?,
            project_id: optional(&fields, "project_id"),
            session_id: optional(&fields, "session_id"),
            status: Status::parse(required(&fields, "status")?)?,
            confidence_milli: required(&fields, "confidence_milli")?
                .parse()
                .map_err(|_| invalid("confidence_milli must be an integer"))?,
            validation: Validation::parse(required(&fields, "validation")?)?,
            sources: list(&fields, "sources"),
            supersedes: list(&fields, "supersedes"),
            superseded_by: list(&fields, "superseded_by"),
            tags: list(&fields, "tags"),
            expires_at: optional(&fields, "expires_at"),
            last_validated_at: required(&fields, "last_validated_at")?.to_owned(),
            successful_reuse_count: required(&fields, "successful_reuse_count")?
                .parse()
                .map_err(|_| invalid("successful_reuse_count must be an integer"))?,
            body: body.trim_start_matches('\n').to_owned(),
        })
    }

    pub(crate) fn expired(&self, now: OffsetDateTime) -> bool {
        self.expires_at
            .as_deref()
            .and_then(|value| OffsetDateTime::parse(value, &Rfc3339).ok())
            .is_some_and(|expiration| expiration <= now)
    }
}

/// A scored memory retrieval result.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RetrievedMemory {
    pub document: MemoryDocument,
    pub path: PathBuf,
    pub score: i64,
}

fn parse_fields(frontmatter: &str) -> MedusaResult<BTreeMap<String, String>> {
    let mut fields = BTreeMap::new();
    for line in frontmatter.lines() {
        let (key, value) = line
            .split_once(':')
            .ok_or_else(|| invalid(format!("invalid frontmatter line: {line}")))?;
        if fields
            .insert(key.trim().to_owned(), value.trim().to_owned())
            .is_some()
        {
            return Err(invalid(format!("duplicate frontmatter key: {key}")));
        }
    }
    Ok(fields)
}

fn required<'a>(fields: &'a BTreeMap<String, String>, key: &str) -> MedusaResult<&'a str> {
    fields
        .get(key)
        .map(String::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| invalid(format!("missing frontmatter key: {key}")))
}

fn optional(fields: &BTreeMap<String, String>, key: &str) -> Option<String> {
    fields.get(key).filter(|value| !value.is_empty()).cloned()
}

fn list(fields: &BTreeMap<String, String>, key: &str) -> Vec<String> {
    fields
        .get(key)
        .map(|value| {
            value
                .split(',')
                .map(str::trim)
                .filter(|entry| !entry.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn field(output: &mut String, key: &str, value: &str) {
    output.push_str(key);
    output.push_str(": ");
    output.push_str(&escape(value));
    output.push('\n');
}

fn optional_field(output: &mut String, key: &str, value: Option<&str>) {
    field(output, key, value.unwrap_or_default());
}

fn list_field(output: &mut String, key: &str, values: &[String]) {
    field(
        output,
        key,
        &values
            .iter()
            .map(|value| escape(value))
            .collect::<Vec<_>>()
            .join(", "),
    );
}

fn escape(value: &str) -> String {
    value.replace('\n', "\\n").replace('\r', "")
}
