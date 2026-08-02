use std::{
    collections::BTreeMap,
    path::Path,
    sync::{Arc, Mutex},
};

use medusa_core::ErrorCode;

use super::*;
use crate::{CommandExecutor, CommandOutput, GitHubService};

#[derive(Clone, Debug, Default)]
struct FakeExecutor {
    calls: Arc<Mutex<Vec<(String, Vec<String>, Vec<u8>)>>>,
    outputs: Arc<Mutex<Vec<CommandOutput>>>,
}

impl FakeExecutor {
    fn with_outputs(outputs: Vec<CommandOutput>) -> Self {
        Self {
            calls: Arc::default(),
            outputs: Arc::new(Mutex::new(outputs.into_iter().rev().collect())),
        }
    }
}

impl CommandExecutor for FakeExecutor {
    fn run(
        &self,
        program: &str,
        arguments: &[String],
        _: Option<&Path>,
    ) -> MedusaResult<CommandOutput> {
        self.calls
            .lock()
            .expect("calls")
            .push((program.into(), arguments.to_vec(), Vec::new()));
        self.outputs
            .lock()
            .expect("outputs")
            .pop()
            .ok_or_else(|| invalid_input("test output missing"))
    }
}

fn output(stdout: &str) -> CommandOutput {
    CommandOutput {
        success: true,
        stdout: stdout.into(),
        stderr: String::new(),
    }
}

fn request() -> GitHubOperationRequest {
    GitHubOperationRequest {
        repository: "acme/project".into(),
        hostname: "github.example".into(),
        resource: GitHubResource::Issues,
        action: "create".into(),
        method: GitHubHttpMethod::Post,
        endpoint: "issues".into(),
        query: BTreeMap::new(),
        body: Some(serde_json::json!({"title":"Bug","body":"Details"})),
        backend: GitHubBackendKind::RestApi,
        paginate: false,
        max_response_bytes: DEFAULT_MAX_RESPONSE_BYTES,
    }
}

fn service(executor: FakeExecutor) -> GitHubService<FakeExecutor> {
    GitHubService::enterprise("acme/project", "github.example", None, executor)
}

#[test]
fn rest_backend_sends_json_without_shell_or_token_arguments() {
    let executor = FakeExecutor::with_outputs(vec![output(
        r#"{"number":7,"html_url":"https://github.example/acme/project/issues/7","token":"hidden"}"#,
    )]);
    let receipt = service(executor.clone())
        .execute_operation(&request())
        .expect("execute");
    assert_eq!(receipt.resource_identity.as_deref(), Some("7"));
    assert!(
        !serde_json::to_string(&receipt)
            .expect("receipt")
            .contains("hidden")
    );
    let calls = executor.calls.lock().expect("calls");
    assert_eq!(calls[0].0, "gh");
    assert!(calls[0].1.contains(&"--input".into()));
    assert!(!calls[0].1.iter().any(|value| value.contains("hidden")));
    assert!(
        !calls[0]
            .1
            .iter()
            .any(|value| value == "sh" || value == "cmd")
    );
    assert!(calls[0].2.is_empty());
}

#[test]
fn native_and_rest_backends_produce_the_same_canonical_repository_payload() {
    let native = FakeExecutor::with_outputs(vec![output(
        r#"{"nameWithOwner":"acme/project","url":"https://github.example/acme/project","visibility":"PRIVATE","defaultBranchRef":{"name":"main"}}"#,
    )]);
    let rest = FakeExecutor::with_outputs(vec![output(
        r#"{"full_name":"acme/project","html_url":"https://github.example/acme/project","visibility":"private","default_branch":"main","irrelevant":true}"#,
    )]);
    let mut operation = request();
    operation.resource = GitHubResource::Repository;
    operation.action = "get".into();
    operation.method = GitHubHttpMethod::Get;
    operation.endpoint.clear();
    operation.body = None;
    operation.backend = GitHubBackendKind::NativeCli;
    let native_receipt = service(native)
        .execute_operation(&operation)
        .expect("native");
    operation.backend = GitHubBackendKind::RestApi;
    let rest_receipt = service(rest).execute_operation(&operation).expect("rest");
    assert_eq!(native_receipt.canonical, rest_receipt.canonical);
    assert_eq!(
        native_receipt.resource_identity,
        rest_receipt.resource_identity
    );
}

#[test]
fn native_backend_falls_back_to_repository_scoped_rest_transport() {
    let executor = FakeExecutor::with_outputs(vec![output(r#"{"id":99}"#)]);
    let mut operation = request();
    operation.resource = GitHubResource::Webhooks;
    operation.action = "create".into();
    operation.endpoint = "hooks".into();
    operation.backend = GitHubBackendKind::NativeCli;
    let receipt = service(executor.clone())
        .execute_operation(&operation)
        .expect("fallback");
    assert_eq!(receipt.backend, GitHubBackendKind::NativeCli);
    assert_eq!(receipt.transport, "gh_api_fallback");
    let calls = executor.calls.lock().expect("calls");
    assert!(calls[0].1.contains(&"/repos/acme/project/hooks".into()));
}

#[test]
fn validation_rejects_cross_host_absolute_and_mismatched_resource_paths() {
    let mut operation = request();
    operation.endpoint = "https://evil.example/repos/acme/project/issues".into();
    assert!(operation.validate().is_err());
    operation.endpoint = "pulls".into();
    assert!(operation.validate().is_err());
    operation.endpoint = "issues".into();
    operation.hostname = "https://github.example".into();
    assert!(operation.validate().is_err());
}

#[test]
fn search_requires_exact_repository_scope_and_uses_global_search_endpoint() {
    let executor = FakeExecutor::with_outputs(vec![output(r#"{"total_count":0,"items":[]}"#)]);
    let mut operation = request();
    operation.resource = GitHubResource::Search;
    operation.action = "search_issues".into();
    operation.method = GitHubHttpMethod::Get;
    operation.endpoint = "issues".into();
    operation.body = None;
    operation
        .query
        .insert("q".into(), "bug repo:acme/project".into());
    service(executor.clone())
        .execute_operation(&operation)
        .expect("search");
    let calls = executor.calls.lock().expect("calls");
    assert!(calls[0].1.contains(&"/search/issues".into()));
    operation
        .query
        .insert("q".into(), "bug repo:other/project".into());
    assert!(operation.validate().is_err());
}

#[test]
fn pagination_is_read_only_and_uses_slurped_pages() {
    let executor = FakeExecutor::with_outputs(vec![output("[]")]);
    let mut operation = request();
    operation.method = GitHubHttpMethod::Get;
    operation.action = "list".into();
    operation.body = None;
    operation.paginate = true;
    service(executor.clone())
        .execute_operation(&operation)
        .expect("paginate");
    let calls = executor.calls.lock().expect("calls");
    assert!(calls[0].1.contains(&"--paginate".into()));
    assert!(calls[0].1.contains(&"--slurp".into()));
    assert_eq!(operation.risk(), GitHubOperationRisk::ReadOnly);
}

#[test]
fn administration_secrets_and_delete_are_high_risk() {
    let mut operation = request();
    operation.resource = GitHubResource::Collaborators;
    operation.endpoint = "collaborators/octocat".into();
    assert_eq!(operation.risk(), GitHubOperationRisk::Administration);
    operation.resource = GitHubResource::Secrets;
    operation.endpoint = "actions/secrets/DEPLOY_TOKEN".into();
    assert_eq!(operation.risk(), GitHubOperationRisk::Secret);
    operation.resource = GitHubResource::Repository;
    assert_eq!(operation.risk(), GitHubOperationRisk::Secret);
    operation.method = GitHubHttpMethod::Delete;
    operation.body = None;
    assert_eq!(operation.risk(), GitHubOperationRisk::Destructive);
}

#[test]
fn secret_preview_and_receipt_never_echo_secret_values() {
    let executor = FakeExecutor::with_outputs(vec![output(
        r#"{"name":"DEPLOY_TOKEN","value":"super-secret","authorization":"Bearer ghp_bad"}"#,
    )]);
    let mut operation = request();
    operation.resource = GitHubResource::Secrets;
    operation.action = "set".into();
    operation.method = GitHubHttpMethod::Put;
    operation.endpoint = "actions/secrets/DEPLOY_TOKEN".into();
    operation.body = Some(serde_json::json!({"encrypted_value":"super-secret","key_id":"7"}));
    let preview = operation.redacted_preview().to_string();
    assert!(!preview.contains("super-secret"));
    let receipt = service(executor)
        .execute_operation(&operation)
        .expect("secret");
    let encoded = serde_json::to_string(&receipt).expect("receipt");
    assert!(!encoded.contains("super-secret"));
    assert!(!encoded.contains("ghp_bad"));
}

#[test]
fn response_is_bounded_and_marked_truncated() {
    let executor = FakeExecutor::with_outputs(vec![output(&"x".repeat(128))]);
    let mut operation = request();
    operation.max_response_bytes = 32;
    let receipt = service(executor)
        .execute_operation(&operation)
        .expect("bounded");
    assert!(receipt.truncated);
    assert!(receipt.payload.to_string().len() < 128);
}

#[test]
fn service_scope_must_match_operation_scope() {
    let mut operation = request();
    operation.repository = "other/project".into();
    let error = service(FakeExecutor::default())
        .execute_operation(&operation)
        .expect_err("scope mismatch");
    assert_eq!(error.code, ErrorCode::PolicyDenied);
}
