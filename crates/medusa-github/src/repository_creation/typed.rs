use std::{collections::BTreeMap, path::PathBuf};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::*;

pub const GITHUB_OPERATION_SCHEMA_VERSION: u16 = 2;
pub const DEFAULT_GITHUB_API_VERSION: &str = "2022-11-28";
const DEFAULT_MAX_RESPONSE_BYTES: usize = 1_048_576;
const MAX_RESPONSE_BYTES: usize = 4_194_304;
const MAX_ARTIFACT_BYTES: u64 = 512 * 1_048_576;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GitHubExecutionBackend {
    NativeCli,
    GhApi,
    DirectOauth,
}

impl Default for GitHubExecutionBackend {
    fn default() -> Self {
        Self::NativeCli
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GitHubOperationDocument {
    pub schema_version: u16,
    pub repository: String,
    #[serde(default = "default_hostname")]
    pub hostname: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_base_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oauth_base_url: Option<String>,
    #[serde(default = "default_api_version")]
    pub api_version: String,
    #[serde(default)]
    pub backend: GitHubExecutionBackend,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
    #[serde(default = "default_response_limit")]
    pub max_response_bytes: usize,
    pub operation: GitHubTypedOperation,
}

impl GitHubOperationDocument {
    pub fn validate(&self) -> MedusaResult<()> {
        if self.schema_version != GITHUB_OPERATION_SCHEMA_VERSION {
            return Err(invalid(format!(
                "unsupported GitHub operation schema version {}; expected {}",
                self.schema_version, GITHUB_OPERATION_SCHEMA_VERSION
            )));
        }
        let prepared = self.prepare()?;
        if let Some(key) = self.idempotency_key.as_deref() {
            validate_idempotency_key(key)?;
        }
        validate_base_url("api_base_url", self.api_base_url.as_deref())?;
        validate_base_url("oauth_base_url", self.oauth_base_url.as_deref())?;
        validate_api_version(&self.api_version)?;
        if self.max_response_bytes == 0 || self.max_response_bytes > MAX_RESPONSE_BYTES {
            return Err(invalid(format!(
                "max_response_bytes must be between 1 and {MAX_RESPONSE_BYTES}"
            )));
        }
        match &prepared.execution {
            PreparedExecution::Api(request) => request.validate(),
            PreparedExecution::RepositoryCreate(request) => request.validate(),
            PreparedExecution::ArtifactDownload(transfer) => transfer.validate(),
            PreparedExecution::ReleaseAssetUpload(transfer) => transfer.validate(),
        }
    }

    pub fn prepare(&self) -> MedusaResult<PreparedGitHubOperation> {
        super::typed_prepare::prepare(self)
    }

    pub fn risk(&self) -> MedusaResult<GitHubOperationRisk> {
        Ok(self.prepare()?.risk)
    }

    pub fn request_digest(&self) -> MedusaResult<String> {
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct DigestMaterial<'a> {
            schema_version: u16,
            repository: &'a str,
            hostname: &'a str,
            api_base_url: &'a Option<String>,
            oauth_base_url: &'a Option<String>,
            api_version: &'a str,
            operation: &'a GitHubTypedOperation,
        }
        let material = DigestMaterial {
            schema_version: self.schema_version,
            repository: &self.repository,
            hostname: &self.hostname,
            api_base_url: &self.api_base_url,
            oauth_base_url: &self.oauth_base_url,
            api_version: &self.api_version,
            operation: &self.operation,
        };
        let encoded = serde_json::to_vec(&material).map_err(json_error)?;
        Ok(hex::encode(Sha256::digest(encoded)))
    }

    #[must_use]
    pub fn idempotency_key_hash(&self) -> Option<String> {
        self.idempotency_key
            .as_ref()
            .map(|value| hex::encode(Sha256::digest(value.as_bytes())))
    }

    pub fn redacted_preview(&self) -> MedusaResult<Value> {
        let mut value = serde_json::to_value(self).map_err(json_error)?;
        redact_preview(&mut value, false);
        Ok(value)
    }

    #[must_use]
    pub fn api_base_url(&self) -> String {
        self.api_base_url.clone().unwrap_or_else(|| {
            if self.hostname == "github.com" {
                "https://api.github.com".into()
            } else {
                format!("https://{}/api/v3", self.hostname)
            }
        })
    }

    #[must_use]
    pub fn oauth_base_url(&self) -> String {
        self.oauth_base_url
            .clone()
            .unwrap_or_else(|| format!("https://{}", self.hostname))
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "resource", content = "operation", rename_all = "snake_case")]
pub enum GitHubTypedOperation {
    Repository(RepositoryOperation),
    Contents(ContentsOperation),
    GitData(GitDataOperation),
    Issues(IssuesOperation),
    PullRequests(PullRequestsOperation),
    Actions(ActionsOperation),
    Releases(ReleasesOperation),
    Collaborators(CollaboratorsOperation),
    Environments(EnvironmentsOperation),
    Variables(VariablesOperation),
    Secrets(SecretsOperation),
    Webhooks(WebhooksOperation),
    BranchProtection(BranchProtectionOperation),
    Projects(ProjectsOperation),
    Search(SearchOperation),
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum RepositoryOperation {
    Get,
    Update {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        description: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        homepage: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        visibility: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        default_branch: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        has_issues: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        has_wiki: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        archived: Option<bool>,
    },
    Delete,
    GetTopics,
    ReplaceTopics { names: Vec<String> },
    Create { request: RepositoryCreateRequest },
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum ContentsOperation {
    Get {
        path: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reference: Option<String>,
    },
    Put {
        path: String,
        message: String,
        content_base64: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        branch: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sha: Option<String>,
    },
    Delete {
        path: String,
        message: String,
        sha: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        branch: Option<String>,
    },
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum GitDataOperation {
    ListBranches {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        protected: Option<bool>,
        #[serde(default = "default_page_size")]
        per_page: u16,
    },
    GetBranch { branch: String },
    CreateReference { reference: String, sha: String },
    UpdateReference {
        reference: String,
        sha: String,
        #[serde(default)]
        force: bool,
    },
    DeleteReference { reference: String },
    GetCommit { sha: String },
    ListCommits {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sha: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        path: Option<String>,
        #[serde(default = "default_page_size")]
        per_page: u16,
    },
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum IssuesOperation {
    List {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        state: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        labels: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        assignee: Option<String>,
        #[serde(default = "default_page_size")]
        per_page: u16,
    },
    Get { number: u64 },
    Create {
        title: String,
        #[serde(default)]
        body: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        labels: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        assignees: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        milestone: Option<u64>,
    },
    Update {
        number: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        body: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        state: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        state_reason: Option<String>,
    },
    Comment { number: u64, body: String },
    AddLabels { number: u64, labels: Vec<String> },
    SetAssignees { number: u64, assignees: Vec<String> },
    Lock {
        number: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    Unlock { number: u64 },
    DeleteComment { comment_id: u64 },
    ReactToComment { comment_id: u64, content: String },
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PullRequestReviewEvent {
    Approve,
    Comment,
    RequestChanges,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum PullRequestsOperation {
    List {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        state: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        head: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        base: Option<String>,
        #[serde(default = "default_page_size")]
        per_page: u16,
    },
    Get { number: u64 },
    Create {
        title: String,
        body: String,
        head: String,
        base: String,
        #[serde(default)]
        draft: bool,
    },
    Update {
        number: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        body: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        state: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        base: Option<String>,
    },
    Review {
        number: u64,
        event: PullRequestReviewEvent,
        #[serde(default)]
        body: String,
    },
    Merge {
        number: u64,
        strategy: MergeStrategy,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        commit_title: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        commit_message: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expected_head_sha: Option<String>,
    },
    Close { number: u64 },
    RequestReviewers {
        number: u64,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        reviewers: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        team_reviewers: Vec<String>,
    },
    RemoveReviewers {
        number: u64,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        reviewers: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        team_reviewers: Vec<String>,
    },
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum ActionsOperation {
    ListRuns {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workflow_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        branch: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        status: Option<String>,
        #[serde(default = "default_page_size")]
        per_page: u16,
    },
    GetRun { run_id: u64 },
    ListJobs { run_id: u64 },
    GetJob { job_id: u64 },
    Rerun { run_id: u64 },
    RerunFailed { run_id: u64 },
    Cancel { run_id: u64 },
    DownloadArtifact {
        artifact_id: u64,
        destination: PathBuf,
        #[serde(default = "default_artifact_limit")]
        max_bytes: u64,
    },
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum ReleasesOperation {
    List { #[serde(default = "default_page_size")] per_page: u16 },
    Get { release_id: u64 },
    GetByTag { tag: String },
    Create {
        tag: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target_commitish: Option<String>,
        name: String,
        body: String,
        #[serde(default)]
        draft: bool,
        #[serde(default)]
        prerelease: bool,
        #[serde(default)]
        generate_release_notes: bool,
    },
    Update {
        release_id: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tag: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        body: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        draft: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        prerelease: Option<bool>,
    },
    Delete { release_id: u64 },
    UploadAsset {
        release_id: u64,
        path: PathBuf,
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        content_type: String,
        #[serde(default = "default_artifact_limit")]
        max_bytes: u64,
    },
    DownloadAsset {
        asset_id: u64,
        destination: PathBuf,
        #[serde(default = "default_artifact_limit")]
        max_bytes: u64,
    },
    DeleteAsset { asset_id: u64 },
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum CollaboratorsOperation {
    List { #[serde(default = "default_page_size")] per_page: u16 },
    GetPermission { username: String },
    Add { username: String, permission: String },
    Remove { username: String },
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum EnvironmentsOperation {
    List { #[serde(default = "default_page_size")] per_page: u16 },
    Get { name: String },
    Put {
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        wait_timer: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        prevent_self_review: Option<bool>,
    },
    Delete { name: String },
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum VariablesOperation {
    ListActions { #[serde(default = "default_page_size")] per_page: u16 },
    GetActions { name: String },
    PutActions { name: String, value: String },
    DeleteActions { name: String },
    ListEnvironment {
        environment: String,
        #[serde(default = "default_page_size")]
        per_page: u16,
    },
    GetEnvironment { environment: String, name: String },
    PutEnvironment { environment: String, name: String, value: String },
    DeleteEnvironment { environment: String, name: String },
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum SecretsOperation {
    ListActions { #[serde(default = "default_page_size")] per_page: u16 },
    GetActionsPublicKey,
    PutActions { name: String, encrypted_value: String, key_id: String },
    DeleteActions { name: String },
    ListEnvironment {
        environment: String,
        #[serde(default = "default_page_size")]
        per_page: u16,
    },
    GetEnvironmentPublicKey { environment: String },
    PutEnvironment {
        environment: String,
        name: String,
        encrypted_value: String,
        key_id: String,
    },
    DeleteEnvironment { environment: String, name: String },
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum WebhooksOperation {
    List { #[serde(default = "default_page_size")] per_page: u16 },
    Get { hook_id: u64 },
    Create {
        name: String,
        #[serde(default = "default_true")]
        active: bool,
        events: Vec<String>,
        config: BTreeMap<String, String>,
    },
    Update {
        hook_id: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        active: Option<bool>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        events: Vec<String>,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        config: BTreeMap<String, String>,
    },
    Delete { hook_id: u64 },
    Ping { hook_id: u64 },
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProtectionSettings {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_status_checks: Vec<String>,
    #[serde(default)]
    pub strict_status_checks: bool,
    #[serde(default)]
    pub enforce_admins: bool,
    #[serde(default)]
    pub require_code_owner_reviews: bool,
    #[serde(default)]
    pub required_approving_review_count: u8,
    #[serde(default)]
    pub dismiss_stale_reviews: bool,
    #[serde(default)]
    pub require_conversation_resolution: bool,
    #[serde(default)]
    pub allow_force_pushes: bool,
    #[serde(default)]
    pub allow_deletions: bool,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RulesetRule {
    RequiredSignatures,
    NonFastForward,
    Creation,
    Update,
    Deletion,
    PullRequest {
        required_approving_review_count: u8,
        #[serde(default)]
        require_code_owner_review: bool,
        #[serde(default)]
        dismiss_stale_reviews_on_push: bool,
        #[serde(default)]
        require_last_push_approval: bool,
        #[serde(default)]
        required_review_thread_resolution: bool,
    },
    RequiredStatusChecks {
        contexts: Vec<String>,
        #[serde(default)]
        strict_required_status_checks_policy: bool,
    },
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RulesetInput {
    pub name: String,
    pub enforcement: String,
    #[serde(default = "default_branch_target")]
    pub target: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub include: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exclude: Vec<String>,
    pub rules: Vec<RulesetRule>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum BranchProtectionOperation {
    Get { branch: String },
    Put { branch: String, settings: ProtectionSettings },
    Delete { branch: String },
    ListRulesets { #[serde(default = "default_page_size")] per_page: u16 },
    GetRuleset { ruleset_id: u64 },
    CreateRuleset { ruleset: RulesetInput },
    UpdateRuleset { ruleset_id: u64, ruleset: RulesetInput },
    DeleteRuleset { ruleset_id: u64 },
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum ProjectsOperation {
    List { #[serde(default = "default_page_size")] per_page: u16 },
    Get { project_id: u64 },
    Create { name: String, #[serde(default)] body: String },
    Update {
        project_id: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        body: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        state: Option<String>,
    },
    Delete { project_id: u64 },
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum SearchOperation {
    Issues {
        query: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sort: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        order: Option<String>,
        #[serde(default = "default_page_size")]
        per_page: u16,
    },
    Commits {
        query: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sort: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        order: Option<String>,
        #[serde(default = "default_page_size")]
        per_page: u16,
    },
    Code {
        query: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sort: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        order: Option<String>,
        #[serde(default = "default_page_size")]
        per_page: u16,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct PreparedGitHubOperation {
    pub operation_kind: String,
    pub risk: GitHubOperationRisk,
    pub idempotent: bool,
    pub reconciliation: Option<GitHubReconciliation>,
    pub execution: PreparedExecution,
}

#[derive(Clone, Debug, PartialEq)]
pub enum PreparedExecution {
    Api(GitHubOperationRequest),
    RepositoryCreate(RepositoryCreateRequest),
    ArtifactDownload(ArtifactDownload),
    ReleaseAssetUpload(ReleaseAssetUpload),
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GitHubReconciliation {
    IssueMarker { marker: String },
    PullRequestMarker { marker: String },
    BranchExists { reference: String, sha: String },
    ContentMatches { path: String, branch: Option<String> },
    ContentAbsent { path: String, branch: Option<String> },
    ReleaseTag { tag: String },
    ResourceAbsent { endpoint: String },
}

#[derive(Clone, Debug, PartialEq)]
pub struct ArtifactDownload {
    pub endpoint: String,
    pub destination: PathBuf,
    pub max_bytes: u64,
    pub accept: String,
}

impl ArtifactDownload {
    pub fn validate(&self) -> MedusaResult<()> {
        validate_artifact_path(&self.destination, self.max_bytes)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ReleaseAssetUpload {
    pub release_id: u64,
    pub path: PathBuf,
    pub name: String,
    pub label: Option<String>,
    pub content_type: String,
    pub max_bytes: u64,
}

impl ReleaseAssetUpload {
    pub fn validate(&self) -> MedusaResult<()> {
        if self.release_id == 0 || self.name.trim().is_empty() || self.content_type.trim().is_empty() {
            return Err(invalid("release asset upload metadata is incomplete"));
        }
        validate_artifact_path(&self.path, self.max_bytes)
    }
}

pub(crate) fn default_hostname() -> String {
    "github.com".into()
}

fn default_api_version() -> String {
    DEFAULT_GITHUB_API_VERSION.into()
}

fn default_response_limit() -> usize {
    DEFAULT_MAX_RESPONSE_BYTES
}

fn default_artifact_limit() -> u64 {
    128 * 1_048_576
}

fn default_page_size() -> u16 {
    30
}

fn default_true() -> bool {
    true
}

fn default_branch_target() -> String {
    "branch".into()
}

fn validate_artifact_path(path: &std::path::Path, max_bytes: u64) -> MedusaResult<()> {
    if path.as_os_str().is_empty() || max_bytes == 0 || max_bytes > MAX_ARTIFACT_BYTES {
        return Err(invalid(format!(
            "artifact path is required and max_bytes must be between 1 and {MAX_ARTIFACT_BYTES}"
        )));
    }
    Ok(())
}

fn validate_idempotency_key(value: &str) -> MedusaResult<()> {
    if value.is_empty()
        || value.len() > 200
        || value.trim() != value
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
    {
        return Err(invalid(
            "idempotency_key must contain 1 to 200 safe printable characters",
        ));
    }
    Ok(())
}

fn validate_base_url(label: &str, value: Option<&str>) -> MedusaResult<()> {
    let Some(value) = value else {
        return Ok(());
    };
    let parsed = reqwest::Url::parse(value)
        .map_err(|_| invalid(format!("{label} must be an absolute HTTPS URL")))?;
    if parsed.scheme() != "https"
        || parsed.host_str().is_none()
        || parsed.username() != ""
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        return Err(invalid(format!(
            "{label} must be an absolute HTTPS URL without credentials, query, or fragment"
        )));
    }
    Ok(())
}

fn validate_api_version(value: &str) -> MedusaResult<()> {
    let bytes = value.as_bytes();
    if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' || bytes.iter().enumerate().any(|(index, byte)| !matches!(index, 4 | 7) && !byte.is_ascii_digit()) {
        return Err(invalid("api_version must use YYYY-MM-DD format"));
    }
    Ok(())
}

fn redact_preview(value: &mut Value, inherited_secret: bool) {
    match value {
        Value::Object(object) => {
            for (key, value) in object {
                let lower = key.to_ascii_lowercase();
                let secret = inherited_secret
                    || lower.contains("secret")
                    || lower.contains("token")
                    || lower.contains("password")
                    || lower.contains("encrypted_value")
                    || lower.contains("content_base64")
                    || lower == "value";
                if secret {
                    *value = Value::String("[REDACTED]".into());
                } else {
                    redact_preview(value, false);
                }
            }
        }
        Value::Array(values) => {
            for value in values {
                redact_preview(value, inherited_secret);
            }
        }
        Value::String(text) if inherited_secret => *text = "[REDACTED]".into(),
        _ => {}
    }
}

fn invalid(message: impl Into<String>) -> MedusaError {
    MedusaError::new(
        ErrorCode::InvalidInput,
        ErrorCategory::Validation,
        message,
    )
}

fn json_error(error: serde_json::Error) -> MedusaError {
    MedusaError::new(
        ErrorCode::InvalidInput,
        ErrorCategory::Validation,
        format!("serialize GitHub operation: {error}"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn document(operation: GitHubTypedOperation) -> GitHubOperationDocument {
        GitHubOperationDocument {
            schema_version: GITHUB_OPERATION_SCHEMA_VERSION,
            repository: "acme/project".into(),
            hostname: "github.com".into(),
            api_base_url: None,
            oauth_base_url: None,
            api_version: DEFAULT_GITHUB_API_VERSION.into(),
            backend: GitHubExecutionBackend::DirectOauth,
            idempotency_key: Some("issue-7".into()),
            max_response_bytes: 1024,
            operation,
        }
    }

    #[test]
    fn digest_changes_when_mutation_body_changes_but_not_backend() {
        let mut first = document(GitHubTypedOperation::Issues(IssuesOperation::Create {
            title: "first".into(),
            body: "body".into(),
            labels: vec![],
            assignees: vec![],
            milestone: None,
        }));
        let mut second = first.clone();
        second.operation = GitHubTypedOperation::Issues(IssuesOperation::Create {
            title: "second".into(),
            body: "body".into(),
            labels: vec![],
            assignees: vec![],
            milestone: None,
        });
        assert_ne!(first.request_digest().expect("first"), second.request_digest().expect("second"));
        first.backend = GitHubExecutionBackend::GhApi;
        assert_eq!(first.request_digest().expect("transport-neutral"), document(GitHubTypedOperation::Issues(IssuesOperation::Create {
            title: "first".into(), body: "body".into(), labels: vec![], assignees: vec![], milestone: None
        })).request_digest().expect("same"));
    }

    #[test]
    fn preview_redacts_secret_and_file_content() {
        let document = document(GitHubTypedOperation::Secrets(SecretsOperation::PutActions {
            name: "DEPLOY".into(),
            encrypted_value: "ciphertext".into(),
            key_id: "key".into(),
        }));
        let preview = document.redacted_preview().expect("preview").to_string();
        assert!(!preview.contains("ciphertext"));
    }

    #[test]
    fn invalid_idempotency_key_fails_closed() {
        let mut document = document(GitHubTypedOperation::Repository(RepositoryOperation::Get));
        document.idempotency_key = Some("contains space".into());
        assert!(document.validate().is_err());
    }
}
