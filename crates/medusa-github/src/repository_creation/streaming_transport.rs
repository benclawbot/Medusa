use std::{
    collections::BTreeMap,
    fs,
    io::Read,
    path::{Path, PathBuf},
    time::Duration,
};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use reqwest::{
    blocking::{Body, Client},
    header::{ACCEPT, AUTHORIZATION, CONTENT_LENGTH, CONTENT_TYPE, USER_AGENT},
};

use super::*;

/// Production direct HTTPS transport with file-backed streaming uploads.
#[derive(Clone, Debug)]
pub struct StreamingReqwestGitHubApiTransport {
    client: Client,
    reads: ReqwestGitHubApiTransport,
}

impl StreamingReqwestGitHubApiTransport {
    pub fn new() -> MedusaResult<Self> {
        let client = Client::builder()
            .connect_timeout(Duration::from_secs(20))
            .timeout(Duration::from_secs(300))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(http_error)?;
        Ok(Self {
            client,
            reads: ReqwestGitHubApiTransport::new()?,
        })
    }
}

impl GitHubApiTransport for StreamingReqwestGitHubApiTransport {
    fn execute(&self, request: &GitHubApiRequest, token: &str) -> MedusaResult<GitHubApiResponse> {
        self.reads.execute(request, token)
    }

    fn download(
        &self,
        request: &GitHubApiRequest,
        token: &str,
        destination_root: &Path,
        destination: &Path,
        max_bytes: u64,
    ) -> MedusaResult<(GitHubApiResponse, GitHubArtifactReceipt)> {
        self.reads
            .download(request, token, destination_root, destination, max_bytes)
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
        let file = fs::File::open(&source).map_err(persistence_error)?;
        let mut builder = self
            .client
            .request(request.method.clone(), request.url.clone())
            .header(ACCEPT, &request.accept)
            .header(USER_AGENT, "medusa-github-direct")
            .header(AUTHORIZATION, format!("Bearer {token}"))
            .header("X-GitHub-Api-Version", &request.api_version)
            .header(CONTENT_LENGTH, metadata.len());
        if let Some(content_type) = request.content_type.as_deref() {
            builder = builder.header(CONTENT_TYPE, content_type);
        }
        let response = builder.body(Body::new(file)).send().map_err(http_error)?;
        read_response(response, request.response_limit)
    }
}

fn confined_source(root: &Path, source: &Path) -> MedusaResult<PathBuf> {
    let canonical_root = root.canonicalize().map_err(persistence_error)?;
    let joined = if source.is_absolute() {
        source.to_path_buf()
    } else {
        canonical_root.join(source)
    };
    let canonical = joined.canonicalize().map_err(persistence_error)?;
    if !canonical.starts_with(&canonical_root) {
        return Err(policy_error(
            "release asset source must remain inside the repository root",
        ));
    }
    let filename = canonical
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| validation_error("release asset filename is not valid UTF-8"))?;
    if filename.is_empty()
        || filename.len() > 255
        || matches!(filename, "." | "..")
        || filename
            .chars()
            .any(|character| character.is_control() || matches!(character, '/' | '\\'))
    {
        return Err(validation_error("release asset filename is unsafe"));
    }
    Ok(canonical)
}

fn read_response(
    mut response: reqwest::blocking::Response,
    max_bytes: usize,
) -> MedusaResult<GitHubApiResponse> {
    let status = response.status().as_u16();
    let headers = response
        .headers()
        .iter()
        .filter_map(|(name, value)| {
            value
                .to_str()
                .ok()
                .map(|value| (name.as_str().to_ascii_lowercase(), value.to_owned()))
        })
        .collect::<BTreeMap<_, _>>();
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

fn http_error(error: reqwest::Error) -> MedusaError {
    MedusaError::new(
        ErrorCode::DependencyUnavailable,
        ErrorCategory::Transient,
        format!("GitHub streaming HTTPS transport: {error}"),
    )
    .with_retryable(error.is_timeout() || error.is_connect())
}

fn http_io_error(error: std::io::Error) -> MedusaError {
    MedusaError::new(
        ErrorCode::DependencyUnavailable,
        ErrorCategory::Transient,
        format!("read GitHub streaming HTTPS response: {error}"),
    )
    .with_retryable(true)
}

fn persistence_error(error: std::io::Error) -> MedusaError {
    MedusaError::new(
        ErrorCode::PersistenceFailed,
        ErrorCategory::Persistence,
        error.to_string(),
    )
}

fn validation_error(message: impl Into<String>) -> MedusaError {
    MedusaError::new(ErrorCode::InvalidInput, ErrorCategory::Validation, message)
}

fn policy_error(message: impl Into<String>) -> MedusaError {
    MedusaError::new(ErrorCode::PolicyDenied, ErrorCategory::Policy, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upload_source_symlink_escape_is_rejected() {
        let root = tempfile::tempdir().expect("root");
        let outside = tempfile::tempdir().expect("outside");
        let outside_file = outside.path().join("secret.bin");
        fs::write(&outside_file, b"secret").expect("write");

        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&outside_file, root.path().join("asset.bin"))
                .expect("symlink");
            assert!(confined_source(root.path(), Path::new("asset.bin")).is_err());
        }
        #[cfg(windows)]
        {
            if std::os::windows::fs::symlink_file(&outside_file, root.path().join("asset.bin"))
                .is_ok()
            {
                assert!(confined_source(root.path(), Path::new("asset.bin")).is_err());
            }
        }
    }
}
