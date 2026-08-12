use std::{
    collections::hash_map::DefaultHasher,
    hash::{Hash, Hasher},
};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use serde_json::{Map, Value};

use super::*;
use crate::{CommandOutput, execution_error, repository_creation::sanitize_external_error};

pub(super) fn receipt_from_output(
    request: &GitHubOperationRequest,
    backend: GitHubBackendKind,
    transport: &str,
    output: &CommandOutput,
) -> MedusaResult<GitHubOperationReceipt> {
    if !output.success {
        return Err(execution_error(
            &format!(
                "GitHub {} {}",
                request.method.as_str(),
                api_endpoint(request)
            ),
            sanitize_operation_error(request, &output.stderr),
        ));
    }
    let (bounded, truncated) = bounded_output(&output.stdout, request.max_response_bytes);
    let mut payload = parse_payload(&bounded);
    normalize_payload(request, &mut payload);
    redact_request_values(request, &mut payload);
    redact_json(&mut payload, request.secret_operation());
    let canonical = canonical_payload(&payload);
    let resource_identity = payload_identity(&canonical)
        .or_else(|| payload_identity(&payload))
        .or_else(|| endpoint_identity(request));
    let resource_url = payload_url(&canonical).or_else(|| payload_url(&payload));
    Ok(GitHubOperationReceipt {
        operation_id: operation_id(request),
        repository: request.repository.clone(),
        hostname: request.hostname.clone(),
        resource: request.resource,
        action: request.action.clone(),
        method: request.method,
        endpoint: request.endpoint.clone(),
        risk: request.risk(),
        backend,
        transport: transport.to_owned(),
        mutated: request.method.mutates(),
        resource_identity,
        resource_url,
        canonical,
        payload,
        truncated,
    })
}

pub(super) fn parse_payload(output: &str) -> Value {
    if output.trim().is_empty() {
        Value::Object(Map::new())
    } else if let Ok(value) = serde_json::from_str(output) {
        value
    } else if output.starts_with("https://") {
        let mut object = Map::new();
        object.insert("html_url".into(), Value::String(output.trim().to_owned()));
        if let Some(last) = output.trim_end_matches('/').rsplit('/').next() {
            if let Ok(number) = last.parse::<u64>() {
                object.insert("number".into(), Value::Number(number.into()));
            }
        }
        Value::Object(object)
    } else {
        Value::Object(Map::from_iter([(
            "output".into(),
            Value::String(sanitize_external_error(output)),
        )]))
    }
}

pub(super) fn normalize_payload(request: &GitHubOperationRequest, payload: &mut Value) {
    let Value::Object(object) = payload else {
        return;
    };
    if matches!(request.resource, GitHubResource::Repository) {
        rename_key(object, "nameWithOwner", "full_name");
        rename_key(object, "url", "html_url");
        if let Some(default_branch) = object.remove("defaultBranchRef") {
            let value = default_branch
                .get("name")
                .cloned()
                .unwrap_or(default_branch);
            object.insert("default_branch".into(), value);
        }
        if let Some(Value::String(visibility)) = object.get_mut("visibility") {
            *visibility = visibility.to_ascii_lowercase();
        }
    }
}

pub(super) fn canonical_payload(payload: &Value) -> Value {
    let Value::Object(object) = payload else {
        return Value::Object(Map::new());
    };
    let allowed = [
        "id",
        "node_id",
        "name",
        "full_name",
        "number",
        "title",
        "state",
        "html_url",
        "url",
        "download_url",
        "sha",
        "ref",
        "status",
        "conclusion",
        "workflow_id",
        "run_number",
        "visibility",
        "default_branch",
        "login",
        "output",
    ];
    Value::Object(
        object
            .iter()
            .filter(|(key, _)| allowed.contains(&key.as_str()))
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect(),
    )
}

pub(super) fn rename_key(object: &mut Map<String, Value>, from: &str, to: &str) {
    if let Some(value) = object.remove(from) {
        object.insert(to.to_owned(), value);
    }
}

pub(super) fn payload_identity(payload: &Value) -> Option<String> {
    for key in ["number", "id", "sha", "ref", "full_name", "name"] {
        if let Some(value) = payload.get(key) {
            if let Some(value) = value.as_str() {
                return Some(value.to_owned());
            }
            if value.is_number() {
                return Some(value.to_string());
            }
        }
    }
    None
}

pub(super) fn payload_url(payload: &Value) -> Option<String> {
    ["html_url", "download_url", "url"]
        .into_iter()
        .find_map(|key| payload.get(key).and_then(Value::as_str).map(str::to_owned))
}

pub(super) fn endpoint_identity(request: &GitHubOperationRequest) -> Option<String> {
    request
        .endpoint
        .split('/')
        .next_back()
        .filter(|part| !part.is_empty())
        .map(str::to_owned)
}

pub(super) fn bounded_output(output: &str, max_bytes: usize) -> (String, bool) {
    if output.len() <= max_bytes {
        return (output.to_owned(), false);
    }
    let mut boundary = max_bytes;
    while !output.is_char_boundary(boundary) {
        boundary -= 1;
    }
    (output[..boundary].to_owned(), true)
}

pub(super) fn operation_id(request: &GitHubOperationRequest) -> String {
    let mut hasher = DefaultHasher::new();
    request.repository.hash(&mut hasher);
    request.hostname.hash(&mut hasher);
    request.action.hash(&mut hasher);
    request.method.as_str().hash(&mut hasher);
    request.endpoint.hash(&mut hasher);
    request.query.hash(&mut hasher);
    format!("github-{:016x}", hasher.finish())
}

pub(super) fn api_endpoint(request: &GitHubOperationRequest) -> String {
    if matches!(request.resource, GitHubResource::Search) {
        format!("/search/{}", request.endpoint)
    } else if request.endpoint.is_empty() {
        format!("/repos/{}", request.repository)
    } else {
        format!(
            "/repos/{}/{}",
            request.repository,
            request.endpoint.trim_start_matches('/')
        )
    }
}

pub(super) fn valid_action(action: &str) -> bool {
    !action.is_empty()
        && action.len() <= 80
        && action.trim() == action
        && action.chars().all(|character| !character.is_control())
}

pub(super) fn validate_repository(repository: &str) -> MedusaResult<()> {
    let mut parts = repository.split('/');
    let owner = parts.next().unwrap_or_default();
    let name = parts.next().unwrap_or_default();
    if owner.is_empty()
        || name.is_empty()
        || matches!(owner, "." | "..")
        || matches!(name, "." | "..")
        || parts.next().is_some()
        || !valid_slug(owner)
        || !valid_slug(name)
    {
        return Err(invalid_input(
            "repository must be an owner/name pair containing GitHub-safe characters",
        ));
    }
    Ok(())
}

pub(super) fn validate_hostname(hostname: &str) -> MedusaResult<()> {
    if hostname.is_empty()
        || hostname.len() > 253
        || hostname.contains('/')
        || hostname.contains(':')
        || hostname.contains('\\')
        || hostname
            .split('.')
            .any(|part| part.is_empty() || !valid_slug(part))
    {
        return Err(invalid_input(
            "hostname must be a DNS hostname without a URL scheme",
        ));
    }
    Ok(())
}

pub(super) fn validate_endpoint(resource: GitHubResource, endpoint: &str) -> MedusaResult<()> {
    let encoded = endpoint.to_ascii_lowercase();
    if endpoint.len() > 1_024
        || endpoint.starts_with('/')
        || endpoint.ends_with('/')
        || endpoint.contains("//")
        || endpoint
            .split('/')
            .any(|segment| matches!(segment, "." | ".."))
        || endpoint.contains('\\')
        || endpoint.contains('?')
        || endpoint.contains('#')
        || endpoint.contains(':')
        || encoded.contains("%2e")
        || encoded.contains("%2f")
        || encoded.contains("%5c")
        || encoded.contains("%25")
        || endpoint
            .bytes()
            .any(|byte| byte == 0 || byte.is_ascii_control())
    {
        return Err(invalid_input(
            "endpoint must be a repository-relative GitHub API path",
        ));
    }
    let allowed = match resource {
        GitHubResource::Repository => true,
        GitHubResource::Contents => endpoint == "contents" || endpoint.starts_with("contents/"),
        GitHubResource::GitData => {
            endpoint == "branches"
                || endpoint.starts_with("branches/")
                || endpoint.starts_with("git/")
                || endpoint.starts_with("commits/")
                || endpoint == "commits"
        }
        GitHubResource::Issues => endpoint == "issues" || endpoint.starts_with("issues/"),
        GitHubResource::PullRequests => endpoint == "pulls" || endpoint.starts_with("pulls/"),
        GitHubResource::Actions => endpoint.starts_with("actions/"),
        GitHubResource::Releases => endpoint == "releases" || endpoint.starts_with("releases/"),
        GitHubResource::Collaborators => {
            endpoint == "collaborators" || endpoint.starts_with("collaborators/")
        }
        GitHubResource::Environments => {
            endpoint == "environments" || endpoint.starts_with("environments/")
        }
        GitHubResource::Variables => {
            endpoint.starts_with("actions/variables")
                || (endpoint.starts_with("environments/") && endpoint.contains("/variables"))
        }
        GitHubResource::Secrets => {
            endpoint.starts_with("actions/secrets")
                || (endpoint.starts_with("environments/") && endpoint.contains("/secrets"))
        }
        GitHubResource::Webhooks => endpoint == "hooks" || endpoint.starts_with("hooks/"),
        GitHubResource::BranchProtection => {
            endpoint.starts_with("branches/") && endpoint.contains("/protection")
        }
        GitHubResource::Projects => endpoint == "projects" || endpoint.starts_with("projects/"),
        GitHubResource::Search => matches!(endpoint, "issues" | "commits" | "code"),
    };
    if !allowed {
        return Err(invalid_input(
            "endpoint does not match the declared GitHub resource",
        ));
    }
    Ok(())
}

pub(super) fn validate_search_scope(query: &str, repository: &str) -> MedusaResult<()> {
    let expected = format!("repo:{repository}").to_ascii_lowercase();
    let mut repository_scopes = 0_usize;
    for token in
        query.split(|character: char| character.is_whitespace() || "()".contains(character))
    {
        let qualifier = token
            .trim_matches(|character| matches!(character, '"' | '\''))
            .trim_start_matches(['+', '-'])
            .to_ascii_lowercase();
        if qualifier.starts_with("repo:") {
            repository_scopes += 1;
            if qualifier != expected {
                return Err(invalid_input(
                    "repository search cannot include another repository scope",
                ));
            }
        }
        if qualifier.starts_with("org:") || qualifier.starts_with("user:") {
            return Err(invalid_input(
                "repository search cannot include organization or user scopes",
            ));
        }
    }
    if repository_scopes != 1 {
        return Err(invalid_input(
            "repository search q must include exactly one configured repo:owner/name scope",
        ));
    }
    Ok(())
}

pub(super) fn validate_body_credentials(value: &Value) -> MedusaResult<()> {
    match value {
        Value::Object(object) => {
            for (key, value) in object {
                if credential_name(key) {
                    return Err(invalid_input(
                        "raw GitHub credentials cannot be supplied in an operation body",
                    ));
                }
                validate_body_credentials(value)?;
            }
        }
        Value::Array(values) => {
            for value in values {
                validate_body_credentials(value)?;
            }
        }
        Value::String(text) if looks_sensitive(text) => {
            return Err(invalid_input(
                "raw GitHub credentials cannot be supplied in an operation body",
            ));
        }
        _ => {}
    }
    Ok(())
}

pub(super) fn valid_slug(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 100
        && !value.starts_with('-')
        && !value.ends_with('-')
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

pub(super) fn valid_query_key(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 100
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'[' | b']'))
}

pub(super) fn redact_json(value: &mut Value, redact_values: bool) {
    match value {
        Value::Object(object) => {
            for (key, value) in object {
                if sensitive_name(key)
                    || (redact_values && key.to_ascii_lowercase().contains("value"))
                {
                    *value = Value::String("[REDACTED]".into());
                } else {
                    redact_json(value, redact_values);
                }
            }
        }
        Value::Array(values) => {
            for value in values {
                redact_json(value, redact_values);
            }
        }
        Value::String(text) => *text = sanitize_external_error(text),
        _ => {}
    }
}

pub(super) fn redact_request_values(request: &GitHubOperationRequest, value: &mut Value) {
    let Some(body) = request.body.as_ref() else {
        return;
    };
    let mut sensitive_values = Vec::new();
    collect_sensitive_values(
        body,
        request.secret_operation(),
        false,
        &mut sensitive_values,
    );
    sensitive_values.sort_by_key(|candidate| std::cmp::Reverse(candidate.len()));
    replace_values(value, &sensitive_values);
}

pub(super) fn sanitize_operation_error(request: &GitHubOperationRequest, detail: &str) -> String {
    let mut sanitized = sanitize_external_error(detail);
    if let Some(body) = request.body.as_ref() {
        let mut sensitive_values = Vec::new();
        collect_sensitive_values(
            body,
            request.secret_operation(),
            false,
            &mut sensitive_values,
        );
        sensitive_values.sort_by_key(|candidate| std::cmp::Reverse(candidate.len()));
        for candidate in sensitive_values {
            if !candidate.is_empty() {
                sanitized = sanitized.replace(&candidate, "[REDACTED]");
            }
        }
    }
    sanitized
}

fn collect_sensitive_values(
    value: &Value,
    secret_operation: bool,
    inherited_sensitive: bool,
    output: &mut Vec<String>,
) {
    match value {
        Value::Object(object) => {
            for (key, value) in object {
                let key_lower = key.to_ascii_lowercase();
                let sensitive = inherited_sensitive
                    || sensitive_name(key)
                    || (secret_operation && key_lower.contains("value"));
                collect_sensitive_values(value, secret_operation, sensitive, output);
            }
        }
        Value::Array(values) => {
            for value in values {
                collect_sensitive_values(value, secret_operation, inherited_sensitive, output);
            }
        }
        Value::String(text) if inherited_sensitive && !text.is_empty() => output.push(text.clone()),
        _ => {}
    }
}

fn replace_values(value: &mut Value, sensitive_values: &[String]) {
    match value {
        Value::Object(object) => {
            for value in object.values_mut() {
                replace_values(value, sensitive_values);
            }
        }
        Value::Array(values) => {
            for value in values {
                replace_values(value, sensitive_values);
            }
        }
        Value::String(text) => {
            for candidate in sensitive_values {
                if !candidate.is_empty() {
                    *text = text.replace(candidate, "[REDACTED]");
                }
            }
        }
        _ => {}
    }
}

pub(super) fn sensitive_name(value: &str) -> bool {
    let normalized = value.to_ascii_lowercase();
    [
        "token",
        "secret",
        "password",
        "authorization",
        "private_key",
        "client_secret",
        "access_key",
    ]
    .iter()
    .any(|needle| normalized.contains(needle))
}

pub(super) fn credential_name(value: &str) -> bool {
    let normalized = value.to_ascii_lowercase();
    [
        "token",
        "authorization",
        "client_secret",
        "access_key",
        "github_pat",
    ]
    .iter()
    .any(|needle| normalized.contains(needle))
}

pub(super) fn looks_sensitive(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    lower.contains("bearer ")
        || lower.contains("ghp_")
        || lower.contains("github_pat_")
        || lower.contains("gho_")
        || lower.contains("ghu_")
        || lower.contains("ghs_")
        || lower.contains("ghr_")
}

pub(super) fn temp_io_error(error: std::io::Error) -> MedusaError {
    MedusaError::new(
        ErrorCode::PersistenceFailed,
        ErrorCategory::Environment,
        format!("unable to prepare secure GitHub API request body: {error}"),
    )
}

pub(super) fn default_hostname() -> String {
    "github.com".into()
}

pub(super) fn default_backend() -> GitHubBackendKind {
    GitHubBackendKind::NativeCli
}

pub(super) fn default_max_response_bytes() -> usize {
    DEFAULT_MAX_RESPONSE_BYTES
}

pub(super) fn json_error(error: serde_json::Error) -> MedusaError {
    MedusaError::new(
        ErrorCode::InvalidInput,
        ErrorCategory::Validation,
        error.to_string(),
    )
}
