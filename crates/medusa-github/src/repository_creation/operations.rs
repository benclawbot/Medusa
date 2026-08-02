use std::collections::BTreeMap;

use crate::invalid_input;
use medusa_core::MedusaResult;
use serde::{Deserialize, Serialize};
use serde_json::Value;

const DEFAULT_MAX_RESPONSE_BYTES: usize = 1_048_576;
const MAX_RESPONSE_BYTES: usize = 4_194_304;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GitHubBackendKind {
    NativeCli,
    RestApi,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum GitHubHttpMethod {
    Get,
    Post,
    Patch,
    Put,
    Delete,
}

impl GitHubHttpMethod {
    fn as_str(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Post => "POST",
            Self::Patch => "PATCH",
            Self::Put => "PUT",
            Self::Delete => "DELETE",
        }
    }

    #[must_use]
    pub fn mutates(self) -> bool {
        !matches!(self, Self::Get)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GitHubResource {
    Repository,
    Contents,
    GitData,
    Issues,
    PullRequests,
    Actions,
    Releases,
    Collaborators,
    Environments,
    Variables,
    Secrets,
    Webhooks,
    BranchProtection,
    Projects,
    Search,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GitHubOperationRisk {
    ReadOnly,
    Mutation,
    Administration,
    Secret,
    Destructive,
}

impl GitHubOperationRisk {
    #[must_use]
    pub fn requires_approval(self) -> bool {
        !matches!(self, Self::ReadOnly)
    }

    #[must_use]
    pub fn high_risk(self) -> bool {
        matches!(
            self,
            Self::Administration | Self::Secret | Self::Destructive
        )
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitHubOperationRequest {
    pub repository: String,
    #[serde(default = "default_hostname")]
    pub hostname: String,
    pub resource: GitHubResource,
    pub action: String,
    pub method: GitHubHttpMethod,
    #[serde(default)]
    pub endpoint: String,
    #[serde(default)]
    pub query: BTreeMap<String, String>,
    #[serde(default)]
    pub body: Option<Value>,
    #[serde(default = "default_backend")]
    pub backend: GitHubBackendKind,
    #[serde(default)]
    pub paginate: bool,
    #[serde(default = "default_max_response_bytes")]
    pub max_response_bytes: usize,
}

impl GitHubOperationRequest {
    pub fn validate(&self) -> MedusaResult<()> {
        validate_repository(&self.repository)?;
        validate_hostname(&self.hostname)?;
        if !valid_action(&self.action) {
            return Err(invalid_input(
                "GitHub operation action must contain 1 to 80 printable characters",
            ));
        }
        validate_endpoint(self.resource, &self.endpoint)?;
        if matches!(self.resource, GitHubResource::Search) {
            let query = self.query.get("q").ok_or_else(|| {
                invalid_input("repository search operations require a q query parameter")
            })?;
            validate_search_scope(query, &self.repository)?;
        }
        if self.paginate && !matches!(self.method, GitHubHttpMethod::Get) {
            return Err(invalid_input("pagination is only valid for GET operations"));
        }
        if self.max_response_bytes == 0 || self.max_response_bytes > MAX_RESPONSE_BYTES {
            return Err(invalid_input(format!(
                "max_response_bytes must be between 1 and {MAX_RESPONSE_BYTES}"
            )));
        }
        for (key, value) in &self.query {
            if !valid_query_key(key) || value.len() > 4_096 {
                return Err(invalid_input(
                    "GitHub operation query is invalid or too large",
                ));
            }
            if sensitive_name(key) || looks_sensitive(value) {
                return Err(invalid_input(
                    "credentials and secret values cannot be supplied in query parameters",
                ));
            }
        }
        if matches!(self.method, GitHubHttpMethod::Get) && self.body.is_some() {
            return Err(invalid_input("GET operations cannot include a body"));
        }
        if let Some(body) = &self.body {
            let encoded = serde_json::to_vec(body).map_err(json_error)?;
            if encoded.len() > 1_048_576 {
                return Err(invalid_input("GitHub operation body exceeds 1 MiB"));
            }
            validate_body_credentials(body)?;
        }
        Ok(())
    }

    #[must_use]
    pub fn risk(&self) -> GitHubOperationRisk {
        if matches!(self.method, GitHubHttpMethod::Delete) {
            return GitHubOperationRisk::Destructive;
        }
        if self.secret_operation() {
            return GitHubOperationRisk::Secret;
        }
        if self.method.mutates() && self.administrative_operation() {
            return GitHubOperationRisk::Administration;
        }
        if self.method.mutates() {
            GitHubOperationRisk::Mutation
        } else {
            GitHubOperationRisk::ReadOnly
        }
    }

    #[must_use]
    pub fn redacted_preview(&self) -> Value {
        let mut value = serde_json::to_value(self).unwrap_or(Value::Null);
        redact_json(&mut value, self.secret_operation());
        value
    }

    fn secret_operation(&self) -> bool {
        matches!(self.resource, GitHubResource::Secrets)
            || self.endpoint.split('/').any(|segment| segment == "secrets")
    }

    fn administrative_operation(&self) -> bool {
        matches!(
            self.resource,
            GitHubResource::Repository
                | GitHubResource::Collaborators
                | GitHubResource::Environments
                | GitHubResource::Webhooks
                | GitHubResource::BranchProtection
        ) || self.endpoint.contains("/protection")
            || self.endpoint.starts_with("collaborators")
            || self.endpoint.starts_with("hooks")
            || self.endpoint.starts_with("environments")
            || self.endpoint.starts_with("rulesets")
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitHubOperationReceipt {
    pub operation_id: String,
    pub repository: String,
    pub hostname: String,
    pub resource: GitHubResource,
    pub action: String,
    pub method: GitHubHttpMethod,
    pub endpoint: String,
    pub risk: GitHubOperationRisk,
    pub backend: GitHubBackendKind,
    pub transport: String,
    pub mutated: bool,
    pub resource_identity: Option<String>,
    pub resource_url: Option<String>,
    pub canonical: Value,
    pub payload: Value,
    pub truncated: bool,
}

pub trait GitHubOperationBackend {
    fn kind(&self) -> GitHubBackendKind;
    fn execute(&self, request: &GitHubOperationRequest) -> MedusaResult<GitHubOperationReceipt>;
}

mod backend;
mod support;

use support::*;

#[cfg(test)]
mod tests;
