use std::io::Write;

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use tempfile::NamedTempFile;

use super::*;
use crate::{CommandExecutor, GitHubService, strings};

const MAX_ERROR_BYTES: usize = 65_536;

struct RestApiBackend<'a, E> {
    service: &'a GitHubService<E>,
    reported_kind: GitHubBackendKind,
    transport: &'static str,
}

impl<E: CommandExecutor> GitHubOperationBackend for RestApiBackend<'_, E> {
    fn kind(&self) -> GitHubBackendKind {
        self.reported_kind
    }

    fn execute(&self, request: &GitHubOperationRequest) -> MedusaResult<GitHubOperationReceipt> {
        let endpoint = api_endpoint(request);
        let mut arguments = strings([
            "api",
            "--hostname",
            &request.hostname,
            "--method",
            request.method.as_str(),
            &endpoint,
        ]);
        for (key, value) in &request.query {
            arguments.extend(["-f".to_owned(), format!("{key}={value}")]);
        }
        if request.paginate {
            arguments.extend(["--paginate".to_owned(), "--slurp".to_owned()]);
        }
        let input = request
            .body
            .as_ref()
            .map(|body| -> MedusaResult<NamedTempFile> {
                let encoded = serde_json::to_vec(body).map_err(json_error)?;
                let mut file = NamedTempFile::new().map_err(temp_io_error)?;
                file.write_all(&encoded).map_err(temp_io_error)?;
                file.flush().map_err(temp_io_error)?;
                Ok(file)
            })
            .transpose()?;
        if let Some(file) = input.as_ref() {
            arguments.extend(["--input".to_owned(), file.path().display().to_string()]);
        }
        let output = self.service.executor.run_bounded(
            "gh",
            &arguments,
            self.service.directory.as_deref(),
            request.max_response_bytes,
            MAX_ERROR_BYTES,
        )?;
        receipt_from_output(request, self.kind(), self.transport, &output)
    }
}

struct NativeCliBackend<'a, E> {
    service: &'a GitHubService<E>,
}

impl<E: CommandExecutor> GitHubOperationBackend for NativeCliBackend<'_, E> {
    fn kind(&self) -> GitHubBackendKind {
        GitHubBackendKind::NativeCli
    }

    fn execute(&self, request: &GitHubOperationRequest) -> MedusaResult<GitHubOperationReceipt> {
        if let Some((arguments, transport)) = native_arguments(request) {
            let output = self.service.executor.run_bounded(
                "gh",
                &arguments,
                self.service.directory.as_deref(),
                request.max_response_bytes,
                MAX_ERROR_BYTES,
            )?;
            return receipt_from_output(request, self.kind(), transport, &output);
        }
        RestApiBackend {
            service: self.service,
            reported_kind: self.kind(),
            transport: "gh_api_fallback",
        }
        .execute(request)
    }
}

impl<E: CommandExecutor> GitHubService<E> {
    pub fn execute_operation(
        &self,
        request: &GitHubOperationRequest,
    ) -> MedusaResult<GitHubOperationReceipt> {
        request.validate()?;
        if request.repository != self.repository || request.hostname != self.hostname {
            return Err(MedusaError::new(
                ErrorCode::PolicyDenied,
                ErrorCategory::Policy,
                "GitHub operation must match the service repository and hostname",
            ));
        }
        match request.backend {
            GitHubBackendKind::NativeCli => NativeCliBackend { service: self }.execute(request),
            GitHubBackendKind::RestApi => RestApiBackend {
                service: self,
                reported_kind: GitHubBackendKind::RestApi,
                transport: "gh_api",
            }
            .execute(request),
        }
    }
}

fn native_arguments(request: &GitHubOperationRequest) -> Option<(Vec<String>, &'static str)> {
    let target = format!("{}/{}", request.hostname, request.repository);
    let arguments = match (
        request.resource,
        request.action.as_str(),
        request.method,
        request.endpoint.as_str(),
    ) {
        (GitHubResource::Repository, "get", GitHubHttpMethod::Get, "") => Some(strings([
            "repo",
            "view",
            &target,
            "--json",
            "nameWithOwner,url,visibility,defaultBranchRef",
        ])),
        (GitHubResource::Issues, "list", GitHubHttpMethod::Get, "issues") => {
            let limit = page_limit(request);
            Some(strings([
                "issue", "list", "--repo", &target, "--limit", &limit, "--json",
                "id,number,title,state,url",
            ]))
        }
        (GitHubResource::Issues, "get", GitHubHttpMethod::Get, endpoint) => {
            exact_number(endpoint, "issues").map(|number| {
                strings([
                    "issue", "view", &number, "--repo", &target, "--json",
                    "id,number,title,state,url",
                ])
            })
        }
        (GitHubResource::PullRequests, "list", GitHubHttpMethod::Get, "pulls") => {
            let limit = page_limit(request);
            Some(strings([
                "pr", "list", "--repo", &target, "--limit", &limit, "--json",
                "id,number,title,state,url,headRefName,baseRefName",
            ]))
        }
        (GitHubResource::PullRequests, "get", GitHubHttpMethod::Get, endpoint) => {
            exact_number(endpoint, "pulls").map(|number| {
                strings([
                    "pr", "view", &number, "--repo", &target, "--json",
                    "id,number,title,state,url,headRefName,baseRefName,mergeStateStatus",
                ])
            })
        }
        (GitHubResource::PullRequests, "merge", GitHubHttpMethod::Put, endpoint) => {
            let number = suffix_number(endpoint, "pulls", "merge")?;
            let body = request.body.as_ref()?.as_object()?;
            if body.get("commit_title").is_some()
                || body.get("commit_message").is_some()
                || body.get("sha").is_some()
            {
                return None;
            }
            let strategy = match body.get("merge_method").and_then(Value::as_str)? {
                "merge" => "--merge",
                "squash" => "--squash",
                "rebase" => "--rebase",
                _ => return None,
            };
            Some(strings([
                "pr", "merge", &number, "--repo", &target, strategy,
            ]))
        }
        (GitHubResource::PullRequests, "close", GitHubHttpMethod::Patch, endpoint) => {
            exact_number(endpoint, "pulls")
                .map(|number| strings(["pr", "close", &number, "--repo", &target]))
        }
        (GitHubResource::Actions, "list_runs", GitHubHttpMethod::Get, "actions/runs") => {
            let limit = page_limit(request);
            Some(strings([
                "run", "list", "--repo", &target, "--limit", &limit, "--json",
                "databaseId,workflowDatabaseId,status,conclusion,url",
            ]))
        }
        (GitHubResource::Actions, "get_run", GitHubHttpMethod::Get, endpoint) => {
            exact_number(endpoint, "actions/runs").map(|number| {
                strings([
                    "run", "view", &number, "--repo", &target, "--json",
                    "databaseId,workflowDatabaseId,status,conclusion,url",
                ])
            })
        }
        (GitHubResource::Actions, "rerun", GitHubHttpMethod::Post, endpoint) => {
            suffix_number(endpoint, "actions/runs", "rerun")
                .map(|number| strings(["run", "rerun", &number, "--repo", &target]))
        }
        (GitHubResource::Actions, "rerun_failed", GitHubHttpMethod::Post, endpoint) => {
            suffix_number(endpoint, "actions/runs", "rerun-failed-jobs").map(|number| {
                strings([
                    "run", "rerun", &number, "--failed", "--repo", &target,
                ])
            })
        }
        (GitHubResource::Actions, "cancel", GitHubHttpMethod::Post, endpoint) => {
            suffix_number(endpoint, "actions/runs", "cancel")
                .map(|number| strings(["run", "cancel", &number, "--repo", &target]))
        }
        (GitHubResource::Releases, "list", GitHubHttpMethod::Get, "releases") => {
            let limit = page_limit(request);
            Some(strings([
                "release", "list", "--repo", &target, "--limit", &limit, "--json",
                "tagName,name,isDraft,isPrerelease,publishedAt",
            ]))
        }
        (GitHubResource::Releases, "get_by_tag", GitHubHttpMethod::Get, endpoint) => {
            endpoint.strip_prefix("releases/tags/").and_then(|tag| {
                (!tag.is_empty() && !tag.contains('/')).then(|| {
                    strings([
                        "release", "view", tag, "--repo", &target, "--json",
                        "tagName,name,isDraft,isPrerelease,publishedAt,url",
                    ])
                })
            })
        }
        _ => None,
    };
    arguments.map(|arguments| (arguments, "gh_native"))
}

fn page_limit(request: &GitHubOperationRequest) -> String {
    request
        .query
        .get("per_page")
        .cloned()
        .unwrap_or_else(|| "30".into())
}

fn exact_number(endpoint: &str, prefix: &str) -> Option<String> {
    let value = endpoint.strip_prefix(&format!("{prefix}/"))?;
    (!value.contains('/') && value.bytes().all(|byte| byte.is_ascii_digit()))
        .then(|| value.to_owned())
}

fn suffix_number(endpoint: &str, prefix: &str, suffix: &str) -> Option<String> {
    let value = endpoint.strip_prefix(&format!("{prefix}/"))?;
    let number = value.strip_suffix(&format!("/{suffix}"))?;
    (!number.contains('/') && number.bytes().all(|byte| byte.is_ascii_digit()))
        .then(|| number.to_owned())
}
