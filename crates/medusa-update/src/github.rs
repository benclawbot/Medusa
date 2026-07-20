use std::{collections::BTreeMap, io::Read};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use reqwest::{
    StatusCode,
    blocking::{Client, Response},
};
use semver::Version;
use serde::Deserialize;

use crate::{Artifact, Release, copy_with_progress, model::invalid};

const GITHUB_API: &str = "https://api.github.com";
const MANIFEST_NAME: &str = "medusa-release-manifest.json";

/// Discovers a published release and streams its assets.
pub trait ReleaseClient {
    /// Returns `None` when the repository has not published a stable release yet.
    fn latest(&self) -> MedusaResult<Option<Release>>;
    fn download(
        &self,
        artifact: &Artifact,
        destination: &std::path::Path,
        progress: impl FnMut(u64, Option<u64>),
    ) -> MedusaResult<u64>;
}

/// GitHub Releases API client for Medusa's public repository or an Enterprise host.
pub struct GithubReleaseClient {
    client: Client,
    api_base: String,
    repository: String,
}

impl GithubReleaseClient {
    pub fn public() -> MedusaResult<Self> {
        Self::new("benclawbot/Medusa", GITHUB_API)
    }

    pub fn new(repository: impl Into<String>, api_base: impl Into<String>) -> MedusaResult<Self> {
        let client = Client::builder()
            .user_agent("medusa-updater")
            .build()
            .map_err(http_error)?;
        Ok(Self {
            client,
            api_base: api_base.into().trim_end_matches('/').to_owned(),
            repository: repository.into(),
        })
    }

    fn response(&self, url: &str) -> MedusaResult<Response> {
        self.client
            .get(url)
            .header("Accept", "application/vnd.github+json")
            .send()
            .map_err(http_error)?
            .error_for_status()
            .map_err(http_error)
    }
}

impl ReleaseClient for GithubReleaseClient {
    fn latest(&self) -> MedusaResult<Option<Release>> {
        let url = format!(
            "{}/repos/{}/releases/latest",
            self.api_base, self.repository
        );
        let response = self
            .client
            .get(&url)
            .header("Accept", "application/vnd.github+json")
            .send()
            .map_err(http_error)?;
        if response.status() == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        let release: GithubRelease = response
            .error_for_status()
            .map_err(http_error)?
            .json()
            .map_err(http_error)?;
        if release.draft || release.prerelease {
            return Err(invalid(
                "latest GitHub release must be published and stable",
            ));
        }
        let version = Version::parse(release.tag_name.trim_start_matches('v'))
            .map_err(|error| invalid(format!("release tag is not semantic version: {error}")))?;
        let manifest_asset = release
            .assets
            .iter()
            .find(|asset| asset.name == MANIFEST_NAME)
            .ok_or_else(|| invalid(format!("release is missing {MANIFEST_NAME}")))?;
        let manifest = self.manifest(manifest_asset)?;
        let checksums = manifest
            .assets
            .into_iter()
            .map(|entry| (entry.path.clone(), entry))
            .collect::<BTreeMap<_, _>>();
        let artifacts = release
            .assets
            .iter()
            .filter(|asset| asset.name != MANIFEST_NAME && asset.name != "SHA256SUMS")
            .map(|asset| {
                let entry = checksums.get(&asset.name).ok_or_else(|| {
                    invalid(format!(
                        "release asset {} is absent from signed manifest",
                        asset.name
                    ))
                })?;
                if entry.bytes != asset.size {
                    return Err(invalid(format!(
                        "release asset {} size differs from manifest",
                        asset.name
                    )));
                }
                Ok(Artifact {
                    name: asset.name.clone(),
                    browser_download_url: asset.browser_download_url.clone(),
                    bytes: asset.size,
                    sha256: entry.sha256.clone(),
                })
            })
            .collect::<MedusaResult<Vec<_>>>()?;
        Ok(Some(Release {
            version,
            repository: self.repository.clone(),
            manifest: Artifact {
                name: manifest_asset.name.clone(),
                browser_download_url: manifest_asset.browser_download_url.clone(),
                bytes: manifest_asset.size,
                sha256: manifest_asset.digest.clone().unwrap_or_default(),
            },
            artifacts,
        }))
    }

    fn download(
        &self,
        artifact: &Artifact,
        destination: &std::path::Path,
        progress: impl FnMut(u64, Option<u64>),
    ) -> MedusaResult<u64> {
        let mut response = self.response(&artifact.browser_download_url)?;
        copy_with_progress(&mut response, destination, Some(artifact.bytes), progress)
    }
}

impl GithubReleaseClient {
    fn manifest(&self, asset: &GithubAsset) -> MedusaResult<ReleaseManifest> {
        let mut response = self.response(&asset.browser_download_url)?;
        let mut body = String::new();
        response.read_to_string(&mut body).map_err(http_error)?;
        let manifest: ReleaseManifest = serde_json::from_str(&body)
            .map_err(|error| invalid(format!("invalid release manifest: {error}")))?;
        if manifest.schema != "medusa-release-manifest-v1" {
            return Err(invalid("unsupported release manifest schema"));
        }
        Ok(manifest)
    }
}

#[derive(Deserialize)]
struct GithubRelease {
    tag_name: String,
    draft: bool,
    prerelease: bool,
    assets: Vec<GithubAsset>,
}

#[derive(Deserialize)]
struct GithubAsset {
    name: String,
    browser_download_url: String,
    size: u64,
    #[serde(default)]
    digest: Option<String>,
}

#[derive(Deserialize)]
struct ReleaseManifest {
    schema: String,
    assets: Vec<ManifestEntry>,
}

#[derive(Deserialize)]
struct ManifestEntry {
    path: String,
    bytes: u64,
    sha256: String,
}

fn http_error(error: impl std::fmt::Display) -> MedusaError {
    MedusaError::new(
        ErrorCode::DependencyUnavailable,
        ErrorCategory::Transient,
        format!("GitHub release request failed: {error}"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn github_enterprise_api_base_is_preserved() {
        let client = GithubReleaseClient::new("octo/medusa", "https://github.example/api/v3")
            .expect("client");
        assert_eq!(client.api_base, "https://github.example/api/v3");
        assert_eq!(client.repository, "octo/medusa");
    }
}
