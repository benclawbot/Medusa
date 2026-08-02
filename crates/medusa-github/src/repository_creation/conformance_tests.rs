use std::{
    collections::{BTreeMap, VecDeque},
    path::Path,
    sync::{Arc, Mutex},
};

use super::*;
use crate::{CommandExecutor, CommandOutput};

#[derive(Clone, Default)]
struct FakeCommandExecutor {
    outputs: Arc<Mutex<VecDeque<CommandOutput>>>,
    commands: Arc<Mutex<Vec<(String, Vec<String>)>>>,
}

impl FakeCommandExecutor {
    fn with(outputs: Vec<CommandOutput>) -> Self {
        Self {
            outputs: Arc::new(Mutex::new(outputs.into())),
            commands: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

impl CommandExecutor for FakeCommandExecutor {
    fn run(
        &self,
        program: &str,
        arguments: &[String],
        _directory: Option<&Path>,
    ) -> MedusaResult<CommandOutput> {
        self.commands
            .lock()
            .expect("commands")
            .push((program.into(), arguments.to_vec()));
        self.outputs
            .lock()
            .expect("outputs")
            .pop_front()
            .ok_or_else(|| invalid_input("missing fake command output"))
    }
}

#[derive(Default)]
struct FakeOAuthTransport;

impl GitHubOAuthTransport for FakeOAuthTransport {
    fn post_form(
        &self,
        _url: &str,
        _form: &BTreeMap<String, String>,
        _max_bytes: usize,
    ) -> MedusaResult<OAuthHttpResponse> {
        Err(invalid_input("OAuth transport is not used in conformance tests"))
    }

    fn get_bearer(
        &self,
        _url: &str,
        _token: &str,
        _api_version: &str,
        _max_bytes: usize,
    ) -> MedusaResult<OAuthHttpResponse> {
        Err(invalid_input("OAuth transport is not used in conformance tests"))
    }
}

#[derive(Default)]
struct FakeApiTransport {
    responses: Mutex<VecDeque<GitHubApiResponse>>,
    requests: Mutex<Vec<(String, String, Option<Vec<u8>>)>>,
}

impl FakeApiTransport {
    fn with(responses: Vec<GitHubApiResponse>) -> Self {
        Self {
            responses: Mutex::new(responses.into()),
            requests: Mutex::new(Vec::new()),
        }
    }
}

impl GitHubApiTransport for FakeApiTransport {
    fn execute(&self, request: &GitHubApiRequest, token: &str) -> MedusaResult<GitHubApiResponse> {
        assert_eq!(token, "conformance-token");
        self.requests.lock().expect("requests").push((
            request.method.to_string(),
            request.url.to_string(),
            request.body.clone(),
        ));
        self.responses
            .lock()
            .expect("responses")
            .pop_front()
            .ok_or_else(|| invalid_input("missing fake API response"))
    }

    fn download(
        &self,
        _request: &GitHubApiRequest,
        _token: &str,
        _destination_root: &Path,
        _destination: &Path,
        _max_bytes: u64,
    ) -> MedusaResult<(GitHubApiResponse, GitHubArtifactReceipt)> {
        Err(invalid_input("download is not used in this conformance test"))
    }

    fn upload(
        &self,
        _request: &GitHubApiRequest,
        _token: &str,
        _source_root: &Path,
        _source: &Path,
        _max_bytes: u64,
    ) -> MedusaResult<GitHubApiResponse> {
        Err(invalid_input("upload is not used in this conformance test"))
    }
}

fn output(stdout: &str) -> CommandOutput {
    CommandOutput {
        success: true,
        stdout: stdout.into(),
        stderr: String::new(),
    }
}

fn response(body: &str) -> GitHubApiResponse {
    GitHubApiResponse {
        status: 200,
        headers: BTreeMap::from([
            ("x-github-request-id".into(), "REQ-CONFORMANCE".into()),
            ("x-ratelimit-limit".into(), "5000".into()),
            ("x-ratelimit-remaining".into(), "4999".into()),
        ]),
        body: body.as_bytes().to_vec(),
        truncated: false,
    }
}

fn oauth_client(
    store: MemoryGitHubCredentialStore,
) -> GitHubOAuthClient<MemoryGitHubCredentialStore, FakeOAuthTransport> {
    let config = GitHubOAuthConfig {
        client_id: "Iv1.conformance".into(),
        hostname: "github.com".into(),
        oauth_base_url: None,
        api_base_url: None,
        api_version: DEFAULT_GITHUB_API_VERSION.into(),
    };
    store
        .save(
            &config,
            &GitHubOAuthCredential {
                access_token: "conformance-token".into(),
                token_type: "bearer".into(),
                expires_at: None,
                refresh_token: None,
                refresh_expires_at: None,
                scope: String::new(),
                login: Some("octocat".into()),
                obtained_at: 1,
            },
        )
        .expect("save credential");
    GitHubOAuthClient::new(config, store, FakeOAuthTransport).expect("OAuth client")
}

fn document(backend: GitHubExecutionBackend, operation: GitHubTypedOperation) -> GitHubOperationDocument {
    GitHubOperationDocument {
        schema_version: GITHUB_OPERATION_SCHEMA_VERSION,
        repository: "acme/project".into(),
        hostname: "github.com".into(),
        api_base_url: None,
        oauth_base_url: None,
        api_version: DEFAULT_GITHUB_API_VERSION.into(),
        backend,
        idempotency_key: Some("conformance-key".into()),
        max_response_bytes: 4096,
        operation,
    }
}

#[test]
fn repository_read_has_one_digest_and_canonical_receipt_across_all_backends() {
    let operation = GitHubTypedOperation::Repository(RepositoryOperation::Get);
    let native_document = document(GitHubExecutionBackend::NativeCli, operation.clone());
    let api_document = document(GitHubExecutionBackend::GhApi, operation.clone());
    let direct_document = document(GitHubExecutionBackend::DirectOauth, operation);
    assert_eq!(
        native_document.request_digest().expect("native digest"),
        api_document.request_digest().expect("API digest")
    );
    assert_eq!(
        api_document.request_digest().expect("API digest"),
        direct_document.request_digest().expect("direct digest")
    );

    let directory = tempfile::tempdir().expect("directory");
    let native_service = GitHubService::enterprise(
        "acme/project",
        "github.com",
        Some(directory.path().to_path_buf()),
        FakeCommandExecutor::with(vec![output(
            r#"{"nameWithOwner":"acme/project","url":"https://github.com/acme/project","visibility":"PUBLIC","defaultBranchRef":{"name":"main"}}"#,
        )]),
    );
    let api_service = GitHubService::enterprise(
        "acme/project",
        "github.com",
        Some(directory.path().to_path_buf()),
        FakeCommandExecutor::with(vec![output(
            r#"{"full_name":"acme/project","html_url":"https://github.com/acme/project","visibility":"public","default_branch":"main"}"#,
        )]),
    );
    let mut native_request = native_document
        .prepare()
        .expect("prepare native")
        .api_request()
        .expect("native API request")
        .clone();
    native_request.backend = GitHubBackendKind::NativeCli;
    let mut api_request = api_document
        .prepare()
        .expect("prepare API")
        .api_request()
        .expect("API request")
        .clone();
    api_request.backend = GitHubBackendKind::RestApi;
    let native = native_service
        .execute_operation(&native_request)
        .expect("native execution");
    let api = api_service
        .execute_operation(&api_request)
        .expect("API execution");

    let direct = DirectGitHubBackend::new(
        oauth_client(MemoryGitHubCredentialStore::default()),
        FakeApiTransport::with(vec![
            response(
                r#"{"full_name":"acme/project","html_url":"https://github.com/acme/project","permissions":{"pull":true,"push":true,"admin":true}}"#,
            ),
            response(
                r#"{"full_name":"acme/project","html_url":"https://github.com/acme/project","visibility":"public","default_branch":"main"}"#,
            ),
        ]),
        directory.path().to_path_buf(),
    )
    .expect("direct backend")
    .execute(
        &direct_document,
        &direct_document.prepare().expect("prepare direct"),
    )
    .expect("direct execution");

    assert_eq!(native.canonical, api.canonical);
    assert_eq!(api.canonical, direct.canonical);
    assert_eq!(native.resource_identity.as_deref(), Some("acme/project"));
    assert_eq!(direct.http.request_id.as_deref(), Some("REQ-CONFORMANCE"));
    assert_eq!(direct.http.rate_limit.remaining, Some(4999));
}

#[test]
fn issue_create_is_backend_neutral_and_keeps_secret_like_body_values_out_of_receipts() {
    let operation = GitHubTypedOperation::Issues(IssuesOperation::Create {
        title: "Conformance issue".into(),
        body: "Do not echo ghu_example_secret".into(),
        labels: vec!["test".into()],
        assignees: Vec::new(),
        milestone: None,
    });
    let direct_document = document(GitHubExecutionBackend::DirectOauth, operation.clone());
    let api_document = document(GitHubExecutionBackend::GhApi, operation);
    assert_eq!(
        direct_document.request_digest().expect("direct digest"),
        api_document.request_digest().expect("API digest")
    );
    assert_eq!(
        direct_document.risk().expect("direct risk"),
        GitHubOperationRisk::Mutation
    );

    let prepared = direct_document.prepare().expect("prepare");
    let body = prepared
        .api_request()
        .expect("API request")
        .body
        .as_ref()
        .expect("body");
    let marker = body
        .get("body")
        .and_then(Value::as_str)
        .expect("marked body");
    assert!(marker.contains("medusa-request:"));

    let directory = tempfile::tempdir().expect("directory");
    let direct = DirectGitHubBackend::new(
        oauth_client(MemoryGitHubCredentialStore::default()),
        FakeApiTransport::with(vec![
            response(
                r#"{"full_name":"acme/project","permissions":{"pull":true,"push":true,"admin":true}}"#,
            ),
            response(
                r#"{"id":7,"number":7,"title":"Conformance issue","state":"open","html_url":"https://github.com/acme/project/issues/7","body":"Do not echo ghu_example_secret"}"#,
            ),
        ]),
        directory.path().to_path_buf(),
    )
    .expect("direct backend")
    .execute(&direct_document, &prepared)
    .expect("direct execution");
    let serialized = serde_json::to_string(&direct).expect("serialize");
    assert!(!serialized.contains("ghu_example_secret"));
    assert!(serialized.contains("[REDACTED]"));
}
