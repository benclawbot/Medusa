use std::{collections::BTreeMap, path::Path};

use serde::{Deserialize, Deserializer, Serialize};

use crate::{
    ChangeKind, EvidenceBundle, EvidenceError, EvidenceSource, Result, SCHEMA_VERSION,
    VerificationCheckKind, VerificationCheckReceipt, VerificationPlan, VerificationStatus,
    verification::VerificationReceipt as InnerVerificationReceipt,
};

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct VerificationReceipt {
    pub schema_version: u16,
    pub plan: VerificationPlan,
    pub checks: Vec<VerificationCheckReceipt>,
    pub evidence: EvidenceBundle,
    pub passed: bool,
    pub coverage: Vec<String>,
    pub fingerprint: String,
}

#[derive(Deserialize)]
struct VerificationReceiptData {
    schema_version: u16,
    plan: VerificationPlan,
    checks: Vec<VerificationCheckReceipt>,
    evidence: EvidenceBundle,
    passed: bool,
    coverage: Vec<String>,
    fingerprint: String,
}

impl VerificationReceipt {
    pub fn new(
        plan: VerificationPlan,
        checks: Vec<VerificationCheckReceipt>,
        evidence: EvidenceBundle,
    ) -> Result<Self> {
        let receipt = Self::from_inner(InnerVerificationReceipt::new(plan, checks, evidence)?);
        receipt.validate()?;
        Ok(receipt)
    }

    pub fn validate(&self) -> Result<()> {
        self.as_inner().validate()?;
        validate_authority(self)
    }

    #[must_use]
    pub fn summary_lines(&self) -> Vec<String> {
        self.as_inner().summary_lines()
    }

    fn from_inner(receipt: InnerVerificationReceipt) -> Self {
        Self {
            schema_version: receipt.schema_version,
            plan: receipt.plan,
            checks: receipt.checks,
            evidence: receipt.evidence,
            passed: receipt.passed,
            coverage: receipt.coverage,
            fingerprint: receipt.fingerprint,
        }
    }

    fn as_inner(&self) -> InnerVerificationReceipt {
        InnerVerificationReceipt {
            schema_version: self.schema_version,
            plan: self.plan.clone(),
            checks: self.checks.clone(),
            evidence: self.evidence.clone(),
            passed: self.passed,
            coverage: self.coverage.clone(),
            fingerprint: self.fingerprint.clone(),
        }
    }
}

impl<'de> Deserialize<'de> for VerificationReceipt {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let data = VerificationReceiptData::deserialize(deserializer)?;
        let receipt = Self {
            schema_version: data.schema_version,
            plan: data.plan,
            checks: data.checks,
            evidence: data.evidence,
            passed: data.passed,
            coverage: data.coverage,
            fingerprint: data.fingerprint,
        };
        receipt.validate().map_err(serde::de::Error::custom)?;
        Ok(receipt)
    }
}

fn validate_authority(receipt: &VerificationReceipt) -> Result<()> {
    if receipt.schema_version != SCHEMA_VERSION {
        return Err(EvidenceError::Validation(
            "verification receipt schema is unsupported".to_owned(),
        ));
    }
    let planned = receipt
        .plan
        .checks
        .iter()
        .map(|check| (check.id.as_str(), check))
        .collect::<BTreeMap<_, _>>();
    let evidence_records = receipt
        .evidence
        .records
        .iter()
        .map(|record| (record.id.clone(), record))
        .collect::<BTreeMap<_, _>>();
    let evidence_commands = receipt
        .evidence
        .commands
        .iter()
        .map(|command| (command.id.as_str(), command))
        .collect::<BTreeMap<_, _>>();
    let evidence_artifacts = receipt
        .evidence
        .artifacts
        .iter()
        .map(|artifact| (artifact.id.clone(), artifact))
        .collect::<BTreeMap<_, _>>();

    for check_receipt in &receipt.checks {
        let check = planned
            .get(check_receipt.check_id.as_str())
            .ok_or_else(|| {
                EvidenceError::Validation("receipt contains unplanned check".to_owned())
            })?;
        let records = check_receipt
            .evidence_ids
            .iter()
            .map(|id| {
                evidence_records.get(id).copied().ok_or_else(|| {
                    EvidenceError::Validation(
                        "check references missing evidence or artifacts".to_owned(),
                    )
                })
            })
            .collect::<Result<Vec<_>>>()?;

        if check_receipt
            .artifact_ids
            .iter()
            .any(|id| !evidence_artifacts.contains_key(id))
        {
            return Err(EvidenceError::Validation(
                "check references missing evidence or artifacts".to_owned(),
            ));
        }
        if check_receipt.passed
            && (records.is_empty()
                || records
                    .iter()
                    .any(|record| record.status != VerificationStatus::Verified))
        {
            return Err(EvidenceError::Validation(
                "passed check requires verified evidence".to_owned(),
            ));
        }

        match (&check.program, &check_receipt.command) {
            (Some(_), None) if check_receipt.passed => {
                return Err(EvidenceError::Validation(
                    "passed command check requires command receipt".to_owned(),
                ));
            }
            (None, Some(_)) => {
                return Err(EvidenceError::Validation(
                    "behavior check cannot contain command receipt".to_owned(),
                ));
            }
            (_, Some(command)) => {
                let bundled = evidence_commands.get(command.id.as_str()).ok_or_else(|| {
                    EvidenceError::Validation(
                        "command receipt is absent from evidence bundle".to_owned(),
                    )
                })?;
                if *bundled != command
                    || command.check_id != check.id
                    || command.command_hash != check.input_fingerprint
                    || command.passed != check_receipt.passed
                    || !check_receipt
                        .artifact_ids
                        .contains(&command.stdout_artifact)
                    || !check_receipt
                        .artifact_ids
                        .contains(&command.stderr_artifact)
                    || !records.iter().any(|record| {
                        record.sources.iter().any(|source| {
                            matches!(
                                source,
                                EvidenceSource::CommandReceipt { receipt_id }
                                    if receipt_id == &command.id
                            )
                        })
                    })
                {
                    return Err(EvidenceError::Validation(
                        "command receipt does not match plan or evidence".to_owned(),
                    ));
                }
            }
            _ => {}
        }

        if check_receipt.passed
            && requires_durable_artifact_evidence(check.kind, &receipt.plan)
            && (check_receipt.artifact_ids.is_empty()
                || !records.iter().any(|record| {
                    record.sources.iter().any(|source| {
                        matches!(
                            source,
                            EvidenceSource::ArtifactRange { artifact_id, .. }
                                if check_receipt.artifact_ids.contains(artifact_id)
                        )
                    })
                }))
        {
            return Err(EvidenceError::Validation(
                "passed behavior check requires durable artifact evidence".to_owned(),
            ));
        }
    }
    Ok(())
}

fn requires_durable_artifact_evidence(
    kind: VerificationCheckKind,
    plan: &VerificationPlan,
) -> bool {
    match kind {
        VerificationCheckKind::BrowserBehavior | VerificationCheckKind::Accessibility => true,
        VerificationCheckKind::ArtifactSemantic => plan.components.iter().any(|component| {
            component.kind != ChangeKind::Deleted
                && (component.generated
                    || Path::new(&component.path)
                        .extension()
                        .and_then(|extension| extension.to_str())
                        .is_some_and(|extension| {
                            matches!(
                                extension.to_ascii_lowercase().as_str(),
                                "html"
                                    | "json"
                                    | "png"
                                    | "pdf"
                                    | "zip"
                                    | "jar"
                                    | "docx"
                                    | "xlsx"
                                    | "pptx"
                            )
                        }))
        }),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::{
        ChangedComponent, EvidenceKind, EvidenceRecord, VerificationCheckReceipt,
        VerificationPlanner,
    };

    fn verified_observation(statement: &str) -> EvidenceRecord {
        EvidenceRecord::new(
            EvidenceKind::Observation,
            statement,
            "repo",
            "commit",
            "authority-test",
            VerificationStatus::Verified,
            Vec::new(),
        )
        .unwrap()
    }

    #[test]
    fn passed_command_checks_require_bound_command_receipts() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(
            directory.path().join("Cargo.toml"),
            "[package]\nname='x'\nversion='0.1.0'\n",
        )
        .unwrap();
        fs::create_dir_all(directory.path().join("src")).unwrap();
        fs::write(directory.path().join("src/lib.rs"), "pub fn x() {}\n").unwrap();
        let components = vec![ChangedComponent::new(ChangeKind::Modified, "src/lib.rs").unwrap()];
        let plan = VerificationPlanner::plan(directory.path(), "repo", "commit", &components, &[])
            .unwrap();
        let record = verified_observation("commands were reported as passing");
        let checks: Vec<VerificationCheckReceipt> = plan
            .checks
            .iter()
            .map(|check| {
                VerificationCheckReceipt::new(
                    check,
                    true,
                    None,
                    vec![record.id.clone()],
                    Vec::new(),
                    Vec::new(),
                )
            })
            .collect();
        let mut evidence = EvidenceBundle::new("repo", "commit");
        evidence.records.push(record);
        let weak =
            InnerVerificationReceipt::new(plan.clone(), checks.clone(), evidence.clone()).unwrap();
        assert!(VerificationReceipt::new(plan, checks, evidence).is_err());
        let serialized = serde_json::to_vec(&weak).unwrap();
        assert!(serde_json::from_slice::<VerificationReceipt>(&serialized).is_err());
    }

    #[test]
    fn passed_behavior_checks_require_durable_artifact_evidence() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("App.tsx"), "export default 1;\n").unwrap();
        let components = vec![ChangedComponent::new(ChangeKind::Modified, "App.tsx").unwrap()];
        let plan = VerificationPlanner::plan(directory.path(), "repo", "commit", &components, &[])
            .unwrap();
        let record = verified_observation("browser checks were reported as passing");
        let checks: Vec<VerificationCheckReceipt> = plan
            .checks
            .iter()
            .map(|check| {
                VerificationCheckReceipt::new(
                    check,
                    true,
                    None,
                    vec![record.id.clone()],
                    Vec::new(),
                    Vec::new(),
                )
            })
            .collect();
        let mut evidence = EvidenceBundle::new("repo", "commit");
        evidence.records.push(record);
        assert!(VerificationReceipt::new(plan, checks, evidence).is_err());
    }
}
