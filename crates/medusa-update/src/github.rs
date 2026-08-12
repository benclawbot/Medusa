use std::{
    collections::BTreeMap,
    fs::{self, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant},
};

use medusa_core::MedusaResult;
use reqwest::{
    StatusCode,
    blocking::{Client, Response},
    header::{ACCEPT, ETAG, IF_NONE_MATCH, RANGE},
};
use semver::Version;

use crate::{
    Artifact, DownloadReport, Release,
    manifest::{MANIFEST_NAME, Platform, SIGNATURE_NAME, TrustStore},
    model::{invalid, verify_artifact},
};

mod support;

use support::{GithubAsset, GithubRelease, atomic_write, http_error, read_bounded, sync_parent};

const GITHUB_API: &str = "https://api.github.com";
const MAX_RELEASE_METADATA: usize = 4 * 1024 * 1024;
const MAX_MANIFEST: usize = 1024 * 1024;
const MAX_SIGNATURE: usize = 16 * 1024;
const DOWNLOAD_ATTEMPTS: u32 = 3;
const _: () = assert!(MAX_SIGNATURE < MAX_MANIFEST);

/// Discovers a published release and downloads only assets authorized by its signed manifest.
pub trait ReleaseClient {
    /// Returns `None` when the repository has not published a stable release yet.
    fn latest(&self) -> MedusaResult<Option<Release>>;
    fn download(
        &self,
        artifact: &Artifact,
        destination: &Path,
        progress: impl FnMut(u64, Option<u64>),
    ) -> MedusaResult<DownloadReport>;
}

/// GitHub Releases API client with an embedded Ed25519 trust store.
pub struct GithubReleaseClient {
    client: Client,
    api_base: String,
    repository: String,
    trust_store: TrustStore,
    cache_dir: Option<PathBuf>,
}

impl GithubReleaseClient {
    pub fn public() -> MedusaResult<Self> {
        Self::new("benclawbot/Medusa", GITHUB_API)
    }

    pub fn new(repository: impl Into<String>, api_base: impl Into<String>) -> MedusaResult<Self> {
        Self::with_trust_store(repository, api_base, TrustStore::production())
    }

    pub fn with_trust_store(
        repository: impl Into<String>,
        api_base: impl Into<String>,
        trust_store: TrustStore,
    ) -> MedusaResult<Self> {
        let client = Client::builder()
            .user_agent("medusa-updater/2")
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(15 * 60))
            .build()
            .map_err(http_error)?;
        Ok(Self {
            client,
            api_base: api_base.into().trim_end_matches('/').to_owned(),
            repository: repository.into(),
            trust_store,
            cache_dir: None,
        })
    }

    #[must_use]
    pub fn with_cache_dir(mut self, cache_dir: impl Into<PathBuf>) -> Self {
        self.cache_dir = Some(cache_dir.into());
        self
    }

    fn response(&self, url: &str, range_start: Option<u64>) -> MedusaResult<Response> {
        let mut request = self
            .client
            .get(url)
            .header(ACCEPT, "application/octet-stream");
        if let Some(start) = range_start {
            request = request.header(RANGE, format!("bytes={start}-"));
        }
        request
            .send()
            .map_err(http_error)?
            .error_for_status()
            .map_err(http_error)
    }

    fn release_metadata(&self, url: &str) -> MedusaResult<Option<Vec<u8>>> {
        let cached = self.read_cache();
        let mut request = self
            .client
            .get(url)
            .header(ACCEPT, "application/vnd.github+json");
        if let Some((etag, _)) = cached.as_ref() {
            request = request.header(IF_NONE_MATCH, etag);
        }
        let response = request.send().map_err(http_error)?;
        if response.status() == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if response.status() == StatusCode::NOT_MODIFIED {
            return cached
                .map(|(_, body)| Some(body))
                .ok_or_else(|| invalid("GitHub returned 304 without cached release metadata"));
        }
        let response = response.error_for_status().map_err(http_error)?;
        let etag = response
            .headers()
            .get(ETAG)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let body = read_bounded(response, MAX_RELEASE_METADATA, "release metadata")?;
        if let Some(etag) = etag {
            self.write_cache(&etag, &body)?;
        }
        Ok(Some(body))
    }

    fn read_cache(&self) -> Option<(String, Vec<u8>)> {
        let directory = self.cache_dir.as_ref()?;
        let etag = fs::read_to_string(directory.join("latest.etag")).ok()?;
        let body = fs::read(directory.join("latest.json")).ok()?;
        if body.is_empty() || body.len() > MAX_RELEASE_METADATA {
            return None;
        }
        Some((etag.trim().to_owned(), body))
    }

    fn write_cache(&self, etag: &str, body: &[u8]) -> MedusaResult<()> {
        let Some(directory) = self.cache_dir.as_ref() else {
            return Ok(());
        };
        fs::create_dir_all(directory)?;
        atomic_write(&directory.join("latest.etag"), etag.as_bytes())?;
        atomic_write(&directory.join("latest.json"), body)?;
        Ok(())
    }

    fn signed_manifest(
        &self,
        manifest_asset: &GithubAsset,
        signature_asset: &GithubAsset,
    ) -> MedusaResult<crate::manifest::VerifiedManifest> {
        if manifest_asset.size as usize > MAX_MANIFEST
            || signature_asset.size as usize > MAX_SIGNATURE
        {
            return Err(invalid(
                "release manifest or signature exceeds its size limit",
            ));
        }
        let manifest = read_bounded(
            self.response(&manifest_asset.browser_download_url, None)?,
            MAX_MANIFEST,
            "release manifest",
        )?;
        let signature = read_bounded(
            self.response(&signature_asset.browser_download_url, None)?,
            MAX_SIGNATURE,
            "release signature",
        )?;
        if manifest.len() as u64 != manifest_asset.size
            || signature.len() as u64 != signature_asset.size
        {
            return Err(invalid(
                "release manifest or signature byte count differs from GitHub metadata",
            ));
        }
        self.trust_store
            .verify(&manifest, &signature)
            .map_err(|error| invalid(format!("release trust verification failed: {error}")))
    }
}

impl ReleaseClient for GithubReleaseClient {
    fn latest(&self) -> MedusaResult<Option<Release>> {
        let url = format!(
            "{}/repos/{}/releases/latest",
            self.api_base, self.repository
        );
        let Some(body) = self.release_metadata(&url)? else {
            return Ok(None);
        };
        let release: GithubRelease = serde_json::from_slice(&body)
            .map_err(|error| invalid(format!("invalid GitHub release response: {error}")))?;
        if release.draft || release.prerelease {
            return Err(invalid(
                "latest GitHub release must be published and stable",
            ));
        }
        let tagged_version = Version::parse(release.tag_name.trim_start_matches('v'))
            .map_err(|error| invalid(format!("release tag is not semantic version: {error}")))?;
        let assets = release
            .assets
            .iter()
            .map(|asset| (asset.name.as_str(), asset))
            .collect::<BTreeMap<_, _>>();
        let manifest_asset = assets
            .get(MANIFEST_NAME)
            .ok_or_else(|| invalid(format!("release is missing {MANIFEST_NAME}")))?;
        let signature_asset = assets
            .get(SIGNATURE_NAME)
            .ok_or_else(|| invalid(format!("release is missing {SIGNATURE_NAME}")))?;

        // No release field other than the two fixed bootstrap asset names is trusted before this.
        let verified = self.signed_manifest(manifest_asset, signature_asset)?;
        if verified.manifest.version != tagged_version {
            return Err(invalid(format!(
                "signed version {} does not match release tag {}",
                verified.manifest.version, release.tag_name
            )));
        }
        let artifacts = verified
            .manifest
            .artifacts
            .iter()
            .map(|entry| {
                let asset = assets.get(entry.name.as_str()).ok_or_else(|| {
                    invalid(format!(
                        "signed artifact {} is absent from the GitHub release",
                        entry.name
                    ))
                })?;
                if asset.size != entry.bytes {
                    return Err(invalid(format!(
                        "GitHub size for {} differs from the signed manifest",
                        entry.name
                    )));
                }
                Ok(Artifact::from_manifest(
                    entry,
                    asset.browser_download_url.clone(),
                ))
            })
            .collect::<MedusaResult<Vec<_>>>()?;
        let platform = Platform::current()
            .map_err(|error| invalid(format!("cannot select update artifact: {error}")))?;
        verified
            .manifest
            .select_cli(platform)
            .map_err(|error| invalid(format!("release is incompatible: {error}")))?;
        Ok(Some(Release::from_verified(
            self.repository.clone(),
            verified,
            artifacts,
        )))
    }

    fn download(
        &self,
        artifact: &Artifact,
        destination: &Path,
        mut progress: impl FnMut(u64, Option<u64>),
    ) -> MedusaResult<DownloadReport> {
        let started = Instant::now();
        let partial = destination.with_extension(format!(
            "{}part",
            destination
                .extension()
                .and_then(|value| value.to_str())
                .map(|value| format!("{value}."))
                .unwrap_or_default()
        ));
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)?;
        }
        let existing = fs::metadata(&partial).map(|meta| meta.len()).unwrap_or(0);
        if existing > artifact.bytes {
            fs::remove_file(&partial)?;
        }

        let mut retries = 0_u32;
        for attempt in 0..DOWNLOAD_ATTEMPTS {
            let offset = fs::metadata(&partial).map(|meta| meta.len()).unwrap_or(0);
            if offset == artifact.bytes {
                break;
            }
            let response = match self.response(&artifact.browser_download_url, Some(offset)) {
                Ok(response) => response,
                Err(error) if attempt + 1 < DOWNLOAD_ATTEMPTS => {
                    retries += 1;
                    thread::sleep(Duration::from_millis(250 * u64::from(retries)));
                    let _ = error;
                    continue;
                }
                Err(error) => return Err(error),
            };
            let append = offset > 0 && response.status() == StatusCode::PARTIAL_CONTENT;
            let mut file = if append {
                OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&partial)?
            } else {
                let mut file = OpenOptions::new()
                    .create(true)
                    .write(true)
                    .truncate(true)
                    .open(&partial)?;
                file.seek(SeekFrom::Start(0))?;
                file
            };
            let mut response = response;
            let mut buffer = [0_u8; 64 * 1024];
            let mut downloaded = if append { offset } else { 0 };
            loop {
                let read = match response.read(&mut buffer) {
                    Ok(read) => read,
                    Err(error) if attempt + 1 < DOWNLOAD_ATTEMPTS => {
                        let _ = error;
                        break;
                    }
                    Err(error) => return Err(http_error(error)),
                };
                if read == 0 {
                    break;
                }
                downloaded = downloaded
                    .checked_add(read as u64)
                    .ok_or_else(|| invalid("download byte count overflow"))?;
                if downloaded > artifact.bytes {
                    return Err(invalid(format!(
                        "download exceeded signed byte count for {}",
                        artifact.name
                    )));
                }
                file.write_all(&buffer[..read])?;
                progress(downloaded, Some(artifact.bytes));
            }
            file.sync_all()?;
            if downloaded == artifact.bytes {
                break;
            }
            if attempt + 1 == DOWNLOAD_ATTEMPTS {
                return Err(invalid(format!(
                    "download remained incomplete after {DOWNLOAD_ATTEMPTS} attempts for {}",
                    artifact.name
                )));
            }
            retries += 1;
            thread::sleep(Duration::from_millis(250 * u64::from(retries)));
        }

        verify_artifact(&partial, artifact.bytes, &artifact.sha256)?;
        fs::rename(&partial, destination)?;
        sync_parent(destination)?;
        Ok(DownloadReport::new(
            artifact.bytes,
            retries,
            started.elapsed(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn github_enterprise_api_base_is_preserved() {
        let client = GithubReleaseClient::with_trust_store(
            "octo/medusa",
            "https://github.example/api/v3",
            TrustStore::production(),
        )
        .expect("client");
        assert_eq!(client.api_base, "https://github.example/api/v3");
        assert_eq!(client.repository, "octo/medusa");
    }
}
