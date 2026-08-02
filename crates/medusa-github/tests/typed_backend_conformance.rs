use std::{
    collections::{BTreeMap, VecDeque},
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use medusa_github::{
    CommandExecutor, CommandOutput, DirectGitHubBackend, FileGitHubOperationLedger,
    GitHubApiRequest, GitHubApiResponse, GitHubApiTransport, GitHubBackendKind,
    GitHubExecutionBackend, GitHubOAuthClient, GitHubOAuthConfig, GitHubOAuthTransport,
    GitHubOperationCoordinator, GitHubOperationDocument, GitHubOperationRisk, GitHubService,
    GitHubTypedOperation, GITHUB_OPERATION_SCHEMA_VERSION, IssuesOperation,
    MemoryGitHubCredentialStore, NoDirectGitHubBackend, OAuthHttpResponse, RepositoryOperation,
    DEFAULT_GITHUB_API_VERSION,
};

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
            .map_err(|_| test_error("command lock poisoned"))?
            .push((program.into(), arguments.to_vec()));
        self.outputs
            .lock()
            .map_err(|_| test_error("output lock poisoned"))?
            .pop_front()
            .ok_or_else(|| test_error("missing fake command output"))
    }
}

#[derive(Default)]
struct FakeOAuthTransport {
    posts: Mutex<VecDeque<OAuthHttpResponse>>,
}

impl FakeOAuthTransport {
    fn configured() -> Self {
        Self {
            posts: Mutex::new(
                vec![
                    oauth_response(
                        r#"{"device_code":"device-code","user_code":"ABCD-EFGH","verification_uri":"https://github.com/login/device","expires_in":900,"interval":1}"#,
                    ),
                    oauth_response(
                        r#"{"access_token":"conformance-token","token_type":"bearer","expires_in":28800,"refresh_token":"refresh-token","refresh_token_expires_in":15897600,"scope":""}"#,
                    ),
                ]
                .into(),
            ),
        }
    }
}

impl GitHubOAuthTransport for FakeOAuthTransport {
    fn post_form(
        &self,
        _url: &str,
        _form: &BTreeMap<String, String>,
        _max_bytes: usize,
    ) -> MedusaResult<OAuthHttpResponse> {
        self.posts
            .lock()
            .map_err(|_| test_error("OAuth response lock poisoned"))?
            .pop_front()
            .ok_or_else(|| test_error("missing fake OAuth response"))
    }

    fn get_bearer(
        &self,
        _url: &str,
        token: &str,
        _api_version: &str,
        _max_bytes: usize,
    ) -> MedusaResult<OAuthHttpResponse> {
        assert_eq!(token, "conformance-token");
        Ok(oauth_response(r#"{"login":"octocat"}"#))
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
        self.requests
            .lock()
            .map_err(|_| test_error("API request lock poisoned"))?
            .push((
                request.method.to_string(),
                request.url.to_string(),
                request.body.clone(),
            ));
        self.responses
            .lock()
            .map_err(|_| test_error("API response lock poisoned"))?
            .pop_front()
            .ok_or_else(|| test_error("missing fake API response"))
    }

    fn download(
        &self,
        _request: &GitHubApiRequest,
        _token: &str,
        _destination_root: &Path,
        _destination: &Path,
        _max_bytes: u64,
    ) -> MedusaResult<(GitHubApiResponse, medusa_github::GitHubArtifactReceipt)> {
        Err(test_error("download is not used in conformance tests"))
    }

    fn upload(
        &self,
        _request: &GitHubApiRequest,
        _token: &str,
        _source_root: &Path,
        _source: &Path,
        _max_bytes: u64,
    ) -> MedusaResult<GitHubApiResponse> {
        Err(test_error("upload is not used in conformance tests"))
    }
}

fn output(stdout: &str) -> CommandOutput {
    CommandOutput {
        success: true,
        stdout: stdout.into(),
        stderr: String::new(),
    }
}

fn oauth_response(body: &str) -> OAuthHttpResponse {
    OAuthHttpResponse {
        status: 200,
        headers: BTreeMap::new(),
        body: body.into(),
    }
}

fn api_response(body: &str) -> GitHubApiResponse {
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

fn direct_oauth() -> GitHubOAuthClient<MemoryGitHubCredentialStore, FakeOAuthTransport> {
    let transport = FakeOAuthTransport::configured();
    let client = GitHubOAuthClient::new(
        GitHubOAuthConfig {
            client_id: "Iv1.conformance".into(),
            hostname: "github.com".into(),
            oauth_base_url: None,
            api_base_url: None,
            api_version: DEFAULT_GITHUB_API_VERSION.into(),
        },
        MemoryGitHubCredentialStore::default(),
        transport,
    )
    .expect("OAuth client");
    let authorization = client.start_device_flow().expect("device flow");
    client
        .complete_device_flow_with(&authorization, |duration| {
            assert!(duration <= Duration::from_secs(6));
        })
        .expect("complete device flow");
    client
}

fn document(
    backend: GitHubExecutionBackend,
    operation: GitHubTypedOperation,
) -> GitHubOperationDocument {
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

fn gh_receipt(
    document: &GitHubOperationDocument,
    outputs: Vec<CommandOutput>,
) -> medusa_github::GitHubOperationReceiptV2 {
    let root = tempfile::tempdir().expect("state root");
    let service = GitHubService::enterprise(
        "acme/project",
        "github.com",
        Some(root.path().to_path_buf()),
        FakeCommandExecutor::with(outputs),
    );
    GitHubOperationCoordinator::new(
        service,
        NoDirectGitHubBackend,
        FileGitHubOperationLedger::at(root.path()).expect("ledger"),
    )
    .execute(document)
    .expect("gh execution")
}

fn direct_receipt(
    document: &GitHubOperationDocument,
    responses: Vec<GitHubApiResponse>,
) -> medusa_github::GitHubOperationReceiptV2 {
    let root = tempfile::tempdir().expect("state root");
    let service = GitHubService::enterprise(
        "acme/project",
        "github.com",
        Some(root.path().to_path_buf()),
        FakeCommandExecutor::default(),
    );
    let direct = DirectGitHubBackend::new(
        direct_oauth(),
        FakeApiTransport::with(responses),
        root.path().to_path_buf(),
    )
    .expect("direct backend");
    GitHubOperationCoordinator::new(
        service,
        direct,
        FileGitHubOperationLedger::at(root.path()).expect("ledger"),
    )
    .execute(document)
    .expect("direct execution")
}

#[test]
fn repository_read_has_one_digest_and_canonical_result_across_all_backends() {
    let operation = GitHubTypedOperation::Repository(RepositoryOperation::Get);
    let native = document(GitHubExecutionBackend::NativeCli, operation.clone());
    let api = document(GitHubExecutionBackend::GhApi, operation.clone());
    let direct = document(GitHubExecutionBackend::DirectOauth, operation);

    assert_eq!(
        native.request_digest().expect("native digest"),
        api.request_digest().expect("API digest")
    );
    assert_eq!(
        api.request_digest().expect("API digest"),
        direct.request_digest().expect("direct digest")
    );

    let auth = output("");
    let capability = output(
        r#"{"full_name":"acme/project","permissions":{"pull":true,"push":true,"admin":true}}"#,
    );
    let native_receipt = gh_receipt(
        &native,
        vec![
            auth.clone(),
            capability.clone(),
            output(
                r#"{"nameWithOwner":"acme/project","url":"https://github.com/acme/project","visibility":"PUBLIC","defaultBranchRef":{"name":"main"}}"#,
            ),
        ],
    );
    let api_receipt = gh_receipt(
        &api,
        vec![
            auth,
            capability,
            output(
                r#"{"full_name":"acme/project","html_url":"https://github.com/acme/project","visibility":"public","default_branch":"main"}"#,
            ),
        ],
    );
    let direct_receipt = direct_receipt(
        &direct,
        vec![
            api_response(
                r#"{"full_name":"acme/project","permissions":{"pull":true,"push":true,"admin":true}}"#,
            ),
            api_response(
                r#"{"full_name":"acme/project","html_url":"https://github.com/acme/project","visibility":"public","default_branch":"main"}"#,
            ),
        ],
    );

    assert_eq!(native_receipt.canonical, api_receipt.canonical);
    assert_eq!(api_receipt.canonical, direct_receipt.canonical);
    assert_eq!(
        native_receipt.resource_identity.as_deref(),
        Some("acme/project")
    );
    assert_eq!(
        direct_receipt.http.request_id.as_deref(),
        Some("REQ-CONFORMANCE")
    );
    assert_eq!(direct_receipt.http.rate_limit.remaining, Some(4999));
}

#[test]
fn issue_create_is_backend_neutral_and_redacts_token_like_response_values() {
    let operation = GitHubTypedOperation::Issues(IssuesOperation::Create {
        title: "Conformance issue".into(),
        body: "Do not echo ghu_example_secret".into(),
        labels: vec!["test".into()],
        assignees: Vec::new(),
        milestone: None,
    });
    let api = document(GitHubExecutionBackend::GhApi, operation.clone());
    let direct = document(GitHubExecutionBackend::DirectOauth, operation);

    assert_eq!(
        direct.request_digest().expect("direct digest"),
        api.request_digest().expect("API digest")
    );
    assert_eq!(direct.risk().expect("risk"), GitHubOperationRisk::Mutation);

    let gh_receipt = gh_receipt(
        &api,
        vec![
            output(""),
            output(
                r#"{"full_name":"acme/project","permissions":{"pull":true,"push":true,"admin":true}}"#,
            ),
            output(
                r#"{"id":7,"number":7,"title":"Conformance issue","state":"open","html_url":"https://github.com/acme/project/issues/7","body":"Do not echo ghu_example_secret"}"#,
            ),
        ],
    );
    let direct_receipt = direct_receipt(
        &direct,
        vec![
            api_response(
                r#"{"full_name":"acme/project","permissions":{"pull":true,"push":true,"admin":true}}"#,
            ),
            api_response(
                r#"{"id":7,"number":7,"title":"Conformance issue","state":"open","html_url":"https://github.com/acme/project/issues/7","body":"Do not echo ghu_example_secret"}"#,
            ),
        ],
    );

    assert_eq!(gh_receipt.canonical, direct_receipt.canonical);
    let serialized = serde_json::to_string(&direct_receipt).expect("serialize");
    assert!(!serialized.contains("ghu_example_secret"));
    assert!(serialized.contains("[REDACTED]"));
    assert!(direct_receipt.redacted);
}

fn test_error(message: impl Into<String>) -> MedusaError {
    MedusaError::new(ErrorCode::ToolExecutionFailed, ErrorCategory::Execution, message)
}
