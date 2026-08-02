use std::{
    collections::BTreeMap,
    fs,
    io::{Read, Write},
    path::{Component, Path, PathBuf},
    thread,
    time::Duration,
};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use reqwest::{
    Method, StatusCode, Url,
    blocking::{Client, RequestBuilder, Response},
    header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE, ETAG, HeaderMap, LOCATION, USER_AGENT},
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use tempfile::NamedTempFile;

use super::*;

const MAX_ERROR_BYTES: usize = 65_536;
const MAX_REDIRECTS: usize = 3;
const MAX_RETRIES: u8 = 3;

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitHubRateLimit {
    pub limit: Option<u64>,
    pub remaining: Option<u64>,
    pub used: Option<u64>,
    pub reset: Option<i64>,
    pub resource: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitHubHttpMetadata {
    pub status: Option<u16>,
    pub request_id: Option<String>,
    pub retry_after: Option<String>,
    pub rate_limit: GitHubRateLimit,
    pub api_version: Option<String>,
    pub deprecation: Option<String>,
    pub sunset: Option<String>,
    pub etag: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitHubArtifactReceipt {
    pub path: PathBuf,
    pub bytes: u64,
    pub sha256: String,
    pub media_type: Option<String>,
    pub filename: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitHubBackendExecution {
    pub transport: String,
    pub mutated: bool,
    pub resource_identity: Option<String>,
    pub resource_url: Option<String>,
    pub canonical: Value,
    pub payload: Value,
    pub truncated: bool,
    pub redacted: bool,
    pub http: GitHubHttpMetadata,
    pub retry_count: u8,
    pub retry_reason: Option<String>,
    pub artifact: Option<GitHubArtifactReceipt>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitHubCapabilityReport {
    pub backend: GitHubExecutionBackend,
    pub available: bool,
    pub authenticated: bool,
    pub login: Option<String>,
    pub repository: String,
    pub hostname: String,
    pub api_version: String,
    pub role_name: Option<String>,
    pub permissions: BTreeMap<String, bool>,
    pub supported_operations: Vec<String>,
    pub missing_permissions: Vec<String>,
    pub rate_limit: GitHubRateLimit,
    pub artifact_download: bool,
    pub release_asset_upload: bool,
    pub credential_backend: String,
}

pub struct GitHubApiRequest {
    pub method: Method,
    pub url: Url,
    pub api_version: String,
    pub accept: String,
    pub content_type: Option<String>,
    pub body: Option<Vec<u8>>,
    pub response_limit: usize,
}

pub struct GitHubApiResponse {
    pub status: u16,
    pub headers: BTreeMap<String, String>,
    pub body: Vec<u8>,
    pub truncated: bool,
}

pub trait GitHubApiTransport: Send + Sync {
    fn execute(&self, request: &GitHubApiRequest, token: &str) -> MedusaResult<GitHubApiResponse>;
    fn download(
        &self,
        request: &GitHubApiRequest,
        token: &str,
        destination_root: &Path,
        destination: &Path,
        max_bytes: u64,
    ) -> MedusaResult<(GitHubApiResponse, GitHubArtifactReceipt)>;
    fn upload(
        &self,
        request: &GitHubApiRequest,
        token: &str,
        source_root: &Path,
        source: &Path,
        max_bytes: u64,
    ) -> MedusaResult<GitHubApiResponse>;
}

#[derive(Clone, Debug)]
pub struct ReqwestGitHubApiTransport {
    client: Client,
}

impl ReqwestGitHubApiTransport {
    pub fn new() -> MedusaResult<Self> {
        let client = Client::builder()
            .connect_timeout(Duration::from_secs(20))
            .timeout(Duration::from_secs(120))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(http_error)?;
        Ok(Self { client })
    }

    fn builder(&self, request: &GitHubApiRequest, token: Option<&str>) -> RequestBuilder {
        let mut builder = self
            .client
            .request(request.method.clone(), request.url.clone())
            .header(ACCEPT, &request.accept)
            .header(USER_AGENT, "medusa-github-direct")
            .header("X-GitHub-Api-Version", &request.api_version);
        if let Some(token) = token {
            builder = builder.header(AUTHORIZATION, format!("Bearer {token}"));
        }
        if let Some(content_type) = request.content_type.as_deref() {
            builder = builder.header(CONTENT_TYPE, content_type);
        }
        if let Some(body) = request.body.as_ref() {
            builder = builder.body(body.clone());
        }
        builder
    }
}

impl GitHubApiTransport for ReqwestGitHubApiTransport {
    fn execute(&self, request: &GitHubApiRequest, token: &str) -> MedusaResult<GitHubApiResponse> {
        let response = self.builder(request, Some(token)).send().map_err(http_error)?;
        read_api_response(response, request.response_limit)
    }

    fn download(
        &self,
        request: &GitHubApiRequest,
        token: &str,
        destination_root: &Path,
        destination: &Path,
        max_bytes: u64,
    ) -> MedusaResult<(GitHubApiResponse, GitHubArtifactReceipt)> {
        let destination = confined_destination(destination_root, destination)?;
        let parent = destination
            .parent()
            .ok_or_else(|| validation_error("artifact destination has no parent"))?;
        fs::create_dir_all(parent).map_err(persistence_error)?;
        let mut response = self.builder(request, Some(token)).send().map_err(http_error)?;
        for _ in 0..MAX_REDIRECTS {
            if !response.status().is_redirection() {
                break;
            }
            let location = response
                .headers()
                .get(LOCATION)
                .and_then(|value| value.to_str().ok())
                .ok_or_else(|| protocol_error("GitHub artifact redirect had no Location header"))?;
            let target = response
                .url()
                .join(location)
                .map_err(|_| protocol_error("GitHub artifact redirect URL was invalid"))?;
            if target.scheme() != "https" {
                return Err(policy_error("GitHub artifact redirect must remain HTTPS"));
            }
            response = self
                .client
                .get(target)
                .header(ACCEPT, &request.accept)
                .header(USER_AGENT, "medusa-github-direct")
                .send()
                .map_err(http_error)?;
        }
        if response.status().is_redirection() {
            return Err(protocol_error("GitHub artifact redirect limit was exceeded"));
        }
        let status = response.status().as_u16();
        let headers = header_map(response.headers());
        if !(200..300).contains(&status) {
            let error = read_api_response(response, MAX_ERROR_BYTES)?;
            return Err(api_status_error(status, &error.body, &error.headers));
        }
        let media_type = headers.get("content-type").cloned();
        let mut temporary = NamedTempFile::new_in(parent).map_err(persistence_error)?;
        let mut digest = Sha256::new();
        let mut bytes = 0_u64;
        let mut buffer = [0_u8; 32 * 1024];
        loop {
            let read = response.read(&mut buffer).map_err(http_io_error)?;
            if read == 0 {
                break;
            }
            bytes = bytes.saturating_add(read as u64);
            if bytes > max_bytes {
                return Err(validation_error(format!(
                    "GitHub artifact exceeded the {max_bytes} byte limit"
                )));
            }
            digest.update(&buffer[..read]);
            temporary
                .write_all(&buffer[..read])
                .map_err(persistence_error)?;
        }
        temporary.as_file_mut().sync_all().map_err(persistence_error)?;
        temporary
            .persist(&destination)
            .map_err(|error| persistence_error(error.error))?;
        sync_parent(&destination);
        let filename = destination
            .file_name()
            .and_then(|value| value.to_str())
            .ok_or_else(|| validation_error("artifact filename is not valid UTF-8"))?
            .to_owned();
        Ok((
            GitHubApiResponse {
                status,
                headers,
                body: Vec::new(),
                truncated: false,
            },
            GitHubArtifactReceipt {
                path: destination,
                bytes,
                sha256: hex::encode(digest.finalize()),
                media_type,
                filename,
            },
        ))
    }

    fn upload(
        &self,
        request: &GitHubApiRequest,
        token: &str,
        source_root: &Path,
        source: &Path,
        max_bytes: u64,
    ) -> MedusaResult<GitHubApiResponse> {
        let source = confined_source(source_root, source)?;
        let metadata = fs::metadata(&source).map_err(persistence_error)?;
        if !metadata.is_file() || metadata.len() == 0 || metadata.len() > max_bytes {
            return Err(validation_error(format!(
                "release asset must be a non-empty file no larger than {max_bytes} bytes"
            )));
        }
        let body = fs::read(&source).map_err(persistence_error)?;
        let mut upload = GitHubApiRequest {
            method: request.method.clone(),
            url: request.url.clone(),
            api_version: request.api_version.clone(),
            accept: request.accept.clone(),
            content_type: request.content_type.clone(),
            body: Some(body),
            response_limit: request.response_limit,
        };
        let response = self.builder(&upload, Some(token)).send().map_err(http_error)?;
        upload.body = None;
        read_api_response(response, request.response_limit)
    }
}

pub struct DirectGitHubBackend<S, O, H> {
    oauth: GitHubOAuthClient<S, O>,
    transport: H,
    repository_root: PathBuf,
}

impl<S, O, H> DirectGitHubBackend<S, O, H>
where
    S: GitHubCredentialStore,
    O: GitHubOAuthTransport,
    H: GitHubApiTransport,
{
    pub fn new(
        oauth: GitHubOAuthClient<S, O>,
        transport: H,
        repository_root: PathBuf,
    ) -> MedusaResult<Self> {
        let repository_root = repository_root
            .canonicalize()
            .map_err(persistence_error)?;
        if !repository_root.is_dir() {
            return Err(validation_error("repository root must be a directory"));
        }
        Ok(Self {
            oauth,
            transport,
            repository_root,
        })
    }

    pub fn capabilities(
        &self,
        document: &GitHubOperationDocument,
    ) -> MedusaResult<GitHubCapabilityReport> {
        document.validate()?;
        let status = self.oauth.status()?;
        if !status.authenticated && !status.refresh_available {
            return Ok(GitHubCapabilityReport {
                backend: GitHubExecutionBackend::DirectOauth,
                available: true,
                authenticated: false,
                login: status.login,
                repository: document.repository.clone(),
                hostname: document.hostname.clone(),
                api_version: document.api_version.clone(),
                role_name: None,
                permissions: BTreeMap::new(),
                supported_operations: supported_operations(),
                missing_permissions: vec!["OAuth login required".into()],
                rate_limit: GitHubRateLimit::default(),
                artifact_download: true,
                release_asset_upload: true,
                credential_backend: status.credential_backend,
            });
        }
        let token = self.oauth.access_token()?;
        let endpoint = repository_url(document, "")?;
        let request = GitHubApiRequest {
            method: Method::GET,
            url: endpoint,
            api_version: document.api_version.clone(),
            accept: "application/vnd.github+json".into(),
            content_type: None,
            body: None,
            response_limit: document.max_response_bytes,
        };
        let response = self.transport.execute(&request, &token)?;
        if !(200..300).contains(&response.status) {
            return Err(api_status_error(
                response.status,
                &response.body,
                &response.headers,
            ));
        }
        let payload: Value = serde_json::from_slice(&response.body)
            .map_err(|error| protocol_error(format!("decode GitHub repository capability response: {error}")))?;
        let permissions = payload
            .get("permissions")
            .and_then(Value::as_object)
            .map(|permissions| {
                permissions
                    .iter()
                    .filter_map(|(key, value)| value.as_bool().map(|value| (key.clone(), value)))
                    .collect()
            })
            .unwrap_or_default();
        let role_name = payload
            .get("role_name")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let required = required_repository_permission(document.risk()?);
        let missing_permissions = if has_permission(&permissions, required) {
            Vec::new()
        } else {
            vec![format!("repository permission `{required}` is required")]
        };
        let metadata = metadata(&response);
        Ok(GitHubCapabilityReport {
            backend: GitHubExecutionBackend::DirectOauth,
            available: true,
            authenticated: true,
            login: status.login,
            repository: document.repository.clone(),
            hostname: document.hostname.clone(),
            api_version: document.api_version.clone(),
            role_name,
            permissions,
            supported_operations: supported_operations(),
            missing_permissions,
            rate_limit: metadata.rate_limit,
            artifact_download: true,
            release_asset_upload: true,
            credential_backend: status.credential_backend,
        })
    }

    pub fn execute(
        &self,
        document: &GitHubOperationDocument,
        prepared: &PreparedGitHubOperation,
    ) -> MedusaResult<GitHubBackendExecution> {
        document.validate()?;
        let capabilities = self.capabilities(document)?;
        if !capabilities.missing_permissions.is_empty() {
            return Err(policy_error(format!(
                "GitHub operation cannot be dispatched: {}",
                capabilities.missing_permissions.join(", ")
            )));
        }
        let token = self.oauth.access_token()?;
        match &prepared.execution {
            PreparedExecution::Api(request) => {
                self.execute_api(document, prepared, request, &token)
            }
            PreparedExecution::ArtifactDownload(transfer) => {
                self.execute_download(document, transfer, &token)
            }
            PreparedExecution::ReleaseAssetUpload(transfer) => {
                self.execute_upload(document, transfer, &token)
            }
            PreparedExecution::RepositoryCreate(_) => Err(validation_error(
                "direct OAuth repository creation must use the typed API create route or the specialized bootstrap service",
            )),
        }
    }

    fn execute_api(
        &self,
        document: &GitHubOperationDocument,
        prepared: &PreparedGitHubOperation,
        operation: &GitHubOperationRequest,
        token: &str,
    ) -> MedusaResult<GitHubBackendExecution> {
        let request = api_request(document, operation)?;
        let digest = document.request_digest()?;
        let mut retry_count = 0_u8;
        let mut retry_reason = None;
        let response = loop {
            match self.transport.execute(&request, token) {
                Ok(response)
                    if retry_count < MAX_RETRIES
                        && prepared.idempotent
                        && retryable_status(response.status) =>
                {
                    retry_reason = Some(format!("HTTP {}", response.status));
                    sleep_retry(&digest, retry_count, &response.headers);
                    retry_count = retry_count.saturating_add(1);
                }
                Ok(response) => break response,
                Err(error)
                    if retry_count < MAX_RETRIES && prepared.idempotent && error.retryable =>
                {
                    retry_reason = Some(error.message.clone());
                    sleep_retry(&digest, retry_count, &BTreeMap::new());
                    retry_count = retry_count.saturating_add(1);
                }
                Err(error) => return Err(error),
            }
        };
        if !(200..300).contains(&response.status) {
            return Err(api_status_error(
                response.status,
                &response.body,
                &response.headers,
            ));
        }
        execution_from_response(document, operation, response, retry_count, retry_reason)
    }

    fn execute_download(
        &self,
        document: &GitHubOperationDocument,
        transfer: &ArtifactDownload,
        token: &str,
    ) -> MedusaResult<GitHubBackendExecution> {
        transfer.validate()?;
        let request = GitHubApiRequest {
            method: Method::GET,
            url: repository_url(document, &transfer.endpoint)?,
            api_version: document.api_version.clone(),
            accept: transfer.accept.clone(),
            content_type: None,
            body: None,
            response_limit: document.max_response_bytes,
        };
        let (response, artifact) = self.transport.download(
            &request,
            token,
            &self.repository_root,
            &transfer.destination,
            transfer.max_bytes,
        )?;
        let metadata = metadata(&response);
        Ok(GitHubBackendExecution {
            transport: "direct_oauth_https".into(),
            mutated: false,
            resource_identity: Some(artifact.filename.clone()),
            resource_url: None,
            canonical: serde_json::to_value(&artifact).unwrap_or(Value::Null),
            payload: Value::Object(Map::new()),
            truncated: false,
            redacted: false,
            http: metadata,
            retry_count: 0,
            retry_reason: None,
            artifact: Some(artifact),
        })
    }

    fn execute_upload(
        &self,
        document: &GitHubOperationDocument,
        transfer: &ReleaseAssetUpload,
        token: &str,
    ) -> MedusaResult<GitHubBackendExecution> {
        transfer.validate()?;
        let url = release_upload_url(document, transfer)?;
        let request = GitHubApiRequest {
            method: Method::POST,
            url,
            api_version: document.api_version.clone(),
            accept: "application/vnd.github+json".into(),
            content_type: Some(transfer.content_type.clone()),
            body: None,
            response_limit: document.max_response_bytes,
        };
        let response = self.transport.upload(
            &request,
            token,
            &self.repository_root,
            &transfer.path,
            transfer.max_bytes,
        )?;
        if !(200..300).contains(&response.status) {
            return Err(api_status_error(
                response.status,
                &response.body,
                &response.headers,
            ));
        }
        let operation = GitHubOperationRequest {
            repository: document.repository.clone(),
            hostname: document.hostname.clone(),
            resource: GitHubResource::Releases,
            action: "upload_asset".into(),
            method: GitHubHttpMethod::Post,
            endpoint: format!("releases/{}/assets", transfer.release_id),
            query: BTreeMap::new(),
            body: None,
            backend: GitHubBackendKind::RestApi,
            paginate: false,
            max_response_bytes: document.max_response_bytes,
        };
        execution_from_response(document, &operation, response, 0, None)
    }
}

fn api_request(
    document: &GitHubOperationDocument,
    operation: &GitHubOperationRequest,
) -> MedusaResult<GitHubApiRequest> {
    let mut url = if matches!(operation.resource, GitHubResource::Search) {
        api_url(document, &format!("search/{}", operation.endpoint))?
    } else {
        repository_url(document, &operation.endpoint)?
    };
    {
        let mut pairs = url.query_pairs_mut();
        for (key, value) in &operation.query {
            pairs.append_pair(key, value);
        }
    }
    let body = operation
        .body
        .as_ref()
        .map(serde_json::to_vec)
        .transpose()
        .map_err(|error| protocol_error(format!("encode GitHub request body: {error}")))?;
    Ok(GitHubApiRequest {
        method: method(operation.method),
        url,
        api_version: document.api_version.clone(),
        accept: "application/vnd.github+json".into(),
        content_type: body.as_ref().map(|_| "application/json".into()),
        body,
        response_limit: operation.max_response_bytes,
    })
}

fn execution_from_response(
    document: &GitHubOperationDocument,
    operation: &GitHubOperationRequest,
    response: GitHubApiResponse,
    retry_count: u8,
    retry_reason: Option<String>,
) -> MedusaResult<GitHubBackendExecution> {
    let mut payload = parse_payload(&response.body);
    let redacted = redact_payload(document, &mut payload);
    normalize_payload(operation.resource, &mut payload);
    let canonical = canonical_payload(&payload);
    let resource_identity = payload_identity(&canonical).or_else(|| payload_identity(&payload));
    let resource_url = payload_url(&canonical).or_else(|| payload_url(&payload));
    let http = metadata(&response);
    Ok(GitHubBackendExecution {
        transport: "direct_oauth_https".into(),
        mutated: operation.method.mutates(),
        resource_identity,
        resource_url,
        canonical,
        payload,
        truncated: response.truncated,
        redacted,
        http,
        retry_count,
        retry_reason,
        artifact: None,
    })
}

fn read_api_response(mut response: Response, max_bytes: usize) -> MedusaResult<GitHubApiResponse> {
    let status = response.status().as_u16();
    let headers = header_map(response.headers());
    let mut body = Vec::with_capacity(max_bytes.min(8_192));
    let mut buffer = [0_u8; 8_192];
    loop {
        let read = response.read(&mut buffer).map_err(http_io_error)?;
        if read == 0 {
            break;
        }
        let remaining = max_bytes.saturating_add(1).saturating_sub(body.len());
        body.extend_from_slice(&buffer[..read.min(remaining)]);
    }
    let truncated = body.len() > max_bytes;
    if truncated {
        body.truncate(max_bytes);
    }
    Ok(GitHubApiResponse {
        status,
        headers,
        body,
        truncated,
    })
}

fn repository_url(document: &GitHubOperationDocument, endpoint: &str) -> MedusaResult<Url> {
    let path = if endpoint.is_empty() {
        format!("repos/{}", document.repository)
    } else {
        format!("repos/{}/{}", document.repository, endpoint.trim_start_matches('/'))
    };
    api_url(document, &path)
}

fn api_url(document: &GitHubOperationDocument, path: &str) -> MedusaResult<Url> {
    join_url(&document.api_base_url(), path)
}

fn release_upload_url(
    document: &GitHubOperationDocument,
    transfer: &ReleaseAssetUpload,
) -> MedusaResult<Url> {
    let base = if document.hostname == "github.com" {
        "https://uploads.github.com".to_owned()
    } else {
        document.api_base_url()
    };
    let mut url = join_url(
        &base,
        &format!(
            "repos/{}/releases/{}/assets",
            document.repository, transfer.release_id
        ),
    )?;
    {
        let mut pairs = url.query_pairs_mut();
        pairs.append_pair("name", &transfer.name);
        if let Some(label) = transfer.label.as_deref() {
            pairs.append_pair("label", label);
        }
    }
    Ok(url)
}

fn join_url(base: &str, path: &str) -> MedusaResult<Url> {
    let mut base = Url::parse(base).map_err(|_| validation_error("GitHub API base URL is invalid"))?;
    if !base.path().ends_with('/') {
        let normalized = format!("{}/", base.path().trim_end_matches('/'));
        base.set_path(&normalized);
    }
    base.join(path)
        .map_err(|_| validation_error("GitHub API endpoint URL is invalid"))
}

fn method(method: GitHubHttpMethod) -> Method {
    match method {
        GitHubHttpMethod::Get => Method::GET,
        GitHubHttpMethod::Post => Method::POST,
        GitHubHttpMethod::Patch => Method::PATCH,
        GitHubHttpMethod::Put => Method::PUT,
        GitHubHttpMethod::Delete => Method::DELETE,
    }
}

fn metadata(response: &GitHubApiResponse) -> GitHubHttpMetadata {
    GitHubHttpMetadata {
        status: Some(response.status),
        request_id: response.headers.get("x-github-request-id").cloned(),
        retry_after: response.headers.get("retry-after").cloned(),
        rate_limit: GitHubRateLimit {
            limit: header_u64(&response.headers, "x-ratelimit-limit"),
            remaining: header_u64(&response.headers, "x-ratelimit-remaining"),
            used: header_u64(&response.headers, "x-ratelimit-used"),
            reset: header_i64(&response.headers, "x-ratelimit-reset"),
            resource: response.headers.get("x-ratelimit-resource").cloned(),
        },
        api_version: response.headers.get("x-github-api-version-selected").cloned(),
        deprecation: response.headers.get("deprecation").cloned(),
        sunset: response.headers.get("sunset").cloned(),
        etag: response.headers.get("etag").cloned(),
    }
}

fn header_map(headers: &HeaderMap) -> BTreeMap<String, String> {
    headers
        .iter()
        .filter_map(|(name, value)| value.to_str().ok().map(|value| (name.as_str().to_ascii_lowercase(), value.to_owned())))
        .collect()
}

fn header_u64(headers: &BTreeMap<String, String>, key: &str) -> Option<u64> {
    headers.get(key).and_then(|value| value.parse().ok())
}

fn header_i64(headers: &BTreeMap<String, String>, key: &str) -> Option<i64> {
    headers.get(key).and_then(|value| value.parse().ok())
}

fn retryable_status(status: u16) -> bool {
    matches!(status, 429 | 500 | 502 | 503 | 504)
}

fn sleep_retry(digest: &str, attempt: u8, headers: &BTreeMap<String, String>) {
    let retry_after = headers
        .get("retry-after")
        .and_then(|value| value.parse::<u64>().ok())
        .map(|seconds| seconds.saturating_mul(1000));
    let exponent = 1_u64 << attempt.min(6);
    let base = 250_u64.saturating_mul(exponent).min(8_000);
    let jitter = digest
        .as_bytes()
        .get(usize::from(attempt) % digest.len().max(1))
        .copied()
        .map(u64::from)
        .unwrap_or(0)
        % 251;
    thread::sleep(Duration::from_millis(retry_after.unwrap_or(base + jitter).min(30_000)));
}

fn confined_destination(root: &Path, destination: &Path) -> MedusaResult<PathBuf> {
    if destination.is_absolute() || destination.components().any(|component| matches!(component, Component::ParentDir | Component::RootDir | Component::Prefix(_))) {
        return Err(policy_error("artifact destination must be repository-relative"));
    }
    let filename = destination
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| validation_error("artifact destination requires a UTF-8 filename"))?;
    validate_filename(filename)?;
    Ok(root.join(destination))
}

fn confined_source(root: &Path, source: &Path) -> MedusaResult<PathBuf> {
    let joined = if source.is_absolute() { source.to_path_buf() } else { root.join(source) };
    let canonical = joined.canonicalize().map_err(persistence_error)?;
    if !canonical.starts_with(root) {
        return Err(policy_error("release asset source must remain inside the repository root"));
    }
    let filename = canonical
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| validation_error("release asset filename is not valid UTF-8"))?;
    validate_filename(filename)?;
    Ok(canonical)
}

fn validate_filename(filename: &str) -> MedusaResult<()> {
    if filename.is_empty() || filename.len() > 255 || filename == "." || filename == ".." || filename.chars().any(|character| character.is_control() || matches!(character, '/' | '\\')) {
        return Err(validation_error("artifact filename is unsafe"));
    }
    Ok(())
}

fn parse_payload(body: &[u8]) -> Value {
    if body.is_empty() {
        Value::Object(Map::new())
    } else if let Ok(value) = serde_json::from_slice(body) {
        value
    } else {
        Value::Object(Map::from_iter([(
            "output".into(),
            Value::String(String::from_utf8_lossy(body).into_owned()),
        )]))
    }
}

fn normalize_payload(resource: GitHubResource, payload: &mut Value) {
    let Value::Object(object) = payload else { return; };
    if matches!(resource, GitHubResource::Repository) {
        rename_key(object, "nameWithOwner", "full_name");
        rename_key(object, "url", "html_url");
        if let Some(Value::String(visibility)) = object.get_mut("visibility") {
            *visibility = visibility.to_ascii_lowercase();
        }
    }
}

fn canonical_payload(payload: &Value) -> Value {
    let Value::Object(object) = payload else { return Value::Object(Map::new()); };
    let allowed = [
        "id", "node_id", "name", "full_name", "number", "title", "state", "merged",
        "html_url", "url", "download_url", "sha", "ref", "status", "conclusion",
        "workflow_id", "run_number", "visibility", "default_branch", "login", "output",
    ];
    Value::Object(object.iter().filter(|(key, _)| allowed.contains(&key.as_str())).map(|(key, value)| (key.clone(), value.clone())).collect())
}

fn payload_identity(payload: &Value) -> Option<String> {
    ["number", "id", "sha", "ref", "full_name", "name"]
        .into_iter()
        .find_map(|key| payload.get(key).and_then(|value| value.as_str().map(str::to_owned).or_else(|| value.is_number().then(|| value.to_string()))))
}

fn payload_url(payload: &Value) -> Option<String> {
    ["html_url", "download_url", "url"].into_iter().find_map(|key| payload.get(key).and_then(Value::as_str).map(str::to_owned))
}

fn rename_key(object: &mut Map<String, Value>, from: &str, to: &str) {
    if let Some(value) = object.remove(from) { object.insert(to.into(), value); }
}

fn redact_payload(document: &GitHubOperationDocument, payload: &mut Value) -> bool {
    let secret_operation = matches!(document.operation, GitHubTypedOperation::Secrets(_));
    let mut redacted = false;
    redact_value(payload, secret_operation, &mut redacted);
    redacted
}

fn redact_value(value: &mut Value, inherited_secret: bool, redacted: &mut bool) {
    match value {
        Value::Object(object) => {
            for (key, value) in object {
                let lower = key.to_ascii_lowercase();
                let secret = inherited_secret || lower.contains("token") || lower.contains("secret") || lower.contains("password") || lower.contains("authorization") || lower.contains("encrypted_value") || (inherited_secret && lower.contains("value"));
                if secret {
                    *value = Value::String("[REDACTED]".into());
                    *redacted = true;
                } else {
                    redact_value(value, inherited_secret, redacted);
                }
            }
        }
        Value::Array(values) => for value in values { redact_value(value, inherited_secret, redacted); },
        Value::String(text) if looks_like_token(text) => {
            *text = "[REDACTED]".into();
            *redacted = true;
        }
        _ => {}
    }
}

fn looks_like_token(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    ["ghp_", "gho_", "ghu_", "ghs_", "ghr_", "github_pat_", "bearer "].iter().any(|prefix| lower.contains(prefix))
}

fn required_repository_permission(risk: GitHubOperationRisk) -> &'static str {
    match risk {
        GitHubOperationRisk::ReadOnly => "pull",
        GitHubOperationRisk::Mutation => "push",
        GitHubOperationRisk::Administration | GitHubOperationRisk::Secret | GitHubOperationRisk::Destructive => "admin",
    }
}

fn has_permission(permissions: &BTreeMap<String, bool>, required: &str) -> bool {
    match required {
        "pull" => permissions.get("pull").copied().unwrap_or(false) || has_permission(permissions, "push"),
        "push" => permissions.get("push").copied().unwrap_or(false) || has_permission(permissions, "admin"),
        "admin" => permissions.get("admin").copied().unwrap_or(false),
        _ => false,
    }
}

fn supported_operations() -> Vec<String> {
    [
        "repository", "contents", "git_data", "issues", "pull_requests", "actions",
        "releases", "collaborators", "environments", "variables", "secrets", "webhooks",
        "branch_protection", "projects", "search",
    ].into_iter().map(str::to_owned).collect()
}

fn api_status_error(status: u16, body: &[u8], headers: &BTreeMap<String, String>) -> MedusaError {
    let message = serde_json::from_slice::<Value>(body)
        .ok()
        .and_then(|value| value.get("message").and_then(Value::as_str).map(str::to_owned))
        .unwrap_or_else(|| String::from_utf8_lossy(body).chars().take(512).collect());
    let request_id = headers.get("x-github-request-id").cloned().unwrap_or_default();
    let retryable = retryable_status(status);
    let category = if status == 401 || status == 403 { ErrorCategory::Policy } else if retryable { ErrorCategory::Transient } else { ErrorCategory::Execution };
    MedusaError::new(
        if status == 401 || status == 403 { ErrorCode::PolicyDenied } else { ErrorCode::ToolExecutionFailed },
        category,
        format!("GitHub API HTTP {status}: {message}; request_id={request_id}"),
    ).with_retryable(retryable)
}

fn header_error_text(body: &[u8]) -> String {
    String::from_utf8_lossy(body).chars().take(512).collect()
}

fn sync_parent(path: &Path) {
    #[cfg(unix)]
    if let Some(parent) = path.parent() && let Ok(directory) = fs::File::open(parent) { let _ = directory.sync_all(); }
    #[cfg(not(unix))]
    let _ = path;
}

fn http_error(error: reqwest::Error) -> MedusaError {
    MedusaError::new(ErrorCode::DependencyUnavailable, ErrorCategory::Transient, format!("GitHub HTTPS transport: {error}"))
        .with_retryable(error.is_timeout() || error.is_connect())
}

fn http_io_error(error: std::io::Error) -> MedusaError {
    MedusaError::new(ErrorCode::DependencyUnavailable, ErrorCategory::Transient, format!("read GitHub HTTPS response: {error}")).with_retryable(true)
}

fn persistence_error(error: std::io::Error) -> MedusaError {
    MedusaError::new(ErrorCode::PersistenceFailed, ErrorCategory::Persistence, error.to_string())
}

fn validation_error(message: impl Into<String>) -> MedusaError {
    MedusaError::new(ErrorCode::InvalidInput, ErrorCategory::Validation, message)
}

fn policy_error(message: impl Into<String>) -> MedusaError {
    MedusaError::new(ErrorCode::PolicyDenied, ErrorCategory::Policy, message)
}

fn protocol_error(message: impl Into<String>) -> MedusaError {
    MedusaError::new(ErrorCode::IncompatibleProtocol, ErrorCategory::Execution, message)
}

#[cfg(test)]
mod tests {
    use std::{collections::VecDeque, sync::Mutex};

    use super::*;

    #[derive(Default)]
    struct FakeApiTransport {
        responses: Mutex<VecDeque<GitHubApiResponse>>,
        requests: Mutex<Vec<(String, String)>>,
    }

    impl FakeApiTransport {
        fn with(responses: Vec<GitHubApiResponse>) -> Self {
            Self { responses: Mutex::new(responses.into()), requests: Mutex::new(Vec::new()) }
        }
    }

    impl GitHubApiTransport for FakeApiTransport {
        fn execute(&self, request: &GitHubApiRequest, token: &str) -> MedusaResult<GitHubApiResponse> {
            assert_eq!(token, "token");
            self.requests.lock().expect("requests").push((request.method.to_string(), request.url.to_string()));
            self.responses.lock().expect("responses").pop_front().ok_or_else(|| protocol_error("missing response"))
        }

        fn download(&self, _request: &GitHubApiRequest, _token: &str, _destination_root: &Path, _destination: &Path, _max_bytes: u64) -> MedusaResult<(GitHubApiResponse, GitHubArtifactReceipt)> {
            Err(protocol_error("not used"))
        }

        fn upload(&self, _request: &GitHubApiRequest, _token: &str, _source_root: &Path, _source: &Path, _max_bytes: u64) -> MedusaResult<GitHubApiResponse> {
            Err(protocol_error("not used"))
        }
    }

    struct StaticOAuthTransport;
    impl GitHubOAuthTransport for StaticOAuthTransport {
        fn post_form(&self, _url: &str, _form: &BTreeMap<String, String>, _max_bytes: usize) -> MedusaResult<OAuthHttpResponse> { Err(protocol_error("unused")) }
        fn get_bearer(&self, _url: &str, _token: &str, _api_version: &str, _max_bytes: usize) -> MedusaResult<OAuthHttpResponse> { Err(protocol_error("unused")) }
    }

    fn document() -> GitHubOperationDocument {
        GitHubOperationDocument {
            schema_version: GITHUB_OPERATION_SCHEMA_VERSION,
            repository: "acme/project".into(), hostname: "github.com".into(), api_base_url: None, oauth_base_url: None,
            api_version: DEFAULT_GITHUB_API_VERSION.into(), backend: GitHubExecutionBackend::DirectOauth,
            idempotency_key: None, max_response_bytes: 1024,
            operation: GitHubTypedOperation::Repository(RepositoryOperation::Get),
        }
    }

    fn oauth_store() -> MemoryGitHubCredentialStore {
        let store = MemoryGitHubCredentialStore::default();
        let config = GitHubOAuthConfig { client_id: "client".into(), hostname: "github.com".into(), oauth_base_url: None, api_base_url: None, api_version: DEFAULT_GITHUB_API_VERSION.into() };
        store.save(&config, &GitHubOAuthCredential { access_token: "token".into(), token_type: "bearer".into(), expires_at: None, refresh_token: None, refresh_expires_at: None, scope: String::new(), login: Some("octocat".into()), obtained_at: 1 }).expect("save");
        store
    }

    #[test]
    fn direct_backend_sends_versioned_bearer_request_and_normalizes_receipt() {
        let directory = tempfile::tempdir().expect("tempdir");
        let config = GitHubOAuthConfig { client_id: "client".into(), hostname: "github.com".into(), oauth_base_url: None, api_base_url: None, api_version: DEFAULT_GITHUB_API_VERSION.into() };
        let oauth = GitHubOAuthClient::new(config, oauth_store(), StaticOAuthTransport).expect("oauth");
        let transport = FakeApiTransport::with(vec![
            GitHubApiResponse { status: 200, headers: BTreeMap::new(), body: br#"{"full_name":"acme/project","html_url":"https://github.com/acme/project","permissions":{"pull":true,"push":true,"admin":true}}"#.to_vec(), truncated: false },
            GitHubApiResponse { status: 200, headers: BTreeMap::from([("x-github-request-id".into(), "REQ".into())]), body: br#"{"full_name":"acme/project","html_url":"https://github.com/acme/project"}"#.to_vec(), truncated: false },
        ]);
        let backend = DirectGitHubBackend::new(oauth, transport, directory.path().to_path_buf()).expect("backend");
        let document = document();
        let prepared = document.prepare().expect("prepare");
        let result = backend.execute(&document, &prepared).expect("execute");
        assert_eq!(result.transport, "direct_oauth_https");
        assert_eq!(result.http.request_id.as_deref(), Some("REQ"));
        assert_eq!(result.resource_identity.as_deref(), Some("acme/project"));
    }

    #[test]
    fn artifact_destination_cannot_escape_repository() {
        let root = tempfile::tempdir().expect("root");
        assert!(confined_destination(root.path(), Path::new("../escape.zip")).is_err());
        assert!(confined_destination(root.path(), Path::new("artifacts/run.zip")).is_ok());
    }

    #[test]
    fn tokens_are_redacted_from_payloads() {
        let mut payload = serde_json::json!({"message":"ghu_verysecret","token":"another"});
        let redacted = redact_payload(&document(), &mut payload);
        assert!(redacted);
        assert!(!payload.to_string().contains("verysecret"));
    }
}
