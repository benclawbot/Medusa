//! Canonical Markdown memory with validated proposals and a rebuildable SQLite index.

use std::{
    cmp::Reverse,
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use ulid::Ulid;
use walkdir::WalkDir;

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

    fn as_str(self) -> &'static str {
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
    fn as_str(self) -> &'static str {
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
    fn as_str(self) -> &'static str {
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

    fn expired(&self, now: OffsetDateTime) -> bool {
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

/// Project-local memory engine.
pub struct MemoryEngine {
    root: PathBuf,
    index_path: PathBuf,
}

impl MemoryEngine {
    pub fn new(project_root: impl Into<PathBuf>) -> MedusaResult<Self> {
        let root = project_root.into().join(".medusa/memory");
        fs::create_dir_all(root.join("proposals"))?;
        fs::create_dir_all(root.join("archive"))?;
        fs::create_dir_all(root.join("lessons"))?;
        fs::create_dir_all(root.join("patterns"))?;
        fs::create_dir_all(root.join("failures"))?;
        fs::create_dir_all(root.join("decisions"))?;
        let engine = Self {
            index_path: root.join("index.sqlite3"),
            root,
        };
        engine.initialize_layout()?;
        Ok(engine)
    }

    /// Validates an untrusted proposal before any canonical write.
    pub fn validate_proposal(&self, proposal: &MemoryProposal) -> MedusaResult<()> {
        if proposal.memory_type.trim().is_empty()
            || proposal.title.trim().is_empty()
            || proposal.claim.trim().is_empty()
        {
            return Err(invalid("memory type, title, and claim are required"));
        }
        if proposal.confidence_milli > 1_000 {
            return Err(invalid("confidence_milli must be at most 1000"));
        }
        if proposal.evidence.is_empty() {
            return Err(invalid("memory proposal requires provenance evidence"));
        }
        if proposal.scope == Scope::Project && proposal.project_id.is_none() {
            return Err(invalid("project memory requires project_id"));
        }
        let serialized = serde_json::to_string(proposal)?;
        if contains_secret(&serialized) {
            return Err(MedusaError::new(
                ErrorCode::PolicyDenied,
                ErrorCategory::Policy,
                "memory proposal appears to contain a secret",
            ));
        }
        if !proposal.validation.high_confidence() && proposal.confidence_milli >= 800 {
            return Err(invalid(
                "unverified or inferred memory cannot claim high confidence",
            ));
        }
        Ok(())
    }

    /// Stores a proposal for review without modifying canonical memory.
    pub fn propose(&self, proposal: &MemoryProposal) -> MedusaResult<PathBuf> {
        self.validate_proposal(proposal)?;
        let path = self
            .root
            .join("proposals")
            .join(format!("proposal-{}.json", Ulid::new()));
        atomic_write(&path, &serde_json::to_vec_pretty(proposal)?)?;
        Ok(path)
    }

    /// Commits a validated proposal to canonical Markdown and refreshes the index.
    pub fn commit_proposal(&self, proposal: &MemoryProposal) -> MedusaResult<MemoryDocument> {
        self.validate_proposal(proposal)?;
        let duplicate = self.documents()?.into_iter().find(|(_, document)| {
            document.status == Status::Active
                && normalize(&document.title) == normalize(&proposal.title)
                && normalize(&document.body).contains(&normalize(&proposal.claim))
        });
        if duplicate.is_some() {
            return Err(invalid("duplicate active memory claim"));
        }
        let now = OffsetDateTime::now_utc()
            .format(&Rfc3339)
            .map_err(|error| internal(error.to_string()))?;
        let id = format!(
            "{}-{}",
            sanitize_component(&proposal.memory_type),
            Ulid::new()
        );
        let document = MemoryDocument {
            id: id.clone(),
            memory_type: proposal.memory_type.clone(),
            title: proposal.title.clone(),
            created_at: now.clone(),
            updated_at: now.clone(),
            scope: proposal.scope,
            project_id: proposal.project_id.clone(),
            session_id: proposal.session_id.clone(),
            status: Status::Active,
            confidence_milli: proposal.confidence_milli,
            validation: proposal.validation,
            sources: proposal.evidence.clone(),
            supersedes: Vec::new(),
            superseded_by: Vec::new(),
            tags: deduplicate(proposal.tags.clone()),
            expires_at: None,
            last_validated_at: now,
            successful_reuse_count: 0,
            body: format!("# {}\n\n{}\n", proposal.title, proposal.claim),
        };
        let path = self.path_for(&document);
        atomic_write(&path, document.to_markdown().as_bytes())?;
        self.rebuild_index()?;
        Ok(document)
    }

    /// Rebuilds the complete machine index exclusively from canonical Markdown.
    pub fn rebuild_index(&self) -> MedusaResult<()> {
        if self.index_path.exists() {
            fs::remove_file(&self.index_path)?;
        }
        let connection = Connection::open(&self.index_path).map_err(sql_error)?;
        create_schema(&connection)?;
        for (path, document) in self.documents()? {
            connection
                .execute(
                    "INSERT INTO memory_documents
                     (id, path, type, title, body, scope, status, confidence_milli, validation,
                      updated_at, expires_at, successful_reuse_count)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                    params![
                        document.id,
                        path.to_string_lossy(),
                        document.memory_type,
                        document.title,
                        document.body,
                        document.scope.as_str(),
                        document.status.as_str(),
                        document.confidence_milli,
                        document.validation.as_str(),
                        document.updated_at,
                        document.expires_at,
                        document.successful_reuse_count,
                    ],
                )
                .map_err(sql_error)?;
            for tag in &document.tags {
                connection
                    .execute(
                        "INSERT INTO memory_tags (document_id, tag) VALUES (?1, ?2)",
                        params![document.id, tag],
                    )
                    .map_err(sql_error)?;
            }
            for source in &document.sources {
                connection
                    .execute(
                        "INSERT INTO memory_validation (document_id, source) VALUES (?1, ?2)",
                        params![document.id, source],
                    )
                    .map_err(sql_error)?;
            }
            for target in &document.supersedes {
                connection
                    .execute(
                        "INSERT INTO memory_links (source_id, target_id, relation)
                         VALUES (?1, ?2, 'supersedes')",
                        params![document.id, target],
                    )
                    .map_err(sql_error)?;
            }
        }
        Ok(())
    }

    /// Retrieves only active, non-expired, high-confidence memory by deterministic score.
    pub fn search(
        &self,
        query: &str,
        scope: Scope,
        limit: usize,
    ) -> MedusaResult<Vec<RetrievedMemory>> {
        let terms = tokenize(query);
        let now = OffsetDateTime::now_utc();
        let mut results = self
            .documents()?
            .into_iter()
            .filter(|(_, document)| {
                document.scope == scope
                    && document.status == Status::Active
                    && document.validation.high_confidence()
                    && !document.expired(now)
            })
            .filter_map(|(path, document)| {
                let score = score(&document, &terms);
                (score > 0).then_some(RetrievedMemory {
                    document,
                    path,
                    score,
                })
            })
            .collect::<Vec<_>>();
        results.sort_by_key(|result| {
            (
                Reverse(result.score),
                result.document.id.clone(),
                result.path.clone(),
            )
        });
        results.truncate(limit);
        Ok(results)
    }

    /// Records a successful reuse as durable Markdown evidence.
    pub fn record_reuse(&self, id: &str, evidence: &str) -> MedusaResult<()> {
        if evidence.trim().is_empty() {
            return Err(invalid("reuse evidence cannot be empty"));
        }
        let (path, mut document) = self.read_by_id(id)?;
        document.successful_reuse_count = document.successful_reuse_count.saturating_add(1);
        document.updated_at = OffsetDateTime::now_utc()
            .format(&Rfc3339)
            .map_err(|error| internal(error.to_string()))?;
        document.sources.push(evidence.to_owned());
        document.sources = deduplicate(document.sources);
        atomic_write(&path, document.to_markdown().as_bytes())?;
        self.rebuild_index()
    }

    /// Supersedes an active document while preserving both records for audit.
    pub fn supersede(&self, old_id: &str, new_id: &str) -> MedusaResult<()> {
        if old_id == new_id {
            return Err(invalid("memory cannot supersede itself"));
        }
        let (old_path, mut old_document) = self.read_by_id(old_id)?;
        let (new_path, mut new_document) = self.read_by_id(new_id)?;
        if old_document.status != Status::Active || new_document.status != Status::Active {
            return Err(invalid("supersession requires active documents"));
        }
        old_document.status = Status::Superseded;
        old_document.superseded_by.push(new_id.to_owned());
        old_document.superseded_by = deduplicate(old_document.superseded_by);
        new_document.supersedes.push(old_id.to_owned());
        new_document.supersedes = deduplicate(new_document.supersedes);
        let now = OffsetDateTime::now_utc()
            .format(&Rfc3339)
            .map_err(|error| internal(error.to_string()))?;
        old_document.updated_at.clone_from(&now);
        new_document.updated_at = now;
        atomic_write(&old_path, old_document.to_markdown().as_bytes())?;
        atomic_write(&new_path, new_document.to_markdown().as_bytes())?;
        self.rebuild_index()
    }

    /// Compacts selected active documents into a summary without deleting source memory.
    pub fn compact(&self, ids: &[String], title: &str) -> MedusaResult<MemoryDocument> {
        if ids.len() < 2 {
            return Err(invalid("compaction requires at least two documents"));
        }
        let mut claims = Vec::new();
        let mut sources = Vec::new();
        let mut tags = Vec::new();
        let mut confidence = 1_000_u16;
        let mut project_id = None;
        for id in ids {
            let (_, document) = self.read_by_id(id)?;
            if document.status != Status::Active || !document.validation.high_confidence() {
                return Err(invalid("only active validated memory may be compacted"));
            }
            claims.push(format!(
                "- {}: {}",
                document.title,
                first_claim(&document.body)
            ));
            sources.push(format!("memory://{}", document.id));
            tags.extend(document.tags);
            confidence = confidence.min(document.confidence_milli);
            project_id = project_id.or(document.project_id);
        }
        let proposal = MemoryProposal {
            memory_type: "summary".into(),
            title: title.into(),
            claim: format!("Compacted validated memory:\n{}", claims.join("\n")),
            evidence: sources,
            confidence_milli: confidence,
            validation: Validation::Observed,
            scope: Scope::Project,
            project_id,
            session_id: None,
            tags,
        };
        self.commit_proposal(&proposal)
    }

    fn initialize_layout(&self) -> MedusaResult<()> {
        let readme = self.root.join("README.md");
        if !readme.exists() {
            atomic_write(
                &readme,
                b"# Medusa Memory\n\nCanonical semantic memory is Markdown. The SQLite index is disposable and rebuildable.\n",
            )?;
        }
        if !self.index_path.exists() {
            self.rebuild_index()?;
        }
        Ok(())
    }

    fn documents(&self) -> MedusaResult<Vec<(PathBuf, MemoryDocument)>> {
        let mut documents = Vec::new();
        for entry in WalkDir::new(&self.root).into_iter().filter_map(Result::ok) {
            if !entry.file_type().is_file()
                || entry
                    .path()
                    .extension()
                    .is_none_or(|extension| extension != "md")
                || entry
                    .path()
                    .file_name()
                    .is_some_and(|name| name == "README.md")
                || entry
                    .path()
                    .components()
                    .any(|component| component.as_os_str() == "archive")
            {
                continue;
            }
            let text = fs::read_to_string(entry.path())?;
            documents.push((
                entry.path().to_path_buf(),
                MemoryDocument::from_markdown(&text)?,
            ));
        }
        documents.sort_by(|left, right| left.0.cmp(&right.0));
        Ok(documents)
    }

    fn read_by_id(&self, id: &str) -> MedusaResult<(PathBuf, MemoryDocument)> {
        self.documents()?
            .into_iter()
            .find(|(_, document)| document.id == id)
            .ok_or_else(|| invalid(format!("memory document not found: {id}")))
    }

    fn path_for(&self, document: &MemoryDocument) -> PathBuf {
        let directory = match document.memory_type.as_str() {
            "lesson" | "command" => "lessons",
            "failure" => "failures",
            "pattern" => "patterns",
            "decision" => "decisions",
            "summary" => "summaries",
            _ => "entities",
        };
        self.root
            .join(directory)
            .join(format!("{}.md", sanitize_component(&document.id)))
    }
}

fn create_schema(connection: &Connection) -> MedusaResult<()> {
    connection
        .execute_batch(
            "PRAGMA journal_mode=WAL;
             CREATE TABLE memory_documents (
               id TEXT PRIMARY KEY,
               path TEXT NOT NULL,
               type TEXT NOT NULL,
               title TEXT NOT NULL,
               body TEXT NOT NULL,
               scope TEXT NOT NULL,
               status TEXT NOT NULL,
               confidence_milli INTEGER NOT NULL,
               validation TEXT NOT NULL,
               updated_at TEXT NOT NULL,
               expires_at TEXT,
               successful_reuse_count INTEGER NOT NULL
             );
             CREATE TABLE memory_chunks (
               document_id TEXT NOT NULL,
               ordinal INTEGER NOT NULL,
               content TEXT NOT NULL,
               PRIMARY KEY (document_id, ordinal)
             );
             CREATE TABLE memory_links (
               source_id TEXT NOT NULL,
               target_id TEXT NOT NULL,
               relation TEXT NOT NULL
             );
             CREATE TABLE memory_tags (
               document_id TEXT NOT NULL,
               tag TEXT NOT NULL
             );
             CREATE TABLE memory_validation (
               document_id TEXT NOT NULL,
               source TEXT NOT NULL
             );",
        )
        .map_err(sql_error)
}

fn score(document: &MemoryDocument, terms: &[String]) -> i64 {
    let title = normalize(&document.title);
    let body = normalize(&document.body);
    let tags = document
        .tags
        .iter()
        .map(|tag| normalize(tag))
        .collect::<Vec<_>>();
    let mut score = i64::from(document.confidence_milli) / 10;
    score += i64::from(document.successful_reuse_count) * 25;
    score += match document.validation {
        Validation::TestVerified => 80,
        Validation::UserStated | Validation::SourceVerified => 70,
        Validation::Observed => 60,
        _ => -500,
    };
    for term in terms {
        if title.contains(term) {
            score += 120;
        }
        if body.contains(term) {
            score += 60;
        }
        if tags.iter().any(|tag| tag.contains(term)) {
            score += 90;
        }
    }
    if terms.is_empty() { 0 } else { score }
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

fn atomic_write(path: &Path, bytes: &[u8]) -> MedusaResult<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension("tmp");
    fs::write(&temporary, bytes)?;
    fs::rename(temporary, path)?;
    Ok(())
}

fn contains_secret(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    [
        "api_key=",
        "api-key:",
        "authorization: bearer",
        "private key-----",
        "secret_access_key",
        "ghp_",
        "sk-",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

fn tokenize(value: &str) -> Vec<String> {
    normalize(value)
        .split_whitespace()
        .map(str::to_owned)
        .collect()
}

fn normalize(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_lowercase()
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn sanitize_component(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '-' || character == '_' {
                character
            } else {
                '-'
            }
        })
        .collect()
}

fn deduplicate(values: Vec<String>) -> Vec<String> {
    values
        .into_iter()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn first_claim(body: &str) -> String {
    body.lines()
        .find(|line| !line.trim().is_empty() && !line.starts_with('#'))
        .unwrap_or_default()
        .trim()
        .to_owned()
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

fn sql_error(error: rusqlite::Error) -> MedusaError {
    MedusaError::new(
        ErrorCode::PersistenceFailed,
        ErrorCategory::Persistence,
        error.to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proposal(title: &str, claim: &str) -> MemoryProposal {
        MemoryProposal {
            memory_type: "command".into(),
            title: title.into(),
            claim: claim.into(),
            evidence: vec!["artifact://sessions/ses-test/verification-1".into()],
            confidence_milli: 950,
            validation: Validation::TestVerified,
            scope: Scope::Project,
            project_id: Some("sha256:test-project".into()),
            session_id: Some("ses-test".into()),
            tags: vec!["rust".into(), "testing".into()],
        }
    }

    #[test]
    fn frontmatter_round_trips() {
        let directory = tempfile::tempdir().expect("tempdir");
        let engine = MemoryEngine::new(directory.path()).expect("engine");
        let document = engine
            .commit_proposal(&proposal(
                "Run workspace tests",
                "Use `cargo test --workspace`.",
            ))
            .expect("commit");
        assert_eq!(
            MemoryDocument::from_markdown(&document.to_markdown()).expect("parse"),
            document
        );
    }

    #[test]
    fn validated_command_is_reused_in_later_session() {
        let directory = tempfile::tempdir().expect("tempdir");
        let first_session = MemoryEngine::new(directory.path()).expect("first session");
        let proposal = proposal(
            "Validated workspace test command",
            "The verified command is `cargo test --workspace --all-features` from repository root.",
        );
        let proposal_path = first_session.propose(&proposal).expect("proposal");
        assert!(proposal_path.exists());
        let committed = first_session.commit_proposal(&proposal).expect("commit");
        drop(first_session);

        let later_session = MemoryEngine::new(directory.path()).expect("later session");
        later_session.rebuild_index().expect("rebuild index");
        let results = later_session
            .search("workspace test command", Scope::Project, 5)
            .expect("retrieve");
        assert_eq!(results.first().expect("memory").document.id, committed.id);
        assert!(
            results[0]
                .document
                .body
                .contains("cargo test --workspace --all-features")
        );
        later_session
            .record_reuse(
                &committed.id,
                "artifact://sessions/ses-later/verification-2",
            )
            .expect("reuse");
        let reused = later_session
            .search("cargo test workspace", Scope::Project, 1)
            .expect("search after reuse");
        assert_eq!(reused[0].document.successful_reuse_count, 1);
    }

    #[test]
    fn supersession_removes_old_claim_from_retrieval() {
        let directory = tempfile::tempdir().expect("tempdir");
        let engine = MemoryEngine::new(directory.path()).expect("engine");
        let old = engine
            .commit_proposal(&proposal("Old test command", "Use `cargo test`."))
            .expect("old");
        let new = engine
            .commit_proposal(&proposal(
                "New test command",
                "Use `cargo test --workspace --all-features`.",
            ))
            .expect("new");
        engine.supersede(&old.id, &new.id).expect("supersede");
        let results = engine
            .search("test command", Scope::Project, 10)
            .expect("search");
        assert!(results.iter().all(|result| result.document.id != old.id));
        assert!(results.iter().any(|result| result.document.id == new.id));
    }

    #[test]
    fn compaction_preserves_source_documents() {
        let directory = tempfile::tempdir().expect("tempdir");
        let engine = MemoryEngine::new(directory.path()).expect("engine");
        let first = engine
            .commit_proposal(&proposal("Command one", "Use command one."))
            .expect("first");
        let second = engine
            .commit_proposal(&proposal("Command two", "Use command two."))
            .expect("second");
        let summary = engine
            .compact(&[first.id.clone(), second.id.clone()], "Command summary")
            .expect("compact");
        assert!(summary.body.contains("Command one"));
        assert!(summary.body.contains("Command two"));
        assert!(engine.read_by_id(&first.id).is_ok());
        assert!(engine.read_by_id(&second.id).is_ok());
    }

    #[test]
    fn secret_like_proposal_is_rejected() {
        let directory = tempfile::tempdir().expect("tempdir");
        let engine = MemoryEngine::new(directory.path()).expect("engine");
        let mut unsafe_proposal = proposal("Credential", "api_key=sk-example-secret");
        unsafe_proposal.validation = Validation::Observed;
        assert!(engine.commit_proposal(&unsafe_proposal).is_err());
    }
}
