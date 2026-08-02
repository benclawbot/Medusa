use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use serde_json::Value;

use super::*;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LegacyGitHubConversion {
    pub document: GitHubOperationDocument,
    pub warning: String,
}

pub fn convert_legacy_github_operation(
    request: GitHubOperationRequest,
) -> MedusaResult<LegacyGitHubConversion> {
    request.validate()?;
    let operation = match (
        request.resource,
        request.method,
        request.endpoint.as_str(),
        request.action.as_str(),
    ) {
        (GitHubResource::Repository, GitHubHttpMethod::Get, "", _) => {
            GitHubTypedOperation::Repository(RepositoryOperation::Get)
        }
        (GitHubResource::Repository, GitHubHttpMethod::Patch, "", _) => {
            let body = object_body(&request)?;
            GitHubTypedOperation::Repository(RepositoryOperation::Update {
                description: string(body, "description")?,
                homepage: string(body, "homepage")?,
                visibility: string(body, "visibility")?,
                default_branch: string(body, "default_branch")?,
                has_issues: boolean(body, "has_issues")?,
                has_wiki: boolean(body, "has_wiki")?,
                archived: boolean(body, "archived")?,
            })
        }
        (GitHubResource::Issues, GitHubHttpMethod::Get, "issues", _) => {
            GitHubTypedOperation::Issues(IssuesOperation::List {
                state: request.query.get("state").cloned(),
                labels: request
                    .query
                    .get("labels")
                    .map(|value| value.split(',').map(str::to_owned).collect())
                    .unwrap_or_default(),
                assignee: request.query.get("assignee").cloned(),
                per_page: legacy_page_size(&request)?,
            })
        }
        (GitHubResource::Issues, GitHubHttpMethod::Get, endpoint, _)
            if issue_number(endpoint).is_some() =>
        {
            GitHubTypedOperation::Issues(IssuesOperation::Get {
                number: issue_number(endpoint).expect("guarded"),
            })
        }
        (GitHubResource::Issues, GitHubHttpMethod::Post, "issues", _) => {
            let body = object_body(&request)?;
            GitHubTypedOperation::Issues(IssuesOperation::Create {
                title: required_string(body, "title")?,
                body: string(body, "body")?.unwrap_or_default(),
                labels: string_array(body, "labels")?,
                assignees: string_array(body, "assignees")?,
                milestone: unsigned(body, "milestone")?,
            })
        }
        (GitHubResource::Issues, GitHubHttpMethod::Patch, endpoint, _)
            if issue_number(endpoint).is_some() =>
        {
            let body = object_body(&request)?;
            GitHubTypedOperation::Issues(IssuesOperation::Update {
                number: issue_number(endpoint).expect("guarded"),
                title: string(body, "title")?,
                body: string(body, "body")?,
                state: string(body, "state")?,
                state_reason: string(body, "state_reason")?,
            })
        }
        (GitHubResource::Issues, GitHubHttpMethod::Post, endpoint, _)
            if issue_comments_number(endpoint).is_some() =>
        {
            let body = object_body(&request)?;
            GitHubTypedOperation::Issues(IssuesOperation::Comment {
                number: issue_comments_number(endpoint).expect("guarded"),
                body: required_string(body, "body")?,
            })
        }
        (GitHubResource::PullRequests, GitHubHttpMethod::Get, "pulls", _) => {
            GitHubTypedOperation::PullRequests(PullRequestsOperation::List {
                state: request.query.get("state").cloned(),
                head: request.query.get("head").cloned(),
                base: request.query.get("base").cloned(),
                per_page: legacy_page_size(&request)?,
            })
        }
        (GitHubResource::PullRequests, GitHubHttpMethod::Get, endpoint, _)
            if pull_number(endpoint).is_some() =>
        {
            GitHubTypedOperation::PullRequests(PullRequestsOperation::Get {
                number: pull_number(endpoint).expect("guarded"),
            })
        }
        (GitHubResource::PullRequests, GitHubHttpMethod::Post, "pulls", _) => {
            let body = object_body(&request)?;
            GitHubTypedOperation::PullRequests(PullRequestsOperation::Create {
                title: required_string(body, "title")?,
                body: string(body, "body")?.unwrap_or_default(),
                head: required_string(body, "head")?,
                base: required_string(body, "base")?,
                draft: boolean(body, "draft")?.unwrap_or(false),
            })
        }
        (GitHubResource::PullRequests, GitHubHttpMethod::Patch, endpoint, _)
            if pull_number(endpoint).is_some() =>
        {
            let body = object_body(&request)?;
            GitHubTypedOperation::PullRequests(PullRequestsOperation::Update {
                number: pull_number(endpoint).expect("guarded"),
                title: string(body, "title")?,
                body: string(body, "body")?,
                state: string(body, "state")?,
                base: string(body, "base")?,
            })
        }
        (GitHubResource::PullRequests, GitHubHttpMethod::Put, endpoint, _)
            if pull_merge_number(endpoint).is_some() =>
        {
            let body = object_body_optional(&request)?;
            let strategy = match body
                .and_then(|body| body.get("merge_method"))
                .and_then(Value::as_str)
                .unwrap_or("merge")
            {
                "merge" => MergeStrategy::Merge,
                "squash" => MergeStrategy::Squash,
                "rebase" => MergeStrategy::Rebase,
                _ => return Err(legacy_error("unsupported legacy merge_method")),
            };
            GitHubTypedOperation::PullRequests(PullRequestsOperation::Merge {
                number: pull_merge_number(endpoint).expect("guarded"),
                strategy,
                commit_title: body
                    .and_then(|body| string(body, "commit_title").transpose())
                    .transpose()?,
                commit_message: body
                    .and_then(|body| string(body, "commit_message").transpose())
                    .transpose()?,
                expected_head_sha: body
                    .and_then(|body| string(body, "sha").transpose())
                    .transpose()?,
            })
        }
        (GitHubResource::Actions, GitHubHttpMethod::Get, "actions/runs", _) => {
            GitHubTypedOperation::Actions(ActionsOperation::ListRuns {
                workflow_id: None,
                branch: request.query.get("branch").cloned(),
                status: request.query.get("status").cloned(),
                per_page: legacy_page_size(&request)?,
            })
        }
        (GitHubResource::Actions, GitHubHttpMethod::Get, endpoint, _)
            if action_run_number(endpoint).is_some() =>
        {
            GitHubTypedOperation::Actions(ActionsOperation::GetRun {
                run_id: action_run_number(endpoint).expect("guarded"),
            })
        }
        (GitHubResource::Actions, GitHubHttpMethod::Post, endpoint, _)
            if action_suffix_number(endpoint, "rerun").is_some() =>
        {
            GitHubTypedOperation::Actions(ActionsOperation::Rerun {
                run_id: action_suffix_number(endpoint, "rerun").expect("guarded"),
            })
        }
        (GitHubResource::Actions, GitHubHttpMethod::Post, endpoint, _)
            if action_suffix_number(endpoint, "rerun-failed-jobs").is_some() =>
        {
            GitHubTypedOperation::Actions(ActionsOperation::RerunFailed {
                run_id: action_suffix_number(endpoint, "rerun-failed-jobs").expect("guarded"),
            })
        }
        (GitHubResource::Actions, GitHubHttpMethod::Post, endpoint, _)
            if action_suffix_number(endpoint, "cancel").is_some() =>
        {
            GitHubTypedOperation::Actions(ActionsOperation::Cancel {
                run_id: action_suffix_number(endpoint, "cancel").expect("guarded"),
            })
        }
        (GitHubResource::Releases, GitHubHttpMethod::Get, "releases", _) => {
            GitHubTypedOperation::Releases(ReleasesOperation::List {
                per_page: legacy_page_size(&request)?,
            })
        }
        (GitHubResource::Releases, GitHubHttpMethod::Get, endpoint, _)
            if release_number(endpoint).is_some() =>
        {
            GitHubTypedOperation::Releases(ReleasesOperation::Get {
                release_id: release_number(endpoint).expect("guarded"),
            })
        }
        (GitHubResource::Releases, GitHubHttpMethod::Post, "releases", _) => {
            let body = object_body(&request)?;
            GitHubTypedOperation::Releases(ReleasesOperation::Create {
                tag: required_string(body, "tag_name")?,
                target_commitish: string(body, "target_commitish")?,
                name: string(body, "name")?.unwrap_or_default(),
                body: string(body, "body")?.unwrap_or_default(),
                draft: boolean(body, "draft")?.unwrap_or(false),
                prerelease: boolean(body, "prerelease")?.unwrap_or(false),
                generate_release_notes: boolean(body, "generate_release_notes")?.unwrap_or(false),
            })
        }
        _ => {
            return Err(legacy_error(format!(
                "legacy GitHub operation `{}` {} `{}` has no exact typed migration; rewrite it using schemaVersion {}",
                request.action,
                method_name(request.method),
                request.endpoint,
                GITHUB_OPERATION_SCHEMA_VERSION
            )));
        }
    };

    let backend = match request.backend {
        GitHubBackendKind::NativeCli => GitHubExecutionBackend::NativeCli,
        GitHubBackendKind::RestApi => GitHubExecutionBackend::GhApi,
    };
    let document = GitHubOperationDocument {
        schema_version: GITHUB_OPERATION_SCHEMA_VERSION,
        repository: request.repository,
        hostname: request.hostname,
        api_base_url: None,
        oauth_base_url: None,
        api_version: DEFAULT_GITHUB_API_VERSION.into(),
        backend,
        idempotency_key: None,
        max_response_bytes: request.max_response_bytes,
        operation,
    };
    document.validate()?;
    Ok(LegacyGitHubConversion {
        document,
        warning: format!(
            "legacy GitHub operation schema is deprecated; migrate to schemaVersion {} typed operations",
            GITHUB_OPERATION_SCHEMA_VERSION
        ),
    })
}

fn object_body(request: &GitHubOperationRequest) -> MedusaResult<&serde_json::Map<String, Value>> {
    request
        .body
        .as_ref()
        .and_then(Value::as_object)
        .ok_or_else(|| legacy_error("legacy mutation requires a JSON object body"))
}

fn object_body_optional(
    request: &GitHubOperationRequest,
) -> MedusaResult<Option<&serde_json::Map<String, Value>>> {
    match request.body.as_ref() {
        None => Ok(None),
        Some(Value::Object(body)) => Ok(Some(body)),
        Some(_) => Err(legacy_error("legacy mutation body must be a JSON object")),
    }
}

fn required_string(body: &serde_json::Map<String, Value>, key: &str) -> MedusaResult<String> {
    string(body, key)?.ok_or_else(|| legacy_error(format!("legacy body requires `{key}`")))
}

fn string(body: &serde_json::Map<String, Value>, key: &str) -> MedusaResult<Option<String>> {
    body.get(key)
        .map(|value| {
            value
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| legacy_error(format!("legacy `{key}` must be a string")))
        })
        .transpose()
}

fn boolean(body: &serde_json::Map<String, Value>, key: &str) -> MedusaResult<Option<bool>> {
    body.get(key)
        .map(|value| {
            value
                .as_bool()
                .ok_or_else(|| legacy_error(format!("legacy `{key}` must be a boolean")))
        })
        .transpose()
}

fn unsigned(body: &serde_json::Map<String, Value>, key: &str) -> MedusaResult<Option<u64>> {
    body.get(key)
        .map(|value| {
            value
                .as_u64()
                .ok_or_else(|| legacy_error(format!("legacy `{key}` must be an unsigned integer")))
        })
        .transpose()
}

fn string_array(body: &serde_json::Map<String, Value>, key: &str) -> MedusaResult<Vec<String>> {
    let Some(value) = body.get(key) else {
        return Ok(Vec::new());
    };
    value
        .as_array()
        .ok_or_else(|| legacy_error(format!("legacy `{key}` must be an array")))?
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| legacy_error(format!("legacy `{key}` entries must be strings")))
        })
        .collect()
}

fn legacy_page_size(request: &GitHubOperationRequest) -> MedusaResult<u16> {
    match request.query.get("per_page") {
        None => Ok(30),
        Some(value) => value
            .parse::<u16>()
            .ok()
            .filter(|value| (1..=100).contains(value))
            .ok_or_else(|| legacy_error("legacy per_page must be between 1 and 100")),
    }
}

fn issue_number(endpoint: &str) -> Option<u64> {
    parse_exact_number(endpoint, "issues")
}

fn issue_comments_number(endpoint: &str) -> Option<u64> {
    parse_suffix_number(endpoint, "issues", "comments")
}

fn pull_number(endpoint: &str) -> Option<u64> {
    parse_exact_number(endpoint, "pulls")
}

fn pull_merge_number(endpoint: &str) -> Option<u64> {
    parse_suffix_number(endpoint, "pulls", "merge")
}

fn action_run_number(endpoint: &str) -> Option<u64> {
    parse_exact_number(endpoint, "actions/runs")
}

fn action_suffix_number(endpoint: &str, suffix: &str) -> Option<u64> {
    parse_suffix_number(endpoint, "actions/runs", suffix)
}

fn release_number(endpoint: &str) -> Option<u64> {
    parse_exact_number(endpoint, "releases")
}

fn parse_exact_number(endpoint: &str, prefix: &str) -> Option<u64> {
    let rest = endpoint.strip_prefix(&format!("{prefix}/"))?;
    (!rest.contains('/')).then(|| rest.parse().ok()).flatten()
}

fn parse_suffix_number(endpoint: &str, prefix: &str, suffix: &str) -> Option<u64> {
    let rest = endpoint.strip_prefix(&format!("{prefix}/"))?;
    let number = rest.strip_suffix(&format!("/{suffix}"))?;
    (!number.contains('/'))
        .then(|| number.parse().ok())
        .flatten()
}

fn method_name(method: GitHubHttpMethod) -> &'static str {
    match method {
        GitHubHttpMethod::Get => "GET",
        GitHubHttpMethod::Post => "POST",
        GitHubHttpMethod::Patch => "PATCH",
        GitHubHttpMethod::Put => "PUT",
        GitHubHttpMethod::Delete => "DELETE",
    }
}

fn legacy_error(message: impl Into<String>) -> MedusaError {
    MedusaError::new(
        ErrorCode::IncompatibleProtocol,
        ErrorCategory::Validation,
        message,
    )
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    fn legacy(
        resource: GitHubResource,
        method: GitHubHttpMethod,
        endpoint: &str,
        body: Option<Value>,
    ) -> GitHubOperationRequest {
        GitHubOperationRequest {
            repository: "acme/project".into(),
            hostname: "github.com".into(),
            resource,
            action: "legacy".into(),
            method,
            endpoint: endpoint.into(),
            query: BTreeMap::new(),
            body,
            backend: GitHubBackendKind::RestApi,
            paginate: false,
            max_response_bytes: 1024,
        }
    }

    #[test]
    fn known_issue_create_converts_exactly() {
        let conversion = convert_legacy_github_operation(legacy(
            GitHubResource::Issues,
            GitHubHttpMethod::Post,
            "issues",
            Some(serde_json::json!({"title":"test","body":"body"})),
        ))
        .expect("convert");
        assert!(matches!(
            conversion.document.operation,
            GitHubTypedOperation::Issues(IssuesOperation::Create { .. })
        ));
        assert!(conversion.warning.contains("deprecated"));
    }

    #[test]
    fn arbitrary_legacy_endpoint_fails_closed() {
        assert!(
            convert_legacy_github_operation(legacy(
                GitHubResource::Repository,
                GitHubHttpMethod::Post,
                "arbitrary",
                None,
            ))
            .is_err()
        );
    }
}
