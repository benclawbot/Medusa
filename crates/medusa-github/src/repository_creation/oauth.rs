use std::{
    collections::BTreeMap,
    fmt,
    io::Read,
    sync::{Arc, Mutex},
    thread,
    time::Duration,
};

use keyring::{Entry, Error as KeyringError};
use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use reqwest::{
    Url,
    blocking::Client,
    header::{ACCEPT, AUTHORIZATION, HeaderMap, USER_AGENT},
};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use super::{DEFAULT_GITHUB_API_VERSION, default_hostname};

const KEYRING_SERVICE: &str = "medusa-github-oauth";
const MAX_OAUTH_RESPONSE_BYTES: usize = 256 * 1024;
const TOKEN_REFRESH_SKEW_SECONDS: i64 = 60;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GitHubOAuthConfig {
    pub client_id: String,
    #[serde(default = "default_hostname")]
    pub hostname: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oauth_base_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_base_url: Option<String>,
    #[serde(default = "default_api_version")]
    pub api_version: String,
}

impl GitHubOAuthConfig {
    pub fn validate(&self) -> MedusaResult<()> {
        if self.client_id.is_empty()
            || self.client_id.len() > 200
            || !self.client_id.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.')
            })
        {
            return Err(validation_error(
                "GitHub OAuth client_id must contain 1 to 200 public client identifier characters",
            ));
        }
        validate_hostname(&self.hostname)?;
        validate_base_url("oauth_base_url", self.oauth_base_url.as_deref())?;
        validate_base_url("api_base_url", self.api_base_url.as_deref())?;
        validate_api_version(&self.api_version)?;
        Ok(())
    }

    #[must_use]
    pub fn oauth_base_url(&self) -> String {
        self.oauth_base_url
            .clone()
            .unwrap_or_else(|| format!("https://{}", self.hostname))
    }

    #[must_use]
    pub fn api_base_url(&self) -> String {
        self.api_base_url.clone().unwrap_or_else(|| {
            if self.hostname == "github.com" {
                "https://api.github.com".into()
            } else {
                format!("https://{}/api/v3", self.hostname)
            }
        })
    }

    fn credential_key(&self) -> String {
        format!("{}:{}", self.hostname, self.client_id)
    }
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GitHubOAuthCredential {
    pub(super) access_token: String,
    pub(super) token_type: String,
    pub(super) expires_at: Option<i64>,
    pub(super) refresh_token: Option<String>,
    pub(super) refresh_expires_at: Option<i64>,
    pub(super) scope: String,
    pub(super) login: Option<String>,
    pub(super) obtained_at: i64,
}

impl fmt::Debug for GitHubOAuthCredential {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GitHubOAuthCredential")
            .field("access_token", &"[REDACTED]")
            .field("token_type", &self.token_type)
            .field("expires_at", &self.expires_at)
            .field(
                "refresh_token",
                &self.refresh_token.as_ref().map(|_| "[REDACTED]"),
            )
            .field("refresh_expires_at", &self.refresh_expires_at)
            .field("scope", &self.scope)
            .field("login", &self.login)
            .field("obtained_at", &self.obtained_at)
            .finish()
    }
}

pub trait GitHubCredentialStore: Send + Sync {
    fn load(
        &self,
        config: &GitHubOAuthConfig,
    ) -> MedusaResult<Option<GitHubOAuthCredential>>;
    fn save(
        &self,
        config: &GitHubOAuthConfig,
        credential: &GitHubOAuthCredential,
    ) -> MedusaResult<()>;
    fn delete(&self, config: &GitHubOAuthConfig) -> MedusaResult<bool>;
    fn backend_name(&self) -> &'static str;
}

#[derive(Clone, Debug)]
pub struct KeyringGitHubCredentialStore {
    service: String,
}

impl Default for KeyringGitHubCredentialStore {
    fn default() -> Self {
        Self {
            service: KEYRING_SERVICE.into(),
        }
    }
}

impl KeyringGitHubCredentialStore {
    fn entry(&self, config: &GitHubOAuthConfig) -> MedusaResult<Entry> {
        Entry::new(&self.service, &config.credential_key()).map_err(keyring_error)
    }
}

impl GitHubCredentialStore for KeyringGitHubCredentialStore {
    fn load(
        &self,
        config: &GitHubOAuthConfig,
    ) -> MedusaResult<Option<GitHubOAuthCredential>> {
        let entry = self.entry(config)?;
        match entry.get_password() {
            Ok(encoded) => serde_json::from_str(&encoded)
                .map(Some)
                .map_err(|error| credential_error(format!("decode keyring credential: {error}"))),
            Err(KeyringError::NoEntry) => Ok(None),
            Err(error) => Err(keyring_error(error)),
        }
    }

    fn save(
        &self,
        config: &GitHubOAuthConfig,
        credential: &GitHubOAuthCredential,
    ) -> MedusaResult<()> {
        let encoded = serde_json::to_string(credential)
            .map_err(|error| credential_error(format!("encode keyring credential: {error}")))?;
        self.entry(config)?
            .set_password(&encoded)
            .map_err(keyring_error)
    }

    fn delete(&self, config: &GitHubOAuthConfig) -> MedusaResult<bool> {
        match self.entry(config)?.delete_credential() {
            Ok(()) => Ok(true),
            Err(KeyringError::NoEntry) => Ok(false),
            Err(error) => Err(keyring_error(error)),
        }
    }

    fn backend_name(&self) -> &'static str {
        "operating-system credential store"
    }
}

#[derive(Clone, Debug, Default)]
pub struct MemoryGitHubCredentialStore {
    values: Arc<Mutex<BTreeMap<String, GitHubOAuthCredential>>>,
}

impl GitHubCredentialStore for MemoryGitHubCredentialStore {
    fn load(
        &self,
        config: &GitHubOAuthConfig,
    ) -> MedusaResult<Option<GitHubOAuthCredential>> {
        Ok(self
            .values
            .lock()
            .map_err(|_| credential_error("memory credential store lock was poisoned"))?
            .get(&config.credential_key())
            .cloned())
    }

    fn save(
        &self,
        config: &GitHubOAuthConfig,
        credential: &GitHubOAuthCredential,
    ) -> MedusaResult<()> {
        self.values
            .lock()
            .map_err(|_| credential_error("memory credential store lock was poisoned"))?
            .insert(config.credential_key(), credential.clone());
        Ok(())
    }

    fn delete(&self, config: &GitHubOAuthConfig) -> MedusaResult<bool> {
        Ok(self
            .values
            .lock()
            .map_err(|_| credential_error("memory credential store lock was poisoned"))?
            .remove(&config.credential_key())
            .is_some())
    }

    fn backend_name(&self) -> &'static str {
        "in-memory test credential store"
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitHubOAuthStatus {
    pub hostname: String,
    pub authenticated: bool,
    pub login: Option<String>,
    pub expires_at: Option<i64>,
    pub refresh_expires_at: Option<i64>,
    pub refresh_available: bool,
    pub needs_refresh: bool,
    pub credential_backend: String,
    pub api_version: String,
}

pub struct GitHubDeviceAuthorization {
    device_code: String,
    user_code: String,
    verification_uri: String,
    expires_at: i64,
    interval_seconds: u64,
}

impl fmt::Debug for GitHubDeviceAuthorization {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GitHubDeviceAuthorization")
            .field("device_code", &"[REDACTED]")
            .field("user_code", &self.user_code)
            .field("verification_uri", &self.verification_uri)
            .field("expires_at", &self.expires_at)
            .field("interval_seconds", &self.interval_seconds)
            .finish()
    }
}

impl GitHubDeviceAuthorization {
    #[must_use]
    pub fn user_code(&self) -> &str {
        &self.user_code
    }

    #[must_use]
    pub fn verification_uri(&self) -> &str {
        &self.verification_uri
    }

    #[must_use]
    pub fn expires_at(&self) -> i64 {
        self.expires_at
    }

    #[must_use]
    pub fn interval_seconds(&self) -> u64 {
        self.interval_seconds
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OAuthHttpResponse {
    pub status: u16,
    pub headers: BTreeMap<String, String>,
    pub body: String,
}

pub trait GitHubOAuthTransport: Send + Sync {
    fn post_form(
        &self,
        url: &str,
        form: &BTreeMap<String, String>,
        max_bytes: usize,
    ) -> MedusaResult<OAuthHttpResponse>;
    fn get_bearer(
        &self,
        url: &str,
        token: &str,
        api_version: &str,
        max_bytes: usize,
    ) -> MedusaResult<OAuthHttpResponse>;
}

#[derive(Clone, Debug)]
pub struct ReqwestOAuthTransport {
    client: Client,
}

impl ReqwestOAuthTransport {
    pub fn new() -> MedusaResult<Self> {
        let client = Client::builder()
            .connect_timeout(Duration::from_secs(20))
            .timeout(Duration::from_secs(60))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(http_error)?;
        Ok(Self { client })
    }
}

impl GitHubOAuthTransport for ReqwestOAuthTransport {
    fn post_form(
        &self,
        url: &str,
        form: &BTreeMap<String, String>,
        max_bytes: usize,
    ) -> MedusaResult<OAuthHttpResponse> {
        let response = self
            .client
            .post(url)
            .header(ACCEPT, "application/json")
            .header(USER_AGENT, "medusa-github-oauth")
            .form(form)
            .send()
            .map_err(http_error)?;
        read_response(response, max_bytes)
    }

    fn get_bearer(
        &self,
        url: &str,
        token: &str,
        api_version: &str,
        max_bytes: usize,
    ) -> MedusaResult<OAuthHttpResponse> {
        let response = self
            .client
            .get(url)
            .header(ACCEPT, "application/vnd.github+json")
            .header(USER_AGENT, "medusa-github-oauth")
            .header(AUTHORIZATION, format!("Bearer {token}"))
            .header("X-GitHub-Api-Version", api_version)
            .send()
            .map_err(http_error)?;
        read_response(response, max_bytes)
    }
}

pub struct GitHubOAuthClient<S, T> {
    config: GitHubOAuthConfig,
    store: S,
    transport: T,
}

impl<S: GitHubCredentialStore, T: GitHubOAuthTransport> GitHubOAuthClient<S, T> {
    pub fn new(config: GitHubOAuthConfig, store: S, transport: T) -> MedusaResult<Self> {
        config.validate()?;
        Ok(Self {
            config,
            store,
            transport,
        })
    }

    #[must_use]
    pub fn config(&self) -> &GitHubOAuthConfig {
        &self.config
    }

    pub fn start_device_flow(&self) -> MedusaResult<GitHubDeviceAuthorization> {
        let endpoint = oauth_endpoint(&self.config.oauth_base_url(), "login/device/code")?;
        let response = self.transport.post_form(
            endpoint.as_str(),
            &BTreeMap::from([("client_id".into(), self.config.client_id.clone())]),
            MAX_OAUTH_RESPONSE_BYTES,
        )?;
        require_success(&response, "request GitHub device code")?;
        let payload: DeviceCodeResponse = serde_json::from_str(&response.body)
            .map_err(|error| protocol_error(format!("decode GitHub device code: {error}")))?;
        if payload.device_code.is_empty()
            || payload.user_code.is_empty()
            || payload.verification_uri.is_empty()
            || payload.expires_in == 0
        {
            return Err(protocol_error("GitHub device code response was incomplete"));
        }
        Ok(GitHubDeviceAuthorization {
            device_code: payload.device_code,
            user_code: payload.user_code,
            verification_uri: payload.verification_uri,
            expires_at: now().saturating_add(i64::from(payload.expires_in)),
            interval_seconds: u64::from(payload.interval.max(1)),
        })
    }

    pub fn complete_device_flow(
        &self,
        authorization: &GitHubDeviceAuthorization,
    ) -> MedusaResult<GitHubOAuthStatus> {
        self.complete_device_flow_with(authorization, thread::sleep)
    }

    pub fn complete_device_flow_with<F>(
        &self,
        authorization: &GitHubDeviceAuthorization,
        mut sleep: F,
    ) -> MedusaResult<GitHubOAuthStatus>
    where
        F: FnMut(Duration),
    {
        let endpoint = oauth_endpoint(&self.config.oauth_base_url(), "login/oauth/access_token")?;
        let mut interval = authorization.interval_seconds.max(1);
        loop {
            if now() >= authorization.expires_at {
                return Err(authentication_error(
                    "GitHub device authorization expired before completion",
                    false,
                ));
            }
            let response = self.transport.post_form(
                endpoint.as_str(),
                &BTreeMap::from([
                    ("client_id".into(), self.config.client_id.clone()),
                    ("device_code".into(), authorization.device_code.clone()),
                    (
                        "grant_type".into(),
                        "urn:ietf:params:oauth:grant-type:device_code".into(),
                    ),
                ]),
                MAX_OAUTH_RESPONSE_BYTES,
            )?;
            let payload: TokenResponse = serde_json::from_str(&response.body)
                .map_err(|error| protocol_error(format!("decode GitHub OAuth response: {error}")))?;
            if let Some(error) = payload.error.as_deref() {
                match error {
                    "authorization_pending" => {
                        sleep(Duration::from_secs(interval));
                        continue;
                    }
                    "slow_down" => {
                        interval = interval.saturating_add(5);
                        sleep(Duration::from_secs(interval));
                        continue;
                    }
                    "expired_token" => {
                        return Err(authentication_error(
                            "GitHub device code expired; start login again",
                            false,
                        ));
                    }
                    "access_denied" => {
                        return Err(authentication_error(
                            "GitHub device authorization was denied",
                            false,
                        ));
                    }
                    "device_flow_disabled" => {
                        return Err(authentication_error(
                            "GitHub App device flow is disabled for this client ID",
                            false,
                        ));
                    }
                    other => {
                        return Err(authentication_error(
                            format!(
                                "GitHub OAuth failed with {other}: {}",
                                payload.error_description.unwrap_or_default()
                            ),
                            false,
                        ));
                    }
                }
            }
            require_success(&response, "exchange GitHub device code")?;
            let mut credential = credential_from_token_response(payload)?;
            credential.login = self.fetch_login(&credential.access_token).ok();
            self.store.save(&self.config, &credential)?;
            return self.status();
        }
    }

    pub fn status(&self) -> MedusaResult<GitHubOAuthStatus> {
        let credential = self.store.load(&self.config)?;
        Ok(match credential {
            None => GitHubOAuthStatus {
                hostname: self.config.hostname.clone(),
                authenticated: false,
                login: None,
                expires_at: None,
                refresh_expires_at: None,
                refresh_available: false,
                needs_refresh: false,
                credential_backend: self.store.backend_name().into(),
                api_version: self.config.api_version.clone(),
            },
            Some(credential) => GitHubOAuthStatus {
                hostname: self.config.hostname.clone(),
                authenticated: !expired(credential.expires_at),
                login: credential.login,
                expires_at: credential.expires_at,
                refresh_expires_at: credential.refresh_expires_at,
                refresh_available: credential.refresh_token.is_some()
                    && !expired(credential.refresh_expires_at),
                needs_refresh: needs_refresh(credential.expires_at),
                credential_backend: self.store.backend_name().into(),
                api_version: self.config.api_version.clone(),
            },
        })
    }

    pub fn refresh(&self) -> MedusaResult<GitHubOAuthStatus> {
        let current = self
            .store
            .load(&self.config)?
            .ok_or_else(|| authentication_error("GitHub OAuth login is not configured", false))?;
        let refresh_token = current.refresh_token.as_ref().ok_or_else(|| {
            authentication_error(
                "GitHub credential has no refresh token; run OAuth login again",
                false,
            )
        })?;
        if expired(current.refresh_expires_at) {
            return Err(authentication_error(
                "GitHub OAuth refresh token expired; run login again",
                false,
            ));
        }
        let endpoint = oauth_endpoint(&self.config.oauth_base_url(), "login/oauth/access_token")?;
        let response = self.transport.post_form(
            endpoint.as_str(),
            &BTreeMap::from([
                ("client_id".into(), self.config.client_id.clone()),
                ("grant_type".into(), "refresh_token".into()),
                ("refresh_token".into(), refresh_token.clone()),
            ]),
            MAX_OAUTH_RESPONSE_BYTES,
        )?;
        require_success(&response, "refresh GitHub OAuth token")?;
        let payload: TokenResponse = serde_json::from_str(&response.body)
            .map_err(|error| protocol_error(format!("decode GitHub refresh response: {error}")))?;
        if let Some(error) = payload.error.as_deref() {
            return Err(authentication_error(
                format!(
                    "GitHub OAuth refresh failed with {error}: {}",
                    payload.error_description.unwrap_or_default()
                ),
                false,
            ));
        }
        let mut credential = credential_from_token_response(payload)?;
        credential.login = current.login.or_else(|| self.fetch_login(&credential.access_token).ok());
        self.store.save(&self.config, &credential)?;
        self.status()
    }

    pub fn access_token(&self) -> MedusaResult<String> {
        let mut credential = self
            .store
            .load(&self.config)?
            .ok_or_else(|| authentication_error("GitHub OAuth login is not configured", false))?;
        if needs_refresh(credential.expires_at) {
            self.refresh()?;
            credential = self
                .store
                .load(&self.config)?
                .ok_or_else(|| authentication_error("refreshed GitHub credential is missing", false))?;
        }
        if expired(credential.expires_at) {
            return Err(authentication_error(
                "GitHub OAuth token expired and could not be refreshed",
                false,
            ));
        }
        Ok(credential.access_token)
    }

    pub fn logout(&self) -> MedusaResult<bool> {
        self.store.delete(&self.config)
    }

    fn fetch_login(&self, token: &str) -> MedusaResult<String> {
        let endpoint = api_endpoint(&self.config.api_base_url(), "user")?;
        let response = self.transport.get_bearer(
            endpoint.as_str(),
            token,
            &self.config.api_version,
            MAX_OAUTH_RESPONSE_BYTES,
        )?;
        require_success(&response, "read authenticated GitHub user")?;
        let payload: AuthenticatedUser = serde_json::from_str(&response.body)
            .map_err(|error| protocol_error(format!("decode authenticated GitHub user: {error}")))?;
        if payload.login.trim().is_empty() {
            return Err(protocol_error("GitHub authenticated user response had no login"));
        }
        Ok(payload.login)
    }
}

#[derive(Debug, Deserialize)]
struct DeviceCodeResponse {
    device_code: String,
    user_code: String,
    verification_uri: String,
    expires_in: u32,
    interval: u32,
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    #[serde(default)]
    access_token: Option<String>,
    #[serde(default)]
    token_type: Option<String>,
    #[serde(default)]
    expires_in: Option<i64>,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    refresh_token_expires_in: Option<i64>,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    error_description: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AuthenticatedUser {
    login: String,
}

fn credential_from_token_response(payload: TokenResponse) -> MedusaResult<GitHubOAuthCredential> {
    let access_token = payload
        .access_token
        .filter(|value| !value.is_empty())
        .ok_or_else(|| protocol_error("GitHub OAuth response contained no access token"))?;
    let token_type = payload.token_type.unwrap_or_else(|| "bearer".into());
    if !token_type.eq_ignore_ascii_case("bearer") {
        return Err(protocol_error("GitHub OAuth returned an unsupported token type"));
    }
    let obtained_at = now();
    Ok(GitHubOAuthCredential {
        access_token,
        token_type,
        expires_at: payload
            .expires_in
            .filter(|value| *value > 0)
            .map(|value| obtained_at.saturating_add(value)),
        refresh_token: payload.refresh_token.filter(|value| !value.is_empty()),
        refresh_expires_at: payload
            .refresh_token_expires_in
            .filter(|value| *value > 0)
            .map(|value| obtained_at.saturating_add(value)),
        scope: payload.scope.unwrap_or_default(),
        login: None,
        obtained_at,
    })
}

fn read_response(
    response: reqwest::blocking::Response,
    max_bytes: usize,
) -> MedusaResult<OAuthHttpResponse> {
    let status = response.status().as_u16();
    let headers = header_map(&response.headers());
    let mut reader = response;
    let mut retained = Vec::with_capacity(max_bytes.min(8_192));
    let mut buffer = [0_u8; 8_192];
    loop {
        let read = reader.read(&mut buffer).map_err(http_io_error)?;
        if read == 0 {
            break;
        }
        let remaining = max_bytes.saturating_add(1).saturating_sub(retained.len());
        retained.extend_from_slice(&buffer[..read.min(remaining)]);
    }
    if retained.len() > max_bytes {
        return Err(protocol_error("GitHub OAuth response exceeded its bounded limit"));
    }
    Ok(OAuthHttpResponse {
        status,
        headers,
        body: String::from_utf8_lossy(&retained).into_owned(),
    })
}

fn header_map(headers: &HeaderMap) -> BTreeMap<String, String> {
    headers
        .iter()
        .filter_map(|(name, value)| {
            value
                .to_str()
                .ok()
                .map(|value| (name.as_str().to_ascii_lowercase(), value.to_owned()))
        })
        .collect()
}

fn require_success(response: &OAuthHttpResponse, action: &str) -> MedusaResult<()> {
    if (200..300).contains(&response.status) {
        return Ok(());
    }
    Err(authentication_error(
        format!("{action} failed with HTTP {}", response.status),
        matches!(response.status, 429 | 500 | 502 | 503 | 504),
    ))
}

fn oauth_endpoint(base: &str, path: &str) -> MedusaResult<Url> {
    join_url(base, path, "OAuth")
}

fn api_endpoint(base: &str, path: &str) -> MedusaResult<Url> {
    join_url(base, path, "API")
}

fn join_url(base: &str, path: &str, label: &str) -> MedusaResult<Url> {
    let mut base = Url::parse(base)
        .map_err(|_| validation_error(format!("GitHub {label} base URL is invalid")))?;
    if !base.path().ends_with('/') {
        let path = format!("{}/", base.path().trim_end_matches('/'));
        base.set_path(&path);
    }
    base.join(path)
        .map_err(|_| validation_error(format!("GitHub {label} endpoint is invalid")))
}

fn validate_hostname(hostname: &str) -> MedusaResult<()> {
    if hostname.is_empty()
        || hostname.len() > 253
        || hostname.contains('/')
        || hostname.contains(':')
        || hostname.contains('\\')
        || hostname
            .split('.')
            .any(|part| part.is_empty() || !part.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'-'))
    {
        return Err(validation_error("GitHub hostname is invalid"));
    }
    Ok(())
}

fn validate_base_url(label: &str, value: Option<&str>) -> MedusaResult<()> {
    let Some(value) = value else {
        return Ok(());
    };
    let parsed = Url::parse(value)
        .map_err(|_| validation_error(format!("{label} must be an absolute HTTPS URL")))?;
    if parsed.scheme() != "https"
        || parsed.host_str().is_none()
        || parsed.username() != ""
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        return Err(validation_error(format!(
            "{label} must be HTTPS without credentials, query, or fragment"
        )));
    }
    Ok(())
}

fn validate_api_version(value: &str) -> MedusaResult<()> {
    let bytes = value.as_bytes();
    if bytes.len() != 10
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || bytes.iter().enumerate().any(|(index, byte)| {
            !matches!(index, 4 | 7) && !byte.is_ascii_digit()
        })
    {
        return Err(validation_error("GitHub API version must use YYYY-MM-DD"));
    }
    Ok(())
}

fn expired(expires_at: Option<i64>) -> bool {
    expires_at.is_some_and(|expires_at| expires_at <= now())
}

fn needs_refresh(expires_at: Option<i64>) -> bool {
    expires_at.is_some_and(|expires_at| expires_at <= now() + TOKEN_REFRESH_SKEW_SECONDS)
}

fn now() -> i64 {
    OffsetDateTime::now_utc().unix_timestamp()
}

fn default_api_version() -> String {
    DEFAULT_GITHUB_API_VERSION.into()
}

fn keyring_error(error: KeyringError) -> MedusaError {
    credential_error(format!("operating-system credential store: {error}"))
}

fn credential_error(message: impl Into<String>) -> MedusaError {
    MedusaError::new(
        ErrorCode::PersistenceFailed,
        ErrorCategory::Environment,
        message,
    )
}

fn authentication_error(message: impl Into<String>, retryable: bool) -> MedusaError {
    MedusaError::new(
        ErrorCode::DependencyUnavailable,
        ErrorCategory::Environment,
        message,
    )
    .with_retryable(retryable)
}

fn protocol_error(message: impl Into<String>) -> MedusaError {
    MedusaError::new(
        ErrorCode::IncompatibleProtocol,
        ErrorCategory::Execution,
        message,
    )
}

fn validation_error(message: impl Into<String>) -> MedusaError {
    MedusaError::new(
        ErrorCode::InvalidConfiguration,
        ErrorCategory::Validation,
        message,
    )
}

fn http_error(error: reqwest::Error) -> MedusaError {
    MedusaError::new(
        ErrorCode::DependencyUnavailable,
        ErrorCategory::Transient,
        format!("GitHub OAuth HTTP transport: {error}"),
    )
    .with_retryable(error.is_timeout() || error.is_connect())
}

fn http_io_error(error: std::io::Error) -> MedusaError {
    MedusaError::new(
        ErrorCode::DependencyUnavailable,
        ErrorCategory::Transient,
        format!("read GitHub OAuth response: {error}"),
    )
    .with_retryable(true)
}

#[cfg(test)]
mod tests {
    use std::{collections::VecDeque, sync::Mutex};

    use super::*;

    #[derive(Debug, Default)]
    struct FakeTransport {
        responses: Mutex<VecDeque<OAuthHttpResponse>>,
        forms: Mutex<Vec<BTreeMap<String, String>>>,
    }

    impl FakeTransport {
        fn with(responses: Vec<OAuthHttpResponse>) -> Self {
            Self {
                responses: Mutex::new(responses.into()),
                forms: Mutex::new(Vec::new()),
            }
        }
    }

    impl GitHubOAuthTransport for FakeTransport {
        fn post_form(
            &self,
            _url: &str,
            form: &BTreeMap<String, String>,
            _max_bytes: usize,
        ) -> MedusaResult<OAuthHttpResponse> {
            self.forms.lock().expect("forms").push(form.clone());
            self.responses
                .lock()
                .expect("responses")
                .pop_front()
                .ok_or_else(|| protocol_error("missing fake response"))
        }

        fn get_bearer(
            &self,
            _url: &str,
            _token: &str,
            _api_version: &str,
            _max_bytes: usize,
        ) -> MedusaResult<OAuthHttpResponse> {
            Ok(OAuthHttpResponse {
                status: 200,
                headers: BTreeMap::new(),
                body: r#"{"login":"octocat"}"#.into(),
            })
        }
    }

    fn response(body: &str) -> OAuthHttpResponse {
        OAuthHttpResponse {
            status: 200,
            headers: BTreeMap::new(),
            body: body.into(),
        }
    }

    fn config() -> GitHubOAuthConfig {
        GitHubOAuthConfig {
            client_id: "Iv1.public-client".into(),
            hostname: "github.com".into(),
            oauth_base_url: None,
            api_base_url: None,
            api_version: DEFAULT_GITHUB_API_VERSION.into(),
        }
    }

    #[test]
    fn device_flow_handles_pending_and_slow_down_without_client_secret() {
        let transport = FakeTransport::with(vec![
            response(r#"{"device_code":"device-secret","user_code":"ABCD-EFGH","verification_uri":"https://github.com/login/device","expires_in":900,"interval":1}"#),
            response(r#"{"error":"authorization_pending"}"#),
            response(r#"{"error":"slow_down"}"#),
            response(r#"{"access_token":"ghu_secret","token_type":"bearer","expires_in":28800,"refresh_token":"ghr_secret","refresh_token_expires_in":15897600,"scope":""}"#),
        ]);
        let store = MemoryGitHubCredentialStore::default();
        let client = GitHubOAuthClient::new(config(), store.clone(), transport).expect("client");
        let authorization = client.start_device_flow().expect("start");
        let mut sleeps = Vec::new();
        let status = client
            .complete_device_flow_with(&authorization, |duration| sleeps.push(duration.as_secs()))
            .expect("complete");
        assert!(status.authenticated);
        assert_eq!(status.login.as_deref(), Some("octocat"));
        assert_eq!(sleeps, vec![1, 6]);
        let stored = store.load(client.config()).expect("load").expect("credential");
        assert_eq!(stored.access_token, "ghu_secret");
    }

    #[test]
    fn refresh_rotates_both_tokens() {
        let store = MemoryGitHubCredentialStore::default();
        store
            .save(
                &config(),
                &GitHubOAuthCredential {
                    access_token: "old".into(),
                    token_type: "bearer".into(),
                    expires_at: Some(now() - 1),
                    refresh_token: Some("refresh-old".into()),
                    refresh_expires_at: Some(now() + 1000),
                    scope: String::new(),
                    login: Some("octocat".into()),
                    obtained_at: now() - 100,
                },
            )
            .expect("save");
        let client = GitHubOAuthClient::new(
            config(),
            store.clone(),
            FakeTransport::with(vec![response(r#"{"access_token":"new","token_type":"bearer","expires_in":28800,"refresh_token":"refresh-new","refresh_token_expires_in":15897600,"scope":""}"#)]),
        )
        .expect("client");
        client.refresh().expect("refresh");
        let stored = store.load(client.config()).expect("load").expect("credential");
        assert_eq!(stored.access_token, "new");
        assert_eq!(stored.refresh_token.as_deref(), Some("refresh-new"));
    }

    #[test]
    fn debug_and_status_never_expose_tokens() {
        let credential = GitHubOAuthCredential {
            access_token: "access-secret".into(),
            token_type: "bearer".into(),
            expires_at: None,
            refresh_token: Some("refresh-secret".into()),
            refresh_expires_at: None,
            scope: String::new(),
            login: None,
            obtained_at: now(),
        };
        let debug = format!("{credential:?}");
        assert!(!debug.contains("access-secret"));
        assert!(!debug.contains("refresh-secret"));
    }
}
