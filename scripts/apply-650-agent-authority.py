from pathlib import Path

manifest = Path('crates/medusa-agent/Cargo.toml')
text = manifest.read_text()
needle = 'medusa-core.workspace = true\n'
if needle not in text:
    raise SystemExit('agent manifest anchor missing')
text = text.replace(needle, needle + 'medusa-evidence.workspace = true\n', 1)
manifest.write_text(text)

lib = Path('crates/medusa-agent/src/lib.rs')
text = lib.read_text()
text = text.replace('mod verification;\n', 'mod verification;\nmod verification_authority;\n', 1)
text = text.replace(
    'pub use verification::{VerificationResult, targeted_verification};\n',
    'pub use verification::{VerificationResult, targeted_verification};\npub use verification_authority::{AuthoritativeVerificationResult, authoritative_verification_for_components};\n',
    1,
)
lib.write_text(text)

engine = Path('crates/medusa-agent/src/engine.rs')
text = engine.read_text()
text = text.replace(
    '    verification::targeted_verification_for_paths,\n',
    '    verification_authority::authoritative_verification_for_paths,\n',
    1,
)
count = text.count('targeted_verification_for_paths(')
if count != 1:
    raise SystemExit(f'expected one direct verification call, found {count}')
text = text.replace('targeted_verification_for_paths(', 'authoritative_verification_for_paths(', 1)
engine.write_text(text)

authority = Path('crates/medusa-agent/src/verification_authority.rs')
text = authority.read_text()
text = text.replace(
    '''    let (_, legacy_read) = store
        .read_range(
            &legacy_artifact.id,
            0,
            legacy_artifact.byte_len.max(1).min(legacy_artifact.byte_len),
            "medusa-agent-verification-authority",
        )
        .map_err(evidence_error)?;
''',
    '''    if legacy_artifact.byte_len == 0 {
        return Err(invalid("targeted verification produced no evidence bytes"));
    }
    let (_, legacy_read) = store
        .read_range(
            &legacy_artifact.id,
            0,
            legacy_artifact.byte_len,
            "medusa-agent-verification-authority",
        )
        .map_err(evidence_error)?;
''',
    1,
)
old = '''            let (_, read) = store
                .read_page(&metadata.id, 0, "medusa-agent-artifact-validator")
                .map_err(evidence_error)?;
            let record = EvidenceRecord::new(
                EvidenceKind::Observation,
                format!("semantic artifact validation for {}", component.path),
                repository_fingerprint,
                commit,
                "medusa-agent-artifact-validator",
                if result.passed {
                    VerificationStatus::Verified
                } else {
                    VerificationStatus::Rejected
                },
                vec![EvidenceSource::ArtifactRange {
                    artifact_id: metadata.id.clone(),
                    read_receipt_id: read.id.clone(),
                    offset: read.offset,
                    length: read.length,
                    content_hash: read.content_hash.clone(),
                }],
            )
            .map_err(evidence_error)?;
            semantic_ids.push(record.id.clone());
            semantic_artifacts.push(metadata.id.clone());
            artifacts.insert(metadata.id.clone(), metadata);
            reads.push(read);
            records.push(record);
'''
new = '''            semantic_artifacts.push(metadata.id.clone());
            artifacts.insert(metadata.id.clone(), metadata.clone());
            if metadata.byte_len > 0 {
                let (_, read) = store
                    .read_page(&metadata.id, 0, "medusa-agent-artifact-validator")
                    .map_err(evidence_error)?;
                let record = EvidenceRecord::new(
                    EvidenceKind::Observation,
                    format!("semantic artifact validation for {}", component.path),
                    repository_fingerprint,
                    commit,
                    "medusa-agent-artifact-validator",
                    if result.passed {
                        VerificationStatus::Verified
                    } else {
                        VerificationStatus::Rejected
                    },
                    vec![EvidenceSource::ArtifactRange {
                        artifact_id: metadata.id.clone(),
                        read_receipt_id: read.id.clone(),
                        offset: read.offset,
                        length: read.length,
                        content_hash: read.content_hash.clone(),
                    }],
                )
                .map_err(evidence_error)?;
                semantic_ids.push(record.id.clone());
                reads.push(read);
                records.push(record);
            }
'''
if old not in text:
    raise SystemExit('semantic artifact block missing')
text = text.replace(old, new, 1)
authority.write_text(text)

evidence = Path('crates/medusa-evidence/src/evidence.rs')
text = evidence.read_text()
text = text.replace(
    '    SCHEMA_VERSION, fingerprint,\n',
    '    SCHEMA_VERSION, fingerprint,\n    artifact::{validate_metadata, validate_read},\n',
    1,
)
anchor = '''        let records = unique_map(&self.records, |value| value.id.clone(), "evidence")?;
        for command in &self.commands {
'''
replacement = '''        let records = unique_map(&self.records, |value| value.id.clone(), "evidence")?;
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
'''
if anchor not in text:
    raise SystemExit('evidence validation anchor missing')
text = text.replace(anchor, replacement, 1)
evidence.write_text(text)
