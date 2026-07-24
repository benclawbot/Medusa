use std::{
    fs,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use time::OffsetDateTime;

use crate::EscalationPacket;

/// One repository input that changed after an escalation packet was exported.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct StaleReadSetEntry {
    pub path: String,
    pub expected_sha256: String,
    pub actual_sha256: Option<String>,
}

/// User-supplied advice bound to one exported escalation packet.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AdviceEnvelope {
    pub schema_version: u16,
    pub packet_id: String,
    pub packet_digest_sha256: String,
    pub advice: String,
    #[serde(with = "time::serde::rfc3339")]
    pub imported_at: OffsetDateTime,
    pub advice_digest_sha256: String,
}

impl AdviceEnvelope {
    pub fn new(
        packet: &EscalationPacket,
        advice: impl Into<String>,
        imported_at: OffsetDateTime,
    ) -> Result<Self, &'static str> {
        if !packet
            .verify_digest()
            .map_err(|_| "could not verify packet digest")?
        {
            return Err("escalation packet digest is invalid");
        }
        let advice = advice.into();
        if advice.trim().is_empty() {
            return Err("imported advice cannot be empty");
        }
        Ok(Self {
            schema_version: 1,
            packet_id: packet.packet_id.clone(),
            packet_digest_sha256: packet.digest_sha256.clone(),
            advice_digest_sha256: sha256_hex(advice.as_bytes()),
            advice,
            imported_at,
        })
    }

    pub fn validate_for(&self, packet: &EscalationPacket) -> Result<(), &'static str> {
        if self.schema_version != 1 {
            return Err("unsupported advice envelope schema version");
        }
        if self.packet_id != packet.packet_id {
            return Err("advice packet identifier does not match escalation packet");
        }
        if self.packet_digest_sha256 != packet.digest_sha256 {
            return Err("advice packet digest does not match escalation packet");
        }
        if self.advice.trim().is_empty() {
            return Err("imported advice cannot be empty");
        }
        if self.advice_digest_sha256 != sha256_hex(self.advice.as_bytes()) {
            return Err("advice digest does not match advice content");
        }
        Ok(())
    }

    /// Refuses to use advice when any file read to build the packet has changed or disappeared.
    pub fn validate_fresh_read_set(
        &self,
        packet: &EscalationPacket,
        repo: &Path,
    ) -> Result<(), Vec<StaleReadSetEntry>> {
        let stale = stale_read_set(packet, repo);
        if stale.is_empty() {
            Ok(())
        } else {
            Err(stale)
        }
    }
}

/// Compares every packet read-set digest against the current repository state.
#[must_use]
pub fn stale_read_set(packet: &EscalationPacket, repo: &Path) -> Vec<StaleReadSetEntry> {
    packet
        .read_set
        .iter()
        .filter_map(|entry| {
            let path = repo.join(&entry.path);
            let actual = fs::read(path).ok().map(|bytes| sha256_hex(&bytes));
            (actual.as_deref() != Some(entry.content_sha256.as_str())).then(|| {
                StaleReadSetEntry {
                    path: entry.path.clone(),
                    expected_sha256: entry.content_sha256.clone(),
                    actual_sha256: actual,
                }
            })
        })
        .collect()
}

/// Writes a pretty JSON packet for copy/paste or attachment to a ChatGPT conversation.
pub fn export_packet(
    directory: &Path,
    packet: &EscalationPacket,
) -> Result<PathBuf, std::io::Error> {
    if !packet.verify_digest().unwrap_or(false) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "escalation packet digest is invalid",
        ));
    }
    fs::create_dir_all(directory)?;
    let path = directory.join(format!("{}.packet.json", safe_name(&packet.packet_id)));
    atomic_write(
        &path,
        &serde_json::to_vec_pretty(packet).map_err(json_io_error)?,
    )?;
    Ok(path)
}

/// Loads and validates an advice envelope against the exact exported packet.
pub fn import_advice(
    path: &Path,
    packet: &EscalationPacket,
) -> Result<AdviceEnvelope, std::io::Error> {
    let envelope: AdviceEnvelope =
        serde_json::from_slice(&fs::read(path)?).map_err(json_io_error)?;
    envelope
        .validate_for(packet)
        .map_err(|message| std::io::Error::new(std::io::ErrorKind::InvalidData, message))?;
    Ok(envelope)
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), std::io::Error> {
    let temporary = path.with_extension("json.tmp");
    fs::write(&temporary, bytes)?;
    fs::rename(temporary, path)
}

fn json_io_error(error: serde_json::Error) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, error)
}

fn safe_name(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.') {
                character
            } else {
                '_'
            }
        })
        .collect()
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;
    use crate::{EscalationMode, EscalationReason, ReadSetEntry};

    fn packet() -> EscalationPacket {
        EscalationPacket::new(
            "packet-1",
            "session-1",
            "task-1",
            EscalationMode::Manual,
            "fix parser",
            "Which invariant is violated?",
            BTreeSet::from([EscalationReason::ExplicitUserRequest]),
            OffsetDateTime::UNIX_EPOCH,
        )
        .expect("packet")
    }

    #[test]
    fn advice_is_bound_to_exact_packet() {
        let packet = packet();
        let envelope = AdviceEnvelope::new(
            &packet,
            "Inspect token boundaries.",
            OffsetDateTime::UNIX_EPOCH,
        )
        .expect("envelope");
        envelope.validate_for(&packet).expect("valid");

        let mut other = packet.clone();
        other.packet_id = "packet-2".into();
        other.refresh_digest().expect("digest");
        assert_eq!(
            envelope.validate_for(&other),
            Err("advice packet identifier does not match escalation packet")
        );
    }

    #[test]
    fn tampered_advice_is_rejected() {
        let packet = packet();
        let mut envelope =
            AdviceEnvelope::new(&packet, "Original", OffsetDateTime::UNIX_EPOCH)
                .expect("envelope");
        envelope.advice = "Tampered".into();
        assert_eq!(
            envelope.validate_for(&packet),
            Err("advice digest does not match advice content")
        );
    }

    #[test]
    fn packet_and_advice_round_trip_on_disk() {
        let directory = tempfile::tempdir().expect("tempdir");
        let packet = packet();
        let packet_path = export_packet(directory.path(), &packet).expect("export");
        assert!(packet_path.is_file());

        let envelope = AdviceEnvelope::new(
            &packet,
            "Use the shared parser helper.",
            OffsetDateTime::UNIX_EPOCH,
        )
        .expect("envelope");
        let advice_path = directory.path().join("answer.advice.json");
        fs::write(
            &advice_path,
            serde_json::to_vec_pretty(&envelope).unwrap(),
        )
        .unwrap();
        assert_eq!(
            import_advice(&advice_path, &packet).expect("import"),
            envelope
        );
    }

    #[test]
    fn changed_or_missing_reads_are_reported_as_stale() {
        let directory = tempfile::tempdir().expect("tempdir");
        fs::write(directory.path().join("parser.rs"), "old").expect("fixture");
        let mut packet = packet();
        packet.read_set.insert(ReadSetEntry {
            path: "parser.rs".into(),
            content_sha256: sha256_hex(b"old"),
        });
        packet.read_set.insert(ReadSetEntry {
            path: "missing.rs".into(),
            content_sha256: sha256_hex(b"present before export"),
        });
        packet.refresh_digest().expect("digest");
        let envelope = AdviceEnvelope::new(&packet, "Review parser state.", OffsetDateTime::UNIX_EPOCH)
            .expect("envelope");

        fs::write(directory.path().join("parser.rs"), "new").expect("mutation");
        let stale = envelope
            .validate_fresh_read_set(&packet, directory.path())
            .expect_err("stale");
        assert_eq!(stale.len(), 2);
        assert!(stale.iter().any(|entry| entry.path == "parser.rs"));
        assert!(stale.iter().any(|entry| entry.path == "missing.rs" && entry.actual_sha256.is_none()));
    }
}