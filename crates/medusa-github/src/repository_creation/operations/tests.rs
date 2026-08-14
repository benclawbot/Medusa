use std::{
    collections::BTreeMap,
    path::Path,
    sync::{Arc, Mutex},
};

use medusa_core::ErrorCode;

use super::*;
use crate::{CommandExecutor, CommandOutput, GitHubService};

type RecordedCalls = Arc<Mutex<Vec<(String, Vec<String>)>>>;

#[derive(Clone, Debug, Default)]
struct FakeExecutor {
    calls: RecordedCalls,
    inputs: Arc<Mutex<Vec<String>>>,
    outputs: Arc<Mutex<Vec<CommandOutput>>>,
}

impl FakeExecutor {
    fn with_outputs(outputs: Vec<CommandOutput>) -> Self {
        Self {
            calls: Arc::default(),
            inputs: Arc::default(),
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
            .push((program.into(), arguments.to_vec()));
        if let Some(index) = arguments.iter().position(|value| value == "--input") {
            if let Some(path) = arguments.get(index + 1) {
                self.inputs
                    .lock()
                    .expect("inputs")
                    .push(std::fs::read_to_string(path).expect("operation input"));
            }
        }
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

fn failed(stderr: &str) -> CommandOutput {
    CommandOutput {
        success: false,
        stdout: String::new(),
        stderr: stderr.into(),
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
        idempotency_key: Some("issue-create-7".into()),
        expected_head: None,
    }
}

fn service(executor: FakeExecutor) -> GitHubService<FakeExecutor> {
    GitHubService::enterprise("acme/project", "github.example", None, executor)
}

#[test]
fn non_idempotent_create_requires_replay_key() {
    let mut operation = request();
    operation.idempotency_key = None;
    let error = operation.validate().expect_err("missing key");
    assert_eq!(error.code, ErrorCode::InvalidInput);
    assert!(error.message.contains("idempotencyKey"));
}

#[test]
fn request_digest_is_stable_and_binds_request_content() {
    let first = request();
    let second = first.clone();
    assert_eq!(
        first.request_digest().expect("first digest"),
        second.request_digest().expect("second digest")
    );

    let mut changed = second;
    changed.body = Some(serde_json::json!({"title":"Different","body":"Details"}));
    assert_ne!(
        first.request_digest().expect("first digest"),
        changed.request_digest().expect("changed digest")
    );
}

#[test]
fn expected_head_is_forwarded_as_atomic_merge_precondition() {
    let executor = FakeExecutor::with_outputs(vec![output(
        r#"{"sha":"0123456789abcdef0123456789abcdef01234567","merged":true}"#,
    )]);
    let mut operation = request();
    operation.resource = GitHubResource::PullRequests;
    operation.action = "merge".into();
    operation.method = GitHubHttpMethod::Put;
    operation.endpoint = "pulls/7/merge".into();
    operation.body = Some(serde_json::json!({"merge_method":"squash"}));
    operation.idempotency_key = None;
    operation.expected_head = Some("abcdef0123456789abcdef0123456789abcdef01".into());

    service(executor.clone())
        .execute_operation(&operation)
        .expect("merge");
    let inputs = executor.inputs.lock().expect("inputs");
    let body: Value = serde_json::from_str(&inputs[0]).expect("merge body");
    assert_eq!(
        body.get("sha").and_then(Value::as_str),
        operation.expected_head.as_deref()
    );
    assert_eq!(
        body.get("merge_method").and_then(Value::as_str),
        Some("squash")
    );
}

#[test]
fn expected_head_is_rejected_outside_pull_request_merge() {
    let mut operation = request();
    operation.expected_head = Some("abcdef0123456789abcdef0123456789abcdef01".into());
    assert!(operation.validate().is_err());
}

#[test]
fn conflicting_merge_sha_fails_before_dispatch() {
    let executor = FakeExecutor::with_outputs(vec![output("{}")]);
    let mut operation = request();
    operation.resource = GitHubResource::PullRequests;
    operation.action = "merge".into();
    operation.method = GitHubHttpMethod::Put;
    operation.endpoint = "pulls/7/merge".into();
    operation.body = Some(serde_json::json!({"sha":"1111111111111111111111111111111111111111"}));
    operation.idempotency_key = None;
    operation.expected_head = Some("abcdef0123456789abcdef0123456789abcdef01".into());
    assert!(
        service(executor.clone())
            .execute_operation(&operation)
            .is_err()
    );
    assert!(executor.calls.lock().expect("calls").is_empty());
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
    operation.idempotency_key = None;
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
fn validation_rejects_dot_repositories_and_encoded_path_escapes() {
    let mut operation = request();
    operation.repository = "acme/..".into();
    assert!(operation.validate().is_err());
    operation.repository = "acme/project".into();
    operation.endpoint = "issues/%2e%2e/hooks".into();
    assert!(operation.validate().is_err());
    operation.endpoint = "issues/%252e%252e/hooks".into();
    assert!(operation.validate().is_err());
}

#[test]
fn search_requires_one_exact_repository_scope_and_uses_global_search_endpoint() {
    let executor = FakeExecutor::with_outputs(vec![output(r#"{"total_count":0,"items":[]}"#)]);
    let mut operation = request();
    operation.resource = GitHubResource::Search;
    operation.action = "search_issues".into();
    operation.method = GitHubHttpMethod::Get;
    operation.endpoint = "issues".into();
    operation.body = None;
    operation.idempotency_key = None;
    operation
        .query
        .insert("q".into(), "bug repo:acme/project".into());
    service(executor.clone())
        .execute_operation(&operation)
        .expect("search");
    let calls = executor.calls.lock().expect("calls");
    assert!(calls[0].1.contains(&"/search/issues".into()));
    drop(calls);

    operation.query.insert(
        "q".into(),
        "bug repo:acme/project OR repo:other/project".into(),
    );
    assert!(operation.validate().is_err());
    operation
        .query
        .insert("q".into(), "bug repo:acme/project org:other".into());
    assert!(operation.validate().is_err());
    operation.query.insert("q".into(), "bug".into());
    assert!(operation.validate().is_err());
}

#[test]
fn pagination_is_read_only_and_uses_slurped_pages() {
    let executor = FakeExecutor::with_outputs(vec![output("[]")]);
    let mut operation = request();
    operation.method = GitHubHttpMethod::Get;
    operation.action = "list".into();
    operation.body = None;
    operation.idempotency_key = None;
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
    operation.idempotency_key = None;
    assert_eq!(operation.risk(), GitHubOperationRisk::Destructive);
}

#[test]
fn raw_github_tokens_are_rejected_from_nested_request_bodies() {
    let mut operation = request();
    operation.body = Some(serde_json::json!({"config":{"credential":"ghp_not_allowed"}}));
    assert!(operation.validate().is_err());
    operation.body = Some(serde_json::json!({"access_token":"ordinary-text"}));
    assert!(operation.validate().is_err());
}

#[test]
fn webhook_shared_secret_is_allowed_but_redacted_from_preview() {
    let mut operation = request();
    operation.resource = GitHubResource::Webhooks;
    operation.endpoint = "hooks".into();
    operation.body = Some(serde_json::json!({"config":{"secret":"shared-hook-secret"}}));
    operation.validate().expect("webhook secret");
    assert!(
        !operation
            .redacted_preview()
            .to_string()
            .contains("shared-hook-secret")
    );
}

#[test]
fn secret_preview_receipt_and_failure_never_echo_request_values() {
    let executor = FakeExecutor::with_outputs(vec![output(
        r#"{"name":"DEPLOY_TOKEN","message":"accepted super-secret","authorization":"Bearer ghp_bad"}"#,
    )]);
    let mut operation = request();
    operation.resource = GitHubResource::Secrets;
    operation.action = "set".into();
    operation.method = GitHubHttpMethod::Put;
    operation.endpoint = "actions/secrets/DEPLOY_TOKEN".into();
    operation.body = Some(serde_json::json!({"encrypted_value":"super-secret","key_id":"7"}));
    operation.idempotency_key = None;
    let preview = operation.redacted_preview().to_string();
    assert!(!preview.contains("super-secret"));
    let receipt = service(executor)
        .execute_operation(&operation)
        .expect("secret");
    let encoded = serde_json::to_string(&receipt).expect("receipt");
    assert!(!encoded.contains("super-secret"));
    assert!(!encoded.contains("ghp_bad"));

    operation.body = Some(serde_json::json!({
        "encrypted_value":"arbitrary-secret-value",
        "key_id":"7"
    }));
    let error = service(FakeExecutor::with_outputs(vec![failed(
        "invalid encrypted value arbitrary-secret-value",
    )]))
    .execute_operation(&operation)
    .expect_err("failure");
    assert!(!error.message.contains("arbitrary-secret-value"));
    assert!(error.message.contains("[REDACTED]"));
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
fn empty_mutation_response_does_not_invent_a_resource_url() {
    let mut operation = request();
    operation.method = GitHubHttpMethod::Delete;
    operation.endpoint = "issues/7".into();
    operation.body = None;
    operation.idempotency_key = None;
    let receipt = service(FakeExecutor::with_outputs(vec![output("")]))
        .execute_operation(&operation)
        .expect("delete");
    assert_eq!(receipt.resource_identity.as_deref(), Some("7"));
    assert!(receipt.resource_url.is_none());
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
