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
    let repository = &request.repository;
    let target = format!("{}/{}", request.hostname, repository);
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
        _ => None,
    };
    arguments.map(|arguments| (arguments, "gh_native"))
}
