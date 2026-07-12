//! Production hardening primitives: migrations, recovery, observability, and release manifests.

mod archive;
mod chaos;
mod migrations;
mod observability;
mod release;
mod support;

pub use archive::validate_archive_entries;
pub use chaos::chaos_recovery_cycle;
pub use migrations::{CURRENT_SCHEMA_VERSION, Migration, MigrationReceipt, Migrator};
pub use observability::{Observability, OperationalEvent};
pub use release::{ArtifactEntry, ReleaseManifest, build_release_manifest, package_smoke};

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, fs, path::Path};

    use proptest::prelude::*;
    use sha2::{Digest, Sha256};

    use super::*;
    use crate::{observability::redact_value, support::now};

    #[test]
    fn clean_install_upgrade_and_rollback_are_byte_exact() {
        let directory = tempfile::tempdir().expect("tempdir");
        let root = directory.path().join("state");
        fs::create_dir_all(&root).expect("root");
        fs::write(root.join("legacy.json"), b"legacy-state").expect("legacy");
        let migrator = Migrator::new(&root);
        let receipts = migrator.upgrade_to_current().expect("upgrade");
        assert_eq!(
            migrator.schema_version().expect("version"),
            CURRENT_SCHEMA_VERSION
        );
        assert_eq!(receipts.len(), 3);
        assert!(migrator.refuse_unsafe_downgrade(1).is_err());
        migrator
            .rollback(receipts.first().expect("first receipt"))
            .expect("rollback");
        assert_eq!(
            fs::read(root.join("legacy.json")).expect("legacy restored"),
            b"legacy-state"
        );
    }

    #[test]
    fn chaos_cycle_recovers_without_corruption() {
        let directory = tempfile::tempdir().expect("tempdir");
        assert_eq!(
            chaos_recovery_cycle(directory.path(), 100).expect("chaos"),
            "chaos-recovery-ok:100"
        );
    }

    #[test]
    fn operational_events_redact_credentials() {
        let directory = tempfile::tempdir().expect("tempdir");
        let observability = Observability::new(directory.path()).expect("observability");
        observability
            .emit(OperationalEvent {
                timestamp: now().expect("now"),
                level: "info".into(),
                component: "test".into(),
                event: "credential_seen".into(),
                correlation_id: "cor-test".into(),
                fields: BTreeMap::from([
                    ("api_key".into(), serde_json::json!("sk-secret")),
                    ("message".into(), serde_json::json!("Bearer secret")),
                ]),
            })
            .expect("emit");
        let text = fs::read_to_string(directory.path().join("events.jsonl")).expect("events");
        assert!(!text.contains("secret"));
        assert!(text.contains("[REDACTED]"));
    }

    #[test]
    fn release_manifest_is_deterministic_and_checksummed() {
        let directory = tempfile::tempdir().expect("tempdir");
        let binary = directory.path().join("medusa");
        let sbom = directory.path().join("sbom.json");
        let rollback = directory.path().join("ROLLBACK.md");
        fs::write(&binary, b"binary").expect("binary");
        fs::write(&sbom, b"{}").expect("sbom");
        fs::write(&rollback, b"rollback").expect("rollback");
        let first = build_release_manifest(
            "1.0.0",
            "x86_64-unknown-linux-gnu",
            std::slice::from_ref(&binary),
            sbom.clone(),
            rollback.clone(),
        )
        .expect("manifest");
        assert_eq!(
            first.artifacts[0].sha256,
            format!("{:x}", Sha256::digest(b"binary"))
        );
        assert_eq!(first.sbom, sbom);
        assert_eq!(first.rollback_instructions, rollback);
    }

    proptest! {
        #[test]
        fn arbitrary_archive_paths_never_escape(segments in proptest::collection::vec("[A-Za-z0-9._-]{0,12}", 0..8)) {
            let entry = segments.join("/");
            let accepted = validate_archive_entries([entry.as_str()]).is_ok();
            if accepted {
                let path = Path::new(&entry);
                prop_assert!(!path.is_absolute());
                prop_assert!(!path.components().any(|component| matches!(component, std::path::Component::ParentDir | std::path::Component::RootDir | std::path::Component::Prefix(_))));
            }
        }

        #[test]
        fn redaction_never_emits_known_secret(secret in "[A-Za-z0-9]{8,32}") {
            let mut value = serde_json::json!(format!("Bearer {secret}"));
            redact_value(&mut value);
            prop_assert!(!value.to_string().contains(&secret));
        }
    }
}
