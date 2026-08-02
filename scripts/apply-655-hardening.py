#!/usr/bin/env python3
"""Apply final fail-closed hardening to the verified updater."""

from __future__ import annotations

import re
from pathlib import Path


def replace_once(text: str, old: str, new: str, label: str) -> str:
    count = text.count(old)
    if count != 1:
        raise RuntimeError(f"{label}: expected one match, found {count}")
    return text.replace(old, new, 1)


def update_manifest() -> None:
    path = Path("crates/medusa-update/src/manifest.rs")
    text = path.read_text(encoding="utf-8")
    text = replace_once(
        text,
        "use std::collections::HashSet;",
        "use std::collections::{HashMap, HashSet};",
        "manifest collection imports",
    )
    struct_names = (
        "Platform",
        "ManifestArtifact",
        "BuildSource",
        "RolloutPolicy",
        "ReleaseManifest",
        "ManifestSignature",
    )
    for name in struct_names:
        pattern = re.compile(
            rf"(#\[derive\([^\n]*Deserialize[^\n]*\)\]\n)(pub struct {name} \{{)"
        )
        text, count = pattern.subn(r"\1#[serde(deny_unknown_fields)]\n\2", text, count=1)
        if count != 1:
            raise RuntimeError(f"deny unknown fields for {name}: expected one match")

    text = replace_once(
        text,
        '''pub struct ManifestArtifact {
    pub name: String,
    pub kind: ArtifactKind,
    pub platform: Platform,
    pub target: String,
    pub bytes: u64,
    pub sha256: String,
}

''',
        '''pub struct ManifestArtifact {
    pub name: String,
    pub kind: ArtifactKind,
    pub platform: Platform,
    pub target: String,
    pub bytes: u64,
    pub sha256: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReleaseEvidence {
    pub name: String,
    pub bytes: u64,
    pub sha256: String,
}

''',
        "release evidence struct",
    )
    text = replace_once(
        text,
        '''    pub rollout: RolloutPolicy,
    pub artifacts: Vec<ManifestArtifact>,
}''',
        '''    pub rollout: RolloutPolicy,
    pub artifacts: Vec<ManifestArtifact>,
    pub evidence: Vec<ReleaseEvidence>,
}''',
        "release evidence field",
    )
    text = replace_once(
        text,
        '''        let mut names = HashSet::new();
        for artifact in &self.artifacts {
            validate_artifact(artifact)?;
            if !names.insert(artifact.name.as_str()) {
                return Err(ManifestError::InvalidField(format!(
                    "duplicate artifact {}",
                    artifact.name
                )));
            }
        }
        Ok(())''',
        '''        let mut names = HashSet::new();
        for artifact in &self.artifacts {
            validate_artifact(artifact)?;
            if !names.insert(artifact.name.as_str()) {
                return Err(ManifestError::InvalidField(format!(
                    "duplicate artifact {}",
                    artifact.name
                )));
            }
        }
        if self.evidence.is_empty() {
            return Err(ManifestError::InvalidField(
                "manifest contains no release evidence".to_owned(),
            ));
        }
        let mut evidence_by_name = HashMap::new();
        for evidence in &self.evidence {
            validate_evidence(evidence)?;
            if evidence_by_name
                .insert(evidence.name.as_str(), evidence)
                .is_some()
            {
                return Err(ManifestError::InvalidField(format!(
                    "duplicate evidence {}",
                    evidence.name
                )));
            }
        }
        for artifact in &self.artifacts {
            let evidence = evidence_by_name.get(artifact.name.as_str()).ok_or_else(|| {
                ManifestError::InvalidField(format!(
                    "artifact {} is missing release evidence",
                    artifact.name
                ))
            })?;
            if evidence.bytes != artifact.bytes || evidence.sha256 != artifact.sha256 {
                return Err(ManifestError::InvalidField(format!(
                    "artifact {} disagrees with release evidence",
                    artifact.name
                )));
            }
        }
        Ok(())''',
        "manifest evidence validation",
    )
    text = replace_once(
        text,
        '''fn validate_artifact(artifact: &ManifestArtifact) -> Result<(), ManifestError> {
    let path = Path::new(&artifact.name);''',
        '''fn validate_artifact(artifact: &ManifestArtifact) -> Result<(), ManifestError> {
    validate_evidence(&ReleaseEvidence {
        name: artifact.name.clone(),
        bytes: artifact.bytes,
        sha256: artifact.sha256.clone(),
    })?;
    let path = Path::new(&artifact.name);''',
        "artifact evidence validation",
    )
    text = replace_once(
        text,
        '''    Ok(())
}

#[cfg(test)]''',
        '''    Ok(())
}

fn validate_evidence(evidence: &ReleaseEvidence) -> Result<(), ManifestError> {
    let path = Path::new(&evidence.name);
    if evidence.name.is_empty()
        || path.is_absolute()
        || path.components().count() != 1
        || !matches!(path.components().next(), Some(Component::Normal(_)))
    {
        return Err(ManifestError::InvalidField(format!(
            "unsafe evidence name {}",
            evidence.name
        )));
    }
    if evidence.bytes == 0 {
        return Err(ManifestError::InvalidField(format!(
            "evidence {} has zero bytes",
            evidence.name
        )));
    }
    validate_hex("evidence.sha256", &evidence.sha256, 64)
}

#[cfg(test)]''',
        "evidence validation helper",
    )
    text = replace_once(
        text,
        '''            artifacts: vec![ManifestArtifact {
                name: "medusa-cli-linux-x86_64.tar.gz".to_owned(),
                kind: ArtifactKind::CliArchive,
                platform: Platform {
                    os: OperatingSystem::Linux,
                    architecture: Architecture::X86_64,
                },
                target: "x86_64-unknown-linux-gnu".to_owned(),
                bytes: 12,
                sha256: "d".repeat(64),
            }],
        }''',
        '''            artifacts: vec![ManifestArtifact {
                name: "medusa-cli-linux-x86_64.tar.gz".to_owned(),
                kind: ArtifactKind::CliArchive,
                platform: Platform {
                    os: OperatingSystem::Linux,
                    architecture: Architecture::X86_64,
                },
                target: "x86_64-unknown-linux-gnu".to_owned(),
                bytes: 12,
                sha256: "d".repeat(64),
            }],
            evidence: vec![ReleaseEvidence {
                name: "medusa-cli-linux-x86_64.tar.gz".to_owned(),
                bytes: 12,
                sha256: "d".repeat(64),
            }],
        }''',
        "fixture release evidence",
    )
    text = replace_once(
        text,
        '''    #[test]
    fn rejects_unknown_and_revoked_keys() {''',
        '''    #[test]
    fn rejects_unknown_manifest_fields() {
        let mut value = serde_json::to_value(fixture_manifest(1)).expect("manifest value");
        value
            .as_object_mut()
            .expect("manifest object")
            .insert("unexpected".to_owned(), serde_json::Value::Bool(true));
        assert!(serde_json::from_value::<ReleaseManifest>(value).is_err());
    }

    #[test]
    fn rejects_unknown_and_revoked_keys() {''',
        "unknown manifest field fixture",
    )
    path.write_text(text, encoding="utf-8")


def update_github_download() -> None:
    path = Path("crates/medusa-update/src/github.rs")
    text = path.read_text(encoding="utf-8")
    old = '''        let mut retries = 0_u32;
        while retries < DOWNLOAD_ATTEMPTS {
            let offset = fs::metadata(&partial).map(|meta| meta.len()).unwrap_or(0);
            if offset == artifact.bytes {
                break;
            }
            let response = match self.response(&artifact.browser_download_url, Some(offset)) {
                Ok(response) => response,
                Err(error) if retries + 1 < DOWNLOAD_ATTEMPTS => {
                    retries += 1;
                    thread::sleep(Duration::from_millis(250 * u64::from(retries)));
                    let _ = error;
                    continue;
                }
                Err(error) => return Err(error),
            };
            let append = offset > 0 && response.status() == StatusCode::PARTIAL_CONTENT;
            let mut file = if append {
                OpenOptions::new().create(true).append(true).open(&partial)?
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
                    Err(error) if retries + 1 < DOWNLOAD_ATTEMPTS => {
                        retries += 1;
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
            thread::sleep(Duration::from_millis(250 * u64::from(retries.max(1))));
        }
'''
    new = '''        let mut retries = 0_u32;
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
                OpenOptions::new().create(true).append(true).open(&partial)?
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
'''
    text = replace_once(text, old, new, "bounded resumable download")
    path.write_text(text, encoding="utf-8")


def update_install() -> None:
    path = Path("crates/medusa-update/src/install.rs")
    text = path.read_text(encoding="utf-8")
    text = replace_once(
        text,
        '''            fs::copy(candidate, &staged)?;
            validate_candidate(&staged)?;''',
        '''            fs::copy(candidate, &staged)?;
            OpenOptions::new().read(true).open(&staged)?.sync_all()?;
            validate_candidate(&staged)?;''',
        "durable staged candidate",
    )
    path.write_text(text, encoding="utf-8")


def update_exports() -> None:
    path = Path("crates/medusa-update/src/lib.rs")
    text = path.read_text(encoding="utf-8")
    text = replace_once(
        text,
        '''    Architecture, ArtifactKind, BuildSource, KeyStatus, ManifestArtifact, ManifestError,
    ManifestSignature, OperatingSystem, Platform, ReleaseManifest, RolloutPolicy, TrustStore,''',
        '''    Architecture, ArtifactKind, BuildSource, KeyStatus, ManifestArtifact, ManifestError,
    ManifestSignature, OperatingSystem, Platform, ReleaseEvidence, ReleaseManifest, RolloutPolicy,
    TrustStore,''',
        "release evidence export",
    )
    path.write_text(text, encoding="utf-8")


if __name__ == "__main__":
    update_manifest()
    update_github_download()
    update_install()
    update_exports()
