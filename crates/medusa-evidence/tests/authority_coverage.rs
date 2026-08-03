use std::{fs, path::Path};

use medusa_evidence::{
    ArtifactId, ArtifactSemanticClass, ArtifactStore, ChangeKind, ChangedComponent, CommandReceipt,
    EvidenceBundle, EvidenceKind, EvidenceRecord, EvidenceSource, VerificationCheckReceipt,
    VerificationPlan, VerificationPlanner, VerificationReceipt, VerificationStatus,
    validate_artifact_semantics,
};

fn command_plan(repo: &Path) -> VerificationPlan {
    fs::create_dir_all(repo.join(".medusa")).unwrap();
    fs::write(
        repo.join(".medusa/verification.json"),
        r#"{"checks":[{"kind":"unit","program":"verify","args":[],"reason":"authority coverage"}]}"#,
    )
    .unwrap();
    fs::write(repo.join("notes.custom"), "changed\n").unwrap();
    let components = vec![ChangedComponent::new(ChangeKind::Modified, "notes.custom").unwrap()];
    VerificationPlanner::plan(repo, "repo", "commit", &components, &[]).unwrap()
}

fn valid_command_receipt(repo: &Path) -> VerificationReceipt {
    let plan = command_plan(repo);
    let store = ArtifactStore::open(repo.join("artifacts")).unwrap();
    let mut evidence = EvidenceBundle::new("repo", "commit");
    let checks = plan
        .checks
        .iter()
        .map(|check| {
            let stdout = store
                .put_bytes(
                    "text/plain",
                    "authority-coverage",
                    format!("stdout:{}\n", check.id).as_bytes(),
                )
                .unwrap();
            let stderr = store
                .put_bytes(
                    "text/plain",
                    "authority-coverage",
                    format!("stderr:{}\n", check.id).as_bytes(),
                )
                .unwrap();
            let command = CommandReceipt::new(
                check,
                Some(0),
                false,
                5,
                stdout.id.clone(),
                stderr.id.clone(),
            );
            let record = EvidenceRecord::new(
                EvidenceKind::Observation,
                format!("{} passed", check.id),
                "repo",
                "commit",
                "authority-coverage",
                VerificationStatus::Verified,
                vec![EvidenceSource::CommandReceipt {
                    receipt_id: command.id.clone(),
                }],
            )
            .unwrap();
            evidence.artifacts.extend([stdout.clone(), stderr.clone()]);
            evidence.commands.push(command.clone());
            evidence.records.push(record.clone());
            VerificationCheckReceipt::new(
                check,
                true,
                Some(command),
                vec![record.id],
                vec![stdout.id, stderr.id],
                vec!["command proof accepted".to_owned()],
            )
        })
        .collect();
    VerificationReceipt::new(plan, checks, evidence).unwrap()
}

fn valid_artifact_receipt(
    repo: &Path,
    path: &str,
    kind: ChangeKind,
    artifact_bytes: &[u8],
) -> VerificationReceipt {
    fs::write(repo.join(path), artifact_bytes).unwrap();
    let components = vec![ChangedComponent::new(kind, path).unwrap()];
    let plan = VerificationPlanner::plan(repo, "repo", "commit", &components, &[]).unwrap();
    let store = ArtifactStore::open(repo.join("artifacts")).unwrap();
    let artifact = store
        .put_bytes(
            "application/octet-stream",
            "authority-coverage",
            artifact_bytes,
        )
        .unwrap();
    let (_, read) = store
        .read_range(&artifact.id, 0, artifact.byte_len, "authority-coverage")
        .unwrap();
    let record = EvidenceRecord::new(
        EvidenceKind::Observation,
        "durable artifact proof",
        "repo",
        "commit",
        "authority-coverage",
        VerificationStatus::Verified,
        vec![EvidenceSource::ArtifactRange {
            artifact_id: artifact.id.clone(),
            read_receipt_id: read.id.clone(),
            offset: read.offset,
            length: read.length,
            content_hash: read.content_hash.clone(),
        }],
    )
    .unwrap();
    let checks = plan
        .checks
        .iter()
        .map(|check| {
            VerificationCheckReceipt::new(
                check,
                true,
                None,
                vec![record.id.clone()],
                vec![artifact.id.clone()],
                vec!["artifact proof accepted".to_owned()],
            )
        })
        .collect();
    let mut evidence = EvidenceBundle::new("repo", "commit");
    evidence.artifacts.push(artifact);
    evidence.reads.push(read);
    evidence.records.push(record);
    VerificationReceipt::new(plan, checks, evidence).unwrap()
}

#[test]
fn command_authority_round_trips_with_exact_proof_bindings() {
    let directory = tempfile::tempdir().unwrap();
    let receipt = valid_command_receipt(directory.path());
    receipt.validate().unwrap();
    let serialized = serde_json::to_vec(&receipt).unwrap();
    let decoded: VerificationReceipt = serde_json::from_slice(&serialized).unwrap();
    assert_eq!(decoded, receipt);
    let summary = receipt.summary_lines().join("\n");
    assert!(summary.contains("verification_passed=true"));
    assert!(summary.contains("command proof accepted"));
}

#[test]
fn behavior_and_semantic_checks_accept_durable_artifact_reads() {
    let ui_directory = tempfile::tempdir().unwrap();
    let ui = valid_artifact_receipt(
        ui_directory.path(),
        "App.tsx",
        ChangeKind::Modified,
        b"export default function App() { return null; }\n",
    );
    assert!(ui.passed);
    assert!(ui.checks.iter().all(|check| check.passed));

    let artifact_directory = tempfile::tempdir().unwrap();
    let artifact = valid_artifact_receipt(
        artifact_directory.path(),
        "report.json",
        ChangeKind::Added,
        br#"{"ok":true}"#,
    );
    assert!(artifact.passed);
    assert!(artifact.checks.iter().all(|check| check.passed));
}

#[test]
fn artifact_store_covers_invalid_ranges_pages_and_searches() {
    let directory = tempfile::tempdir().unwrap();
    let store = ArtifactStore::open(directory.path().join("artifacts")).unwrap();
    assert!(store.root().ends_with("artifacts"));
    assert!(store.put_bytes("", "producer", b"bad").is_err());
    assert!(
        store
            .metadata(&ArtifactId("artifact-missing".to_owned()))
            .is_err()
    );

    let text = store
        .put_bytes("text/plain", "authority-coverage", b"alpha beta alpha\n")
        .unwrap();
    assert!(store.read_range(&text.id, 0, 0, "reader").is_err());
    assert!(
        store
            .read_range(&text.id, text.byte_len, 1, "reader")
            .is_err()
    );
    assert!(
        store
            .read_page(&text.id, text.page_count, "reader")
            .is_err()
    );
    assert!(store.search_text(&text.id, "", "reader").is_err());
    let (hits, receipt) = store.search_text(&text.id, "alpha", "reader").unwrap();
    assert_eq!(hits.len(), 2);
    assert_eq!(receipt.artifact_id, text.id);

    let binary = store
        .put_bytes(
            "application/octet-stream",
            "authority-coverage",
            b"\0binary",
        )
        .unwrap();
    assert!(store.search_text(&binary.id, "binary", "reader").is_err());
}

#[test]
fn semantic_validator_classifies_supported_artifact_formats() {
    let directory = tempfile::tempdir().unwrap();
    let cases: [(&str, &[u8], ArtifactSemanticClass); 7] = [
        (
            "report.json",
            br#"{"ok":true}"#,
            ArtifactSemanticClass::Json,
        ),
        (
            "page.html",
            b"<html><body>ok</body></html>",
            ArtifactSemanticClass::Html,
        ),
        (
            "image.png",
            b"\x89PNG\r\n\x1a\nbody",
            ArtifactSemanticClass::Png,
        ),
        (
            "document.pdf",
            b"%PDF-1.7\nbody",
            ArtifactSemanticClass::Pdf,
        ),
        ("archive.zip", b"PK\x03\x04body", ArtifactSemanticClass::Zip),
        ("notes.md", b"covered text\n", ArtifactSemanticClass::Text),
        ("payload.bin", b"\0binary", ArtifactSemanticClass::Binary),
    ];
    for (name, bytes, expected) in cases {
        let path = directory.path().join(name);
        fs::write(&path, bytes).unwrap();
        let result = validate_artifact_semantics(&path).unwrap();
        assert_eq!(result.class, expected, "{name}");
        assert!(result.passed, "{name}: {:?}", result.details);
    }

    let missing = validate_artifact_semantics(&directory.path().join("missing.bin")).unwrap();
    assert_eq!(missing.class, ArtifactSemanticClass::Binary);
    assert!(!missing.passed);
}
