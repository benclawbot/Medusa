//! One-way, fail-closed migration adapters for the pre-authority learning stores.

use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

use medusa_context::refinement::{
    EvidenceKind, EvidenceRef, ProposerMetadata, RefinementArtifactKind, RefinementContent,
    RefinementProposal, RefinementRisk, RefinementScope,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::{
    refinement_authority::{RefinementAuthorityError, RefinementAuthorityStore},
    refinement_persistence::{current_unix_ms, quarantine_bytes},
};
use medusa_core::learning_policy::LearningPrivacyPolicy;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MigrationDisposition {
    Imported,
    CompatibilityOnly,
    Quarantined,
    AlreadyImported,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct MigrationReceipt {
    pub source: String,
    pub source_record_id: String,
    pub source_fingerprint: String,
    pub canonical_proposal_id: Option<String>,
    pub canonical_version: Option<u64>,
    pub disposition: MigrationDisposition,
    pub redacted: bool,
    pub reason: String,
    pub recorded_at_unix_ms: i64,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MigrationReport {
    pub receipts: Vec<MigrationReceipt>,
}

pub struct RefinementMigrator;

impl RefinementMigrator {
    pub fn run(
        repo: &Path,
        store: &mut RefinementAuthorityStore,
    ) -> Result<MigrationReport, RefinementAuthorityError> {
        let mut report = MigrationReport::default();
        import_legacy_privacy(repo, store)?;
        let mut sources = legacy_sources(repo);
        sources.sort();
        sources.dedup();
        for source in sources {
            if !source.is_file() {
                continue;
            }
            migrate_source(repo, store, &source, &mut report)?;
        }
        append_receipts(store.root(), &report.receipts)?;
        Ok(report)
    }
}

fn import_legacy_privacy(
    repo: &Path,
    store: &RefinementAuthorityStore,
) -> Result<(), RefinementAuthorityError> {
    if store.privacy_revision()? > 0 {
        return Ok(());
    }
    let path = repo.join(".medusa/learning-review/state.json");
    let Ok(bytes) = fs::read(path) else {
        return Ok(());
    };
    let Ok(document) = serde_json::from_slice::<Value>(&bytes) else {
        return Ok(());
    };
    let Some(privacy) = document.get("privacy") else {
        return Ok(());
    };
    let privacy: LearningPrivacyPolicy = serde_json::from_value(privacy.clone())?;
    store.initialize_privacy(privacy)
}

fn migrate_source(
    repo: &Path,
    store: &mut RefinementAuthorityStore,
    source: &Path,
    report: &mut MigrationReport,
) -> Result<(), RefinementAuthorityError> {
    let bytes = fs::read(source)?;
    let fingerprint = digest_bytes(&bytes);
    let source_label = source
        .strip_prefix(repo)
        .unwrap_or(source)
        .to_string_lossy()
        .replace('\\', "/");
    let default_scope = default_scope_for_source(&source_label);
    let values = match serde_json::from_slice::<Value>(&bytes) {
        Ok(value) => values_for_source(&source_label, value),
        Err(error) => {
            let quarantined = quarantine_bytes(store.root(), "legacy-json", &bytes)?;
            report.receipts.push(MigrationReceipt {
                source: source_label,
                source_record_id: fingerprint.clone(),
                source_fingerprint: fingerprint,
                canonical_proposal_id: None,
                canonical_version: None,
                disposition: MigrationDisposition::Quarantined,
                redacted: false,
                reason: format!(
                    "invalid legacy JSON quarantined at {}: {error}",
                    quarantined.display()
                ),
                recorded_at_unix_ms: current_unix_ms(),
            });
            return Ok(());
        }
    };
    for (record_id, value, source_scope) in values {
        migrate_record(
            store,
            &source_label,
            &fingerprint,
            &record_id,
            value,
            source_scope.or(default_scope),
            report,
        )?;
    }
    Ok(())
}

fn migrate_record(
    store: &mut RefinementAuthorityStore,
    source: &str,
    source_fingerprint: &str,
    source_record_id: &str,
    value: Value,
    source_scope: Option<RefinementScope>,
    report: &mut MigrationReport,
) -> Result<(), RefinementAuthorityError> {
    let canonical_id = format!(
        "legacy-{}",
        digest_text(&format!("{source}:{source_record_id}"))
    );
    let version = value
        .get("version")
        .and_then(Value::as_u64)
        .or_else(|| value.get("revision").and_then(Value::as_u64))
        .filter(|version| *version > 0)
        .unwrap_or(1);
    let Some(scope) = source_scope.or_else(|| scope_from_value(&value)) else {
        report.receipts.push(quarantined_receipt(
            source,
            source_record_id,
            source_fingerprint,
            "legacy record has no verifiable supported scope",
        ));
        return Ok(());
    };
    let Some((artifact_kind, key, body)) = content_from_value(&value, source_record_id) else {
        report.receipts.push(quarantined_receipt(
            source,
            source_record_id,
            source_fingerprint,
            "legacy record has no non-empty generalizable content",
        ));
        return Ok(());
    };
    let evidence_text = evidence_text(&value, source_fingerprint);
    let proposal = RefinementProposal {
        id: canonical_id.clone(),
        version,
        artifact_kind,
        scope,
        evidence: vec![EvidenceRef {
            id: format!("legacy-evidence-{source_fingerprint}"),
            kind: EvidenceKind::ToolEvent,
            trajectory_id: format!("migration:{source_fingerprint}"),
            start_sequence: 1,
            end_sequence: 1,
        }],
        before: None,
        after: match artifact_kind {
            RefinementArtifactKind::Memory => RefinementContent::Memory { key, value: body },
            RefinementArtifactKind::RepositoryConvention => {
                RefinementContent::RepositoryConvention { key, value: body }
            }
            RefinementArtifactKind::WorkflowMetadata => RefinementContent::WorkflowMetadata {
                name: key,
                summary: body,
            },
            RefinementArtifactKind::TeamRoleMetadata => RefinementContent::TeamRoleMetadata {
                name: key,
                guidance: body,
            },
            RefinementArtifactKind::PromptGuidance => RefinementContent::PromptGuidance {
                key,
                guidance: body,
            },
        },
        rationale: format!("Migrated from {source}; preserved legacy evidence: {evidence_text}"),
        expected_outcome:
            "legacy behavior remains a reviewed candidate until canonical validation and approval"
                .into(),
        proposer: ProposerMetadata {
            model: "legacy-migrator".into(),
            route: "one-way-compatibility-import".into(),
            version: "1".into(),
        },
        risk: RefinementRisk::Low,
    };
    let existing = store
        .snapshot()?
        .records
        .iter()
        .any(|record| record.proposal_id == canonical_id && record.version == version);
    let status = value
        .get("state")
        .or_else(|| value.get("status"))
        .and_then(Value::as_str)
        .unwrap_or("proposed")
        .to_ascii_lowercase();
    let compatibility_only = matches!(
        status.as_str(),
        "active" | "adopted" | "approved" | "validated" | "probation" | "installed"
    );
    let disposition = if existing {
        MigrationDisposition::AlreadyImported
    } else {
        let revision = store.snapshot()?.revision;
        if let Err(error) = store.propose(proposal, revision) {
            report.receipts.push(quarantined_receipt(
                source,
                source_record_id,
                source_fingerprint,
                &format!("canonical import rejected and was quarantined: {error}"),
            ));
            return Ok(());
        }
        if compatibility_only {
            MigrationDisposition::CompatibilityOnly
        } else {
            MigrationDisposition::Imported
        }
    };
    report.receipts.push(MigrationReceipt {
        source: source.into(),
        source_record_id: source_record_id.into(),
        source_fingerprint: source_fingerprint.into(),
        canonical_proposal_id: Some(canonical_id),
        canonical_version: Some(version),
        disposition,
        redacted: false,
        reason: if compatibility_only {
            "legacy active/adopted state was imported as non-active compatibility data; approval was not inferred".into()
        } else {
            "legacy candidate imported without inferred validation, evaluation, or approval".into()
        },
        recorded_at_unix_ms: current_unix_ms(),
    });
    Ok(())
}

fn values_for_source(source: &str, value: Value) -> Vec<(String, Value, Option<RefinementScope>)> {
    if source.ends_with("learning-review/state.json") {
        return value
            .get("items")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .enumerate()
                    .map(|(index, item)| {
                        (
                            item.get("id")
                                .and_then(Value::as_str)
                                .map_or_else(|| index.to_string(), str::to_owned),
                            item.clone(),
                            None,
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
    }
    if source.ends_with("engineering/improvements.json") {
        return value
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .enumerate()
                    .map(|(index, item)| {
                        (
                            item.get("id")
                                .and_then(Value::as_str)
                                .map_or_else(|| index.to_string(), str::to_owned),
                            item.clone(),
                            Some(RefinementScope::Repository),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
    }
    if value.get("entries").and_then(Value::as_object).is_some() {
        return value["entries"]
            .as_object()
            .into_iter()
            .flat_map(|entries| {
                entries
                    .iter()
                    .map(|(id, item)| (id.clone(), item.clone(), scope_from_value(item)))
            })
            .collect();
    }
    let record_id = value
        .get("id")
        .or_else(|| value.get("name"))
        .and_then(Value::as_str)
        .map_or_else(|| "record".to_owned(), str::to_owned);
    vec![(record_id, value, None)]
}

fn content_from_value(
    value: &Value,
    fallback_id: &str,
) -> Option<(RefinementArtifactKind, String, String)> {
    let key = value
        .get("key")
        .or_else(|| value.get("name"))
        .or_else(|| value.get("title"))
        .and_then(Value::as_str)
        .filter(|text| !text.trim().is_empty())
        .unwrap_or(fallback_id)
        .to_owned();
    let body = [
        "generalized_rule",
        "generalizedRule",
        "proposed_change",
        "proposedChange",
        "proposed_solution",
        "proposedSolution",
        "summary",
        "guidance",
        "value",
        "description",
    ]
    .iter()
    .find_map(|field| value.get(*field).and_then(Value::as_str))
    .map(str::trim)
    .filter(|text| !text.is_empty())
    .map(str::to_owned)
    .or_else(|| {
        value
            .get("procedure")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join("; ")
            })
            .filter(|text| !text.trim().is_empty())
    })
    .or_else(|| {
        value
            .get("skill_file")
            .or_else(|| value.get("proposed_install_path"))
            .and_then(Value::as_str)
            .map(|path| format!("legacy skill artifact at {path}"))
    })
    .or_else(|| {
        value.get("responses").and_then(Value::as_object).map(|_| {
            format!(
                "legacy skill {} version {}",
                value
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or(fallback_id),
                value
                    .get("version")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown")
            )
        })
    })?;
    let artifact_kind = if value.get("proposed_change").is_some()
        || value.get("proposedChange").is_some()
        || value.get("proposed_solution").is_some()
        || value.get("proposedSolution").is_some()
    {
        RefinementArtifactKind::RepositoryConvention
    } else if value.get("guidance").is_some() {
        RefinementArtifactKind::TeamRoleMetadata
    } else {
        RefinementArtifactKind::RepositoryConvention
    };
    Some((artifact_kind, key, body))
}

fn scope_from_value(value: &Value) -> Option<RefinementScope> {
    match value
        .get("scope")
        .and_then(Value::as_str)
        .map(|scope| scope.to_ascii_lowercase())
        .as_deref()
    {
        Some("repository") | Some("workspace") => Some(RefinementScope::Repository),
        Some("user") | Some("global") => Some(RefinementScope::User),
        Some("session") => Some(RefinementScope::Session),
        _ => None,
    }
}

fn default_scope_for_source(source: &str) -> Option<RefinementScope> {
    if source.contains("user/learnings.json") {
        Some(RefinementScope::User)
    } else if source.contains(".medusa/") {
        Some(RefinementScope::Repository)
    } else {
        None
    }
}

fn evidence_text(value: &Value, source_fingerprint: &str) -> String {
    let mut evidence = Vec::new();
    for field in [
        "evidence",
        "evidence_digests",
        "source_signal_ids",
        "source_sessions",
    ] {
        if let Some(items) = value.get(field).and_then(Value::as_array) {
            evidence.extend(items.iter().filter_map(Value::as_str).map(str::to_owned));
        }
    }
    if evidence.is_empty() {
        format!("legacy-source-digest:{source_fingerprint}")
    } else {
        evidence.sort();
        evidence.dedup();
        evidence.join(",")
    }
}

fn legacy_sources(repo: &Path) -> Vec<PathBuf> {
    let medusa = repo.join(".medusa");
    let mut sources = vec![
        medusa.join("learning-review/state.json"),
        medusa.join("learnings.json"),
        medusa.join("user/learnings.json"),
        medusa.join("engineering/improvements.json"),
    ];
    for directory in [
        medusa.join("memory/lessons"),
        medusa.join("learning/proposals"),
        medusa.join("learning/skill-proposals"),
        medusa.join("improvements/skills"),
        medusa.join("improvements/history"),
    ] {
        collect_json_files(&directory, &mut sources);
    }
    sources
}

fn collect_json_files(directory: &Path, output: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_json_files(&path, output);
        } else if path
            .extension()
            .is_some_and(|extension| extension == "json")
        {
            output.push(path);
        }
    }
}

fn append_receipts(
    root: &Path,
    receipts: &[MigrationReceipt],
) -> Result<(), RefinementAuthorityError> {
    if receipts.is_empty() {
        return Ok(());
    }
    let path = root.join("migrations.jsonl");
    fs::create_dir_all(root)?;
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    for receipt in receipts {
        serde_json::to_writer(&mut file, receipt)?;
        file.write_all(b"\n")?;
    }
    file.sync_all()?;
    Ok(())
}

fn quarantined_receipt(
    source: &str,
    source_record_id: &str,
    source_fingerprint: &str,
    reason: &str,
) -> MigrationReceipt {
    MigrationReceipt {
        source: source.into(),
        source_record_id: source_record_id.into(),
        source_fingerprint: source_fingerprint.into(),
        canonical_proposal_id: None,
        canonical_version: None,
        disposition: MigrationDisposition::Quarantined,
        redacted: false,
        reason: reason.into(),
        recorded_at_unix_ms: current_unix_ms(),
    }
}

fn digest_bytes(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn digest_text(value: &str) -> String {
    digest_bytes(value.as_bytes())
}
