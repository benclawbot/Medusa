use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use ulid::Ulid;

use crate::{
    ArtifactId, ArtifactMetadata, ArtifactReadReceipt, CommandReceipt, EvidenceError, Result,
    SCHEMA_VERSION,
    artifact::{validate_metadata, validate_read},
    fingerprint,
};

#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct EvidenceId(pub String);

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceKind {
    Observation,
    Inference,
    Claim,
    Decision,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationStatus {
    Unverified,
    Verified,
    Rejected,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EvidenceSource {
    RepositoryRange {
        repository_fingerprint: String,
        commit: String,
        path: String,
        start_line: u32,
        end_line: u32,
        content_hash: String,
    },
    CommandReceipt {
        receipt_id: String,
    },
    ArtifactRange {
        artifact_id: ArtifactId,
        read_receipt_id: String,
        offset: u64,
        length: u64,
        content_hash: String,
    },
    Evidence {
        evidence_id: EvidenceId,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct EvidenceRecord {
    pub schema_version: u16,
    pub id: EvidenceId,
    pub kind: EvidenceKind,
    pub statement: String,
    pub repository_fingerprint: String,
    pub commit: String,
    pub producer: String,
    pub created_at: OffsetDateTime,
    pub status: VerificationStatus,
    pub sources: Vec<EvidenceSource>,
    pub contradictions: Vec<EvidenceId>,
    pub fingerprint: String,
}

impl EvidenceRecord {
    pub fn new(
        kind: EvidenceKind,
        statement: impl Into<String>,
        repository_fingerprint: impl Into<String>,
        commit: impl Into<String>,
        producer: impl Into<String>,
        status: VerificationStatus,
        sources: Vec<EvidenceSource>,
    ) -> Result<Self> {
        let mut record = Self {
            schema_version: SCHEMA_VERSION,
            id: EvidenceId(format!("evidence-{}", Ulid::new())),
            kind,
            statement: statement.into(),
            repository_fingerprint: repository_fingerprint.into(),
            commit: commit.into(),
            producer: producer.into(),
            created_at: OffsetDateTime::now_utc(),
            status,
            sources,
            contradictions: Vec::new(),
            fingerprint: String::new(),
        };
        record.refresh();
        record.validate_shape()?;
        Ok(record)
    }

    pub fn refresh(&mut self) {
        self.fingerprint = record_fingerprint(self);
    }

    pub fn validate_shape(&self) -> Result<()> {
        if self.schema_version != SCHEMA_VERSION
            || self.id.0.trim().is_empty()
            || self.statement.trim().is_empty()
            || self.repository_fingerprint.trim().is_empty()
            || self.commit.trim().is_empty()
            || self.producer.trim().is_empty()
            || self.fingerprint != record_fingerprint(self)
        {
            return Err(EvidenceError::Validation(
                "typed evidence record is incomplete or corrupted".to_owned(),
            ));
        }
        if matches!(self.kind, EvidenceKind::Claim | EvidenceKind::Decision)
            && self.status == VerificationStatus::Verified
            && self.sources.is_empty()
        {
            return Err(EvidenceError::Validation(
                "verified claims and decisions require sources".to_owned(),
            ));
        }
        if self.status == VerificationStatus::Verified && !self.contradictions.is_empty() {
            return Err(EvidenceError::Validation(
                "verified evidence cannot retain unresolved contradictions".to_owned(),
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct EvidenceBundle {
    pub schema_version: u16,
    pub repository_fingerprint: String,
    pub commit: String,
    pub artifacts: Vec<ArtifactMetadata>,
    pub reads: Vec<ArtifactReadReceipt>,
    pub commands: Vec<CommandReceipt>,
    pub records: Vec<EvidenceRecord>,
    pub fingerprint: String,
}

impl EvidenceBundle {
    pub fn new(repository_fingerprint: impl Into<String>, commit: impl Into<String>) -> Self {
        let mut bundle = Self {
            schema_version: SCHEMA_VERSION,
            repository_fingerprint: repository_fingerprint.into(),
            commit: commit.into(),
            artifacts: Vec::new(),
            reads: Vec::new(),
            commands: Vec::new(),
            records: Vec::new(),
            fingerprint: String::new(),
        };
        bundle.refresh();
        bundle
    }

    pub fn refresh(&mut self) {
        self.artifacts.sort_by(|left, right| left.id.cmp(&right.id));
        self.reads.sort_by(|left, right| left.id.cmp(&right.id));
        self.commands.sort_by(|left, right| left.id.cmp(&right.id));
        self.records.sort_by(|left, right| left.id.cmp(&right.id));
        self.fingerprint = bundle_fingerprint(self);
    }

    pub fn validate(&self) -> Result<()> {
        if self.schema_version != SCHEMA_VERSION
            || self.repository_fingerprint.trim().is_empty()
            || self.commit.trim().is_empty()
            || self.fingerprint != bundle_fingerprint(self)
        {
            return Err(EvidenceError::Validation(
                "evidence bundle is incomplete or corrupted".to_owned(),
            ));
        }
        let artifacts = unique_map(&self.artifacts, |value| value.id.clone(), "artifact")?;
        let reads = unique_map(&self.reads, |value| value.id.clone(), "read receipt")?;
        let commands = unique_map(&self.commands, |value| value.id.clone(), "command receipt")?;
        let records = unique_map(&self.records, |value| value.id.clone(), "evidence")?;
        for artifact in &self.artifacts {
            validate_metadata(artifact)?;
        }
        for read in &self.reads {
            validate_read(read)?;
            let artifact = artifacts.get(&read.artifact_id).ok_or_else(|| {
                EvidenceError::Validation("read receipt references missing artifact".to_owned())
            })?;
            if read.offset.saturating_add(read.length) > artifact.byte_len {
                return Err(EvidenceError::Validation(
                    "read receipt exceeds artifact bounds".to_owned(),
                ));
            }
        }
        for command in &self.commands {
            command.validate()?;
            if !artifacts.contains_key(&command.stdout_artifact)
                || !artifacts.contains_key(&command.stderr_artifact)
            {
                return Err(EvidenceError::Validation(
                    "command receipt references missing output artifacts".to_owned(),
                ));
            }
        }
        for record in &self.records {
            record.validate_shape()?;
            if record.repository_fingerprint != self.repository_fingerprint
                || record.commit != self.commit
            {
                return Err(EvidenceError::Validation(
                    "evidence record is stale for repository or commit".to_owned(),
                ));
            }
            for source in &record.sources {
                match source {
                    EvidenceSource::RepositoryRange {
                        repository_fingerprint,
                        commit,
                        path,
                        start_line,
                        end_line,
                        content_hash,
                    } => {
                        if repository_fingerprint != &self.repository_fingerprint
                            || commit != &self.commit
                            || path.trim().is_empty()
                            || *start_line == 0
                            || end_line < start_line
                            || content_hash.trim().is_empty()
                        {
                            return Err(EvidenceError::Validation(
                                "stale or invalid repository source".to_owned(),
                            ));
                        }
                    }
                    EvidenceSource::CommandReceipt { receipt_id } => {
                        let receipt = commands.get(receipt_id).ok_or_else(|| {
                            EvidenceError::Validation(
                                "evidence references missing command receipt".to_owned(),
                            )
                        })?;
                        if record.status == VerificationStatus::Verified && !receipt.passed {
                            return Err(EvidenceError::Validation(
                                "failed command cannot verify a conclusion".to_owned(),
                            ));
                        }
                    }
                    EvidenceSource::ArtifactRange {
                        artifact_id,
                        read_receipt_id,
                        offset,
                        length,
                        content_hash,
                    } => {
                        let artifact = artifacts.get(artifact_id).ok_or_else(|| {
                            EvidenceError::Validation("missing artifact source".to_owned())
                        })?;
                        let read = reads.get(read_receipt_id).ok_or_else(|| {
                            EvidenceError::Validation(
                                "artifact conclusion lacks a durable read receipt".to_owned(),
                            )
                        })?;
                        if &read.artifact_id != artifact_id
                            || read.offset != *offset
                            || read.length != *length
                            || &read.content_hash != content_hash
                            || offset.saturating_add(*length) > artifact.byte_len
                        {
                            return Err(EvidenceError::Validation(
                                "artifact source does not match actual read".to_owned(),
                            ));
                        }
                    }
                    EvidenceSource::Evidence { evidence_id } => {
                        let dependency = records.get(evidence_id).ok_or_else(|| {
                            EvidenceError::Validation("missing evidence dependency".to_owned())
                        })?;
                        if dependency.status != VerificationStatus::Verified {
                            return Err(EvidenceError::Validation(
                                "unverified evidence cannot satisfy a dependency".to_owned(),
                            ));
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct EvidenceDependency {
    pub bundle_fingerprint: String,
    pub decision_ids: Vec<EvidenceId>,
    pub fingerprint: String,
}

impl EvidenceDependency {
    pub fn from_bundle(bundle: &EvidenceBundle, decision_ids: Vec<EvidenceId>) -> Result<Self> {
        bundle.validate()?;
        if decision_ids.is_empty() {
            return Err(EvidenceError::Validation(
                "scheduler dependency requires decision evidence".to_owned(),
            ));
        }
        for id in &decision_ids {
            let record = bundle
                .records
                .iter()
                .find(|record| &record.id == id)
                .ok_or_else(|| EvidenceError::Validation("missing decision evidence".to_owned()))?;
            if record.kind != EvidenceKind::Decision
                || record.status != VerificationStatus::Verified
            {
                return Err(EvidenceError::Validation(
                    "scheduler dependency requires verified decisions".to_owned(),
                ));
            }
        }
        let mut dependency = Self {
            bundle_fingerprint: bundle.fingerprint.clone(),
            decision_ids,
            fingerprint: String::new(),
        };
        dependency.fingerprint = dependency_fingerprint(&dependency);
        Ok(dependency)
    }

    pub fn validate(&self, bundle: &EvidenceBundle) -> Result<()> {
        if self != &Self::from_bundle(bundle, self.decision_ids.clone())? {
            return Err(EvidenceError::Validation(
                "scheduler evidence dependency is stale or corrupted".to_owned(),
            ));
        }
        Ok(())
    }
}

fn unique_map<'a, T, K: Ord + Clone>(
    values: &'a [T],
    key: impl Fn(&T) -> K,
    label: &str,
) -> Result<BTreeMap<K, &'a T>> {
    let mut map = BTreeMap::new();
    for value in values {
        if map.insert(key(value), value).is_some() {
            return Err(EvidenceError::Validation(format!(
                "duplicate {label} in evidence bundle"
            )));
        }
    }
    Ok(map)
}

fn record_fingerprint(record: &EvidenceRecord) -> String {
    fingerprint(&(
        record.schema_version,
        &record.id,
        record.kind,
        &record.statement,
        &record.repository_fingerprint,
        &record.commit,
        &record.producer,
        record.created_at,
        record.status,
        &record.sources,
        &record.contradictions,
    ))
}

fn bundle_fingerprint(bundle: &EvidenceBundle) -> String {
    fingerprint(&(
        bundle.schema_version,
        &bundle.repository_fingerprint,
        &bundle.commit,
        &bundle.artifacts,
        &bundle.reads,
        &bundle.commands,
        &bundle.records,
    ))
}

fn dependency_fingerprint(dependency: &EvidenceDependency) -> String {
    fingerprint(&(&dependency.bundle_fingerprint, &dependency.decision_ids))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unread_artifact_cannot_verify_a_claim() {
        let mut bundle = EvidenceBundle::new("repo", "commit");
        let artifact = ArtifactMetadata {
            schema_version: SCHEMA_VERSION,
            id: ArtifactId(format!("artifact-{}", "a".repeat(64))),
            media_type: "text/plain".to_owned(),
            byte_len: 3,
            sha256: "a".repeat(64),
            producer: "test".to_owned(),
            created_at: OffsetDateTime::now_utc(),
            binary: false,
            page_size: crate::DEFAULT_PAGE_SIZE,
            page_count: 1,
            fingerprint: "metadata".to_owned(),
        };
        bundle.artifacts.push(artifact.clone());
        bundle.records.push(
            EvidenceRecord::new(
                EvidenceKind::Claim,
                "artifact proves result",
                "repo",
                "commit",
                "reviewer",
                VerificationStatus::Verified,
                vec![EvidenceSource::ArtifactRange {
                    artifact_id: artifact.id,
                    read_receipt_id: "missing".to_owned(),
                    offset: 0,
                    length: 3,
                    content_hash: "hash".to_owned(),
                }],
            )
            .unwrap(),
        );
        bundle.refresh();
        assert!(bundle.validate().is_err());
    }

    #[test]
    fn unverified_decision_cannot_satisfy_scheduler() {
        let mut bundle = EvidenceBundle::new("repo", "commit");
        let decision = EvidenceRecord::new(
            EvidenceKind::Decision,
            "accept",
            "repo",
            "commit",
            "reviewer",
            VerificationStatus::Unverified,
            Vec::new(),
        )
        .unwrap();
        bundle.records.push(decision.clone());
        bundle.refresh();
        assert!(EvidenceDependency::from_bundle(&bundle, vec![decision.id]).is_err());
    }
}
