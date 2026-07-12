use std::{fs, path::PathBuf};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use serde::{Deserialize, Serialize};
use ulid::Ulid;

use crate::support::{
    atomic_json, atomic_write, copy_tree, directory_digest, invalid, now, restore_tree,
};

pub const CURRENT_SCHEMA_VERSION: u32 = 3;

/// One reversible application-state migration.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Migration {
    pub from: u32,
    pub to: u32,
    pub name: String,
}

/// Durable migration receipt with backup provenance.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct MigrationReceipt {
    pub migration_id: String,
    pub from: u32,
    pub to: u32,
    pub backup_directory: PathBuf,
    pub completed_at: String,
    pub before_digest: String,
    pub after_digest: String,
}

/// Versioned state migrator. Every upgrade takes a complete backup first.
pub struct Migrator {
    root: PathBuf,
}

impl Migrator {
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn schema_version(&self) -> MedusaResult<u32> {
        let path = self.root.join("schema-version");
        if !path.exists() {
            return Ok(0);
        }
        fs::read_to_string(path)?
            .trim()
            .parse()
            .map_err(|_| invalid("schema-version is not an integer"))
    }

    pub fn upgrade_to_current(&self) -> MedusaResult<Vec<MigrationReceipt>> {
        fs::create_dir_all(&self.root)?;
        let migrations = [
            Migration {
                from: 0,
                to: 1,
                name: "initialize-layout".into(),
            },
            Migration {
                from: 1,
                to: 2,
                name: "add-observability".into(),
            },
            Migration {
                from: 2,
                to: 3,
                name: "add-release-state".into(),
            },
        ];
        let mut receipts = Vec::new();
        while self.schema_version()? < CURRENT_SCHEMA_VERSION {
            let current = self.schema_version()?;
            let migration = migrations
                .iter()
                .find(|migration| migration.from == current)
                .ok_or_else(|| invalid(format!("no migration from schema {current}")))?;
            receipts.push(self.apply(migration)?);
        }
        Ok(receipts)
    }

    pub fn apply(&self, migration: &Migration) -> MedusaResult<MigrationReceipt> {
        let actual = self.schema_version()?;
        if actual != migration.from || migration.to != migration.from + 1 {
            return Err(invalid(format!(
                "invalid migration {} -> {}; current schema is {actual}",
                migration.from, migration.to
            )));
        }
        let before_digest = directory_digest(&self.root)?;
        let backup_directory = self
            .root
            .join("backups")
            .join(format!("migration-{}", Ulid::new()));
        copy_tree(&self.root, &backup_directory, Some("backups"))?;
        if let Err(error) = self.apply_contents(migration) {
            restore_tree(&backup_directory, &self.root)?;
            return Err(error);
        }
        atomic_write(
            &self.root.join("schema-version"),
            migration.to.to_string().as_bytes(),
        )?;
        let receipt = MigrationReceipt {
            migration_id: format!("mig-{}", Ulid::new()),
            from: migration.from,
            to: migration.to,
            backup_directory,
            completed_at: now()?,
            before_digest,
            after_digest: directory_digest(&self.root)?,
        };
        atomic_json(
            &self
                .root
                .join("migration-history")
                .join(format!("{}.json", receipt.migration_id)),
            &receipt,
        )?;
        Ok(receipt)
    }

    pub fn rollback(&self, receipt: &MigrationReceipt) -> MedusaResult<()> {
        if !receipt.backup_directory.is_dir() {
            return Err(invalid("migration backup is unavailable"));
        }
        restore_tree(&receipt.backup_directory, &self.root)?;
        if directory_digest(&self.root)? != receipt.before_digest {
            return Err(MedusaError::new(
                ErrorCode::ChecksumMismatch,
                ErrorCategory::Persistence,
                "rollback did not restore byte-identical state",
            ));
        }
        Ok(())
    }

    pub fn refuse_unsafe_downgrade(&self, target: u32) -> MedusaResult<()> {
        let current = self.schema_version()?;
        if target < current {
            return Err(MedusaError::new(
                ErrorCode::PolicyDenied,
                ErrorCategory::Policy,
                format!("unsafe downgrade from schema {current} to {target} is refused"),
            ));
        }
        Ok(())
    }

    fn apply_contents(&self, migration: &Migration) -> MedusaResult<()> {
        match migration.to {
            1 => {
                fs::create_dir_all(self.root.join("sessions"))?;
                fs::create_dir_all(self.root.join("memory"))?;
            }
            2 => fs::create_dir_all(self.root.join("observability"))?,
            3 => fs::create_dir_all(self.root.join("release"))?,
            _ => return Err(invalid("unsupported migration target")),
        }
        Ok(())
    }
}
