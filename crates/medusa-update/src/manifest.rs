use std::collections::{HashMap, HashSet};
use std::path::{Component, Path};

use ring::signature::{ED25519, UnparsedPublicKey};
use semver::Version;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const MANIFEST_NAME: &str = "medusa-release-manifest.json";
pub const SIGNATURE_NAME: &str = "medusa-release-manifest.sig.json";
pub const MANIFEST_SCHEMA: &str = "medusa-release-manifest-v2";
pub const SIGNATURE_SCHEMA: &str = "medusa-release-signature-v1";
pub const DEFAULT_KEY_ID: &str = "medusa-release-2026-08-primary";
pub const RECOVERY_KEY_ID: &str = "medusa-release-2026-08-recovery";
const PRIMARY_PUBLIC_KEY: [u8; 32] = [
    0x23, 0x2f, 0xdf, 0xfd, 0x05, 0xb5, 0x82, 0xb8, 0x26, 0x0b, 0x68, 0xbf, 0x0c, 0x72, 0xb0, 0x47,
    0xfd, 0xae, 0x6e, 0xae, 0x77, 0x52, 0xf5, 0xa0, 0x7a, 0x15, 0xa8, 0x58, 0x0b, 0x56, 0xdf, 0xe9,
];
const RECOVERY_PUBLIC_KEY: [u8; 32] = [
    0xe6, 0x7f, 0x26, 0x8b, 0x69, 0x65, 0x5b, 0xed, 0x4c, 0xa9, 0x76, 0x89, 0xd4, 0xe2, 0x61, 0xda,
    0x77, 0x28, 0x9c, 0x9c, 0x9e, 0x69, 0x96, 0x38, 0x46, 0xb2, 0x1e, 0x9e, 0x8b, 0xc4, 0x08, 0x2c,
];
const LEGACY_UNUSED_PUBLIC_KEY: [u8; 32] = [
    0x2e, 0xa0, 0x16, 0xf0, 0x0f, 0x81, 0x87, 0x45, 0x3c, 0x66, 0x31, 0x64, 0x4a, 0x9b, 0x47, 0x2a,
    0x9e, 0x1b, 0x6e, 0x6b, 0x28, 0x1f, 0xe6, 0xcb, 0xa8, 0x62, 0x57, 0x8a, 0x2b, 0x85, 0x2f, 0x44,
];

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum OperatingSystem {
    Linux,
    Macos,
    Windows,
}

impl TryFrom<&str> for OperatingSystem {
    type Error = ManifestError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "linux" => Ok(Self::Linux),
            "macos" => Ok(Self::Macos),
            "windows" => Ok(Self::Windows),
            other => Err(ManifestError::UnsupportedPlatform(format!(
                "unsupported operating system {other}"
            ))),
        }
    }
}

impl OperatingSystem {
    pub fn current() -> Result<Self, ManifestError> {
        Self::try_from(std::env::consts::OS)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Architecture {
    X86_64,
    Aarch64,
}

impl TryFrom<&str> for Architecture {
    type Error = ManifestError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "x86_64" => Ok(Self::X86_64),
            "aarch64" => Ok(Self::Aarch64),
            other => Err(ManifestError::UnsupportedPlatform(format!(
                "unsupported architecture {other}"
            ))),
        }
    }
}

impl Architecture {
    pub fn current() -> Result<Self, ManifestError> {
        Self::try_from(std::env::consts::ARCH)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Platform {
    pub os: OperatingSystem,
    pub architecture: Architecture,
}

impl Platform {
    pub fn current() -> Result<Self, ManifestError> {
        Ok(Self {
            os: OperatingSystem::current()?,
            architecture: Architecture::current()?,
        })
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ArtifactKind {
    CliArchive,
    DesktopPackage,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ManifestArtifact {
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

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BuildSource {
    pub repository: String,
    pub revision: String,
    pub rust_toolchain: String,
    pub cargo_lock_sha256: String,
    pub desktop_lock_sha256: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RolloutPolicy {
    pub channel: String,
    pub sequence: u64,
    pub percentage: u8,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReleaseManifest {
    pub schema: String,
    pub version: Version,
    pub minimum_updater_version: Version,
    pub source: BuildSource,
    pub rollout: RolloutPolicy,
    pub artifacts: Vec<ManifestArtifact>,
    pub evidence: Vec<ReleaseEvidence>,
}

impl ReleaseManifest {
    pub fn validate(&self) -> Result<(), ManifestError> {
        if self.schema != MANIFEST_SCHEMA {
            return Err(ManifestError::Schema(self.schema.clone()));
        }
        if self.source.repository != "benclawbot/Medusa" {
            return Err(ManifestError::InvalidField(
                "source.repository must be benclawbot/Medusa".to_owned(),
            ));
        }
        validate_hex("source.revision", &self.source.revision, 40)?;
        validate_hex(
            "source.cargo_lock_sha256",
            &self.source.cargo_lock_sha256,
            64,
        )?;
        validate_hex(
            "source.desktop_lock_sha256",
            &self.source.desktop_lock_sha256,
            64,
        )?;
        if self.source.rust_toolchain.trim().is_empty() {
            return Err(ManifestError::InvalidField(
                "source.rust_toolchain is empty".to_owned(),
            ));
        }
        if self.rollout.channel != "stable" {
            return Err(ManifestError::InvalidField(
                "rollout.channel must be stable".to_owned(),
            ));
        }
        if self.rollout.sequence == 0 {
            return Err(ManifestError::InvalidField(
                "rollout.sequence must be positive".to_owned(),
            ));
        }
        if !(1..=100).contains(&self.rollout.percentage) {
            return Err(ManifestError::InvalidField(
                "rollout.percentage must be in 1..=100".to_owned(),
            ));
        }
        if self.artifacts.is_empty() {
            return Err(ManifestError::InvalidField(
                "manifest contains no artifacts".to_owned(),
            ));
        }
        let mut names = HashSet::new();
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
            let evidence = evidence_by_name
                .get(artifact.name.as_str())
                .ok_or_else(|| {
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
        Ok(())
    }

    pub fn select_cli(&self, platform: Platform) -> Result<&ManifestArtifact, ManifestError> {
        let matches = self
            .artifacts
            .iter()
            .filter(|artifact| {
                artifact.kind == ArtifactKind::CliArchive && artifact.platform == platform
            })
            .collect::<Vec<_>>();
        match matches.as_slice() {
            [artifact] => Ok(*artifact),
            [] => Err(ManifestError::UnsupportedPlatform(format!(
                "no CLI artifact for {:?}/{:?}",
                platform.os, platform.architecture
            ))),
            _ => Err(ManifestError::InvalidField(format!(
                "multiple CLI artifacts for {:?}/{:?}",
                platform.os, platform.architecture
            ))),
        }
    }

    pub fn validate_install_policy(
        &self,
        installed_version: &Version,
        updater_version: &Version,
        installed_sequence: Option<u64>,
        allow_downgrade: bool,
    ) -> Result<(), ManifestError> {
        if updater_version < &self.minimum_updater_version {
            return Err(ManifestError::UpdaterTooOld {
                minimum: self.minimum_updater_version.clone(),
                installed: updater_version.clone(),
            });
        }
        if !allow_downgrade && self.version < *installed_version {
            return Err(ManifestError::DowngradeRefused {
                installed: installed_version.clone(),
                candidate: self.version.clone(),
            });
        }
        if !allow_downgrade
            && installed_sequence.is_some_and(|sequence| self.rollout.sequence < sequence)
        {
            return Err(ManifestError::SequenceRollback {
                installed: installed_sequence.unwrap_or_default(),
                candidate: self.rollout.sequence,
            });
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ManifestSignature {
    pub schema: String,
    pub key_id: String,
    pub algorithm: String,
    pub manifest_sha256: String,
    pub signature: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum KeyStatus {
    Active,
    Revoked,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TrustedKey {
    pub key_id: String,
    pub public_key: [u8; 32],
    pub status: KeyStatus,
    pub first_sequence: u64,
    pub last_sequence: Option<u64>,
}

#[derive(Clone, Debug, Default)]
pub struct TrustStore {
    keys: Vec<TrustedKey>,
}

impl TrustStore {
    pub fn production() -> Self {
        Self {
            keys: vec![
                TrustedKey {
                    key_id: DEFAULT_KEY_ID.to_owned(),
                    public_key: PRIMARY_PUBLIC_KEY,
                    status: KeyStatus::Active,
                    first_sequence: 1,
                    last_sequence: None,
                },
                TrustedKey {
                    key_id: RECOVERY_KEY_ID.to_owned(),
                    public_key: RECOVERY_PUBLIC_KEY,
                    status: KeyStatus::Active,
                    first_sequence: 1,
                    last_sequence: None,
                },
                TrustedKey {
                    key_id: "medusa-release-2026-01".to_owned(),
                    public_key: LEGACY_UNUSED_PUBLIC_KEY,
                    status: KeyStatus::Revoked,
                    first_sequence: 1,
                    last_sequence: Some(1),
                },
            ],
        }
    }

    pub fn new(keys: Vec<TrustedKey>) -> Result<Self, ManifestError> {
        let mut ids = HashSet::new();
        for key in &keys {
            if key.key_id.trim().is_empty() || !ids.insert(key.key_id.as_str()) {
                return Err(ManifestError::InvalidKeyring);
            }
            if key.first_sequence == 0
                || key
                    .last_sequence
                    .is_some_and(|last| last < key.first_sequence)
            {
                return Err(ManifestError::InvalidKeyring);
            }
        }
        Ok(Self { keys })
    }

    pub fn verify(
        &self,
        manifest_bytes: &[u8],
        signature_bytes: &[u8],
    ) -> Result<VerifiedManifest, ManifestError> {
        const MAX_MANIFEST_BYTES: usize = 1024 * 1024;
        const MAX_SIGNATURE_BYTES: usize = 16 * 1024;
        if manifest_bytes.is_empty() || manifest_bytes.len() > MAX_MANIFEST_BYTES {
            return Err(ManifestError::Size("manifest"));
        }
        if signature_bytes.is_empty() || signature_bytes.len() > MAX_SIGNATURE_BYTES {
            return Err(ManifestError::Size("signature"));
        }
        let envelope: ManifestSignature = serde_json::from_slice(signature_bytes)
            .map_err(|error| ManifestError::Json(error.to_string()))?;
        if envelope.schema != SIGNATURE_SCHEMA || envelope.algorithm != "Ed25519" {
            return Err(ManifestError::SignatureEnvelope);
        }
        let key = self
            .keys
            .iter()
            .find(|key| key.key_id == envelope.key_id)
            .ok_or_else(|| ManifestError::UnknownKey(envelope.key_id.clone()))?;
        if key.status == KeyStatus::Revoked {
            return Err(ManifestError::RevokedKey(key.key_id.clone()));
        }
        let digest = hex::encode(Sha256::digest(manifest_bytes));
        if envelope.manifest_sha256 != digest {
            return Err(ManifestError::ManifestDigest);
        }
        let signature =
            hex::decode(&envelope.signature).map_err(|_| ManifestError::SignatureEnvelope)?;
        if signature.len() != 64 {
            return Err(ManifestError::SignatureEnvelope);
        }
        UnparsedPublicKey::new(&ED25519, key.public_key)
            .verify(manifest_bytes, &signature)
            .map_err(|_| ManifestError::InvalidSignature)?;

        let manifest: ReleaseManifest = serde_json::from_slice(manifest_bytes)
            .map_err(|error| ManifestError::Json(error.to_string()))?;
        manifest.validate()?;
        if manifest.rollout.sequence < key.first_sequence
            || key
                .last_sequence
                .is_some_and(|last| manifest.rollout.sequence > last)
        {
            return Err(ManifestError::KeyOutsideSequence {
                key_id: key.key_id.clone(),
                sequence: manifest.rollout.sequence,
            });
        }
        Ok(VerifiedManifest {
            manifest,
            key_id: key.key_id.clone(),
            manifest_sha256: digest,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifiedManifest {
    pub manifest: ReleaseManifest,
    pub key_id: String,
    pub manifest_sha256: String,
}

#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    #[error("unsupported release manifest schema {0}")]
    Schema(String),
    #[error("invalid manifest field: {0}")]
    InvalidField(String),
    #[error("unsupported update platform: {0}")]
    UnsupportedPlatform(String),
    #[error("invalid release signature envelope")]
    SignatureEnvelope,
    #[error("release manifest signature is invalid")]
    InvalidSignature,
    #[error("release manifest digest does not match the signature envelope")]
    ManifestDigest,
    #[error("unknown release signing key {0}")]
    UnknownKey(String),
    #[error("revoked release signing key {0}")]
    RevokedKey(String),
    #[error("release key {key_id} is not valid for rollout sequence {sequence}")]
    KeyOutsideSequence { key_id: String, sequence: u64 },
    #[error("invalid release keyring")]
    InvalidKeyring,
    #[error("{0} exceeds its allowed size")]
    Size(&'static str),
    #[error("invalid JSON: {0}")]
    Json(String),
    #[error("updater {installed} is older than required version {minimum}")]
    UpdaterTooOld {
        minimum: Version,
        installed: Version,
    },
    #[error("downgrade from {installed} to {candidate} requires explicit approval")]
    DowngradeRefused {
        installed: Version,
        candidate: Version,
    },
    #[error("rollout sequence rollback from {installed} to {candidate} requires explicit approval")]
    SequenceRollback { installed: u64, candidate: u64 },
}

fn validate_hex(name: &str, value: &str, length: usize) -> Result<(), ManifestError> {
    if value.len() != length || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(ManifestError::InvalidField(format!(
            "{name} must contain {length} hexadecimal characters"
        )));
    }
    Ok(())
}

fn validate_artifact(artifact: &ManifestArtifact) -> Result<(), ManifestError> {
    validate_evidence(&ReleaseEvidence {
        name: artifact.name.clone(),
        bytes: artifact.bytes,
        sha256: artifact.sha256.clone(),
    })?;
    let path = Path::new(&artifact.name);
    if artifact.name.is_empty()
        || path.is_absolute()
        || path.components().count() != 1
        || !matches!(path.components().next(), Some(Component::Normal(_)))
    {
        return Err(ManifestError::InvalidField(format!(
            "unsafe artifact name {}",
            artifact.name
        )));
    }
    if artifact.bytes == 0 {
        return Err(ManifestError::InvalidField(format!(
            "artifact {} has zero bytes",
            artifact.name
        )));
    }
    validate_hex("artifact.sha256", &artifact.sha256, 64)?;
    if artifact.target.trim().is_empty() {
        return Err(ManifestError::InvalidField(format!(
            "artifact {} has no target triple",
            artifact.name
        )));
    }
    Ok(())
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

#[cfg(test)]
mod tests {
    use ring::signature::{Ed25519KeyPair, KeyPair};
    use semver::Version;
    use sha2::{Digest, Sha256};

    use super::*;

    const TEST_KEY_ID: &str = "test-key";
    const TEST_SEED: [u8; 32] = [7; 32];

    fn fixture_manifest(sequence: u64) -> ReleaseManifest {
        ReleaseManifest {
            schema: MANIFEST_SCHEMA.to_owned(),
            version: Version::new(2, 0, 0),
            minimum_updater_version: Version::new(1, 0, 0),
            source: BuildSource {
                repository: "benclawbot/Medusa".to_owned(),
                revision: "a".repeat(40),
                rust_toolchain: "1.88.0".to_owned(),
                cargo_lock_sha256: "b".repeat(64),
                desktop_lock_sha256: "c".repeat(64),
            },
            rollout: RolloutPolicy {
                channel: "stable".to_owned(),
                sequence,
                percentage: 100,
            },
            artifacts: vec![ManifestArtifact {
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
        }
    }

    fn signed_fixture(sequence: u64) -> (TrustStore, Vec<u8>, Vec<u8>) {
        let key_pair = Ed25519KeyPair::from_seed_unchecked(&TEST_SEED).expect("test key");
        let manifest = serde_json::to_vec(&fixture_manifest(sequence)).expect("manifest");
        let envelope = ManifestSignature {
            schema: SIGNATURE_SCHEMA.to_owned(),
            key_id: TEST_KEY_ID.to_owned(),
            algorithm: "Ed25519".to_owned(),
            manifest_sha256: hex::encode(Sha256::digest(&manifest)),
            signature: hex::encode(key_pair.sign(&manifest).as_ref()),
        };
        let store = TrustStore::new(vec![TrustedKey {
            key_id: TEST_KEY_ID.to_owned(),
            public_key: key_pair
                .public_key()
                .as_ref()
                .try_into()
                .expect("public key"),
            status: KeyStatus::Active,
            first_sequence: 1,
            last_sequence: None,
        }])
        .expect("trust store");
        (
            store,
            manifest,
            serde_json::to_vec(&envelope).expect("signature envelope"),
        )
    }

    #[test]
    fn verifies_exact_manifest_bytes_before_parsing() {
        let (store, manifest, signature) = signed_fixture(5);
        let verified = store.verify(&manifest, &signature).expect("verified");
        assert_eq!(verified.manifest.rollout.sequence, 5);

        let mut tampered = manifest;
        tampered.push(b' ');
        assert!(matches!(
            store.verify(&tampered, &signature),
            Err(ManifestError::ManifestDigest)
        ));
    }

    #[test]
    fn rejects_unknown_manifest_fields() {
        let mut value = serde_json::to_value(fixture_manifest(1)).expect("manifest value");
        value
            .as_object_mut()
            .expect("manifest object")
            .insert("unexpected".to_owned(), serde_json::Value::Bool(true));
        assert!(serde_json::from_value::<ReleaseManifest>(value).is_err());
    }

    #[test]
    fn rejects_unknown_and_revoked_keys() {
        let (store, manifest, mut signature) = signed_fixture(5);
        let mut envelope: ManifestSignature = serde_json::from_slice(&signature).expect("envelope");
        envelope.key_id = "unknown".to_owned();
        signature = serde_json::to_vec(&envelope).expect("envelope");
        assert!(matches!(
            store.verify(&manifest, &signature),
            Err(ManifestError::UnknownKey(_))
        ));

        let (_, manifest, signature) = signed_fixture(5);
        let key_pair = Ed25519KeyPair::from_seed_unchecked(&TEST_SEED).expect("test key");
        let revoked = TrustStore::new(vec![TrustedKey {
            key_id: TEST_KEY_ID.to_owned(),
            public_key: key_pair
                .public_key()
                .as_ref()
                .try_into()
                .expect("public key"),
            status: KeyStatus::Revoked,
            first_sequence: 1,
            last_sequence: None,
        }])
        .expect("store");
        assert!(matches!(
            revoked.verify(&manifest, &signature),
            Err(ManifestError::RevokedKey(_))
        ));
    }

    #[test]
    fn enforces_key_rotation_window() {
        let (store, manifest, signature) = signed_fixture(5);
        let key = store.keys[0].clone();
        let future = TrustStore::new(vec![TrustedKey {
            first_sequence: 6,
            ..key
        }])
        .expect("store");
        assert!(matches!(
            future.verify(&manifest, &signature),
            Err(ManifestError::KeyOutsideSequence { .. })
        ));
    }

    #[test]
    fn production_trust_store_keeps_independent_primary_and_recovery_authorities() {
        let store = TrustStore::production();
        assert_eq!(store.keys.len(), 3);
        let primary = store
            .keys
            .iter()
            .find(|key| key.key_id == DEFAULT_KEY_ID)
            .expect("primary key");
        let recovery = store
            .keys
            .iter()
            .find(|key| key.key_id == RECOVERY_KEY_ID)
            .expect("recovery key");
        assert_eq!(primary.status, KeyStatus::Active);
        assert_eq!(recovery.status, KeyStatus::Active);
        assert_ne!(primary.public_key, recovery.public_key);
        assert!(
            store
                .keys
                .iter()
                .any(|key| key.status == KeyStatus::Revoked)
        );

        let keyring: serde_json::Value =
            serde_json::from_str(include_str!("../../../release/keys/keyring.json"))
                .expect("release keyring");
        for trusted in &store.keys {
            let declared = keyring["keys"]
                .as_array()
                .expect("key array")
                .iter()
                .find(|key| key["key_id"] == trusted.key_id)
                .unwrap_or_else(|| panic!("missing keyring entry for {}", trusted.key_id));
            assert_eq!(declared["public_key_hex"], hex::encode(trusted.public_key));
            assert_eq!(declared["first_sequence"], trusted.first_sequence);
            assert_eq!(declared["last_sequence"].as_u64(), trusted.last_sequence);
            assert_eq!(
                declared["status"],
                if trusted.status == KeyStatus::Active {
                    "active"
                } else {
                    "revoked"
                }
            );
        }
    }

    #[test]
    fn rejects_wrong_platform_and_traversal() {
        let manifest = fixture_manifest(1);
        assert!(
            manifest
                .select_cli(Platform {
                    os: OperatingSystem::Windows,
                    architecture: Architecture::X86_64,
                })
                .is_err()
        );
        let mut unsafe_manifest = manifest;
        unsafe_manifest.artifacts[0].name = "../medusa".to_owned();
        assert!(unsafe_manifest.validate().is_err());
    }

    #[test]
    fn downgrade_and_sequence_rollback_are_explicit() {
        let manifest = fixture_manifest(4);
        assert!(matches!(
            manifest.validate_install_policy(
                &Version::new(3, 0, 0),
                &Version::new(1, 0, 0),
                Some(5),
                false,
            ),
            Err(ManifestError::DowngradeRefused { .. })
        ));
        manifest
            .validate_install_policy(
                &Version::new(3, 0, 0),
                &Version::new(1, 0, 0),
                Some(5),
                true,
            )
            .expect("explicit downgrade");
    }
}
