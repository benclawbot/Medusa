use std::path::{Path, PathBuf};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult, SessionId};
use medusa_escalation::{AdviceEnvelope, EscalationPacket, export_packet, import_advice};
use time::OffsetDateTime;

use super::{load_escalation_journal, persist_escalation_journal};

/// Exports a validated packet and advances its durable journal entry to awaiting advice.
pub fn export_manual_escalation(
    repo: &Path,
    session_id: &SessionId,
    packet: &EscalationPacket,
) -> MedusaResult<PathBuf> {
    validate_session_binding(session_id, packet)?;
    let directory = repo.join(".medusa/escalations/exchange");
    let path = export_packet(&directory, packet).map_err(exchange_error)?;

    let mut journal = load_escalation_journal(repo, session_id)?;
    let entry = journal
        .entries
        .iter_mut()
        .find(|entry| entry.packet_id == packet.packet_id)
        .ok_or_else(|| exchange_error("escalation packet is not registered in the session journal"))?;
    if entry.packet_digest_sha256 != packet.digest_sha256 {
        return Err(exchange_error("journal packet digest does not match exported packet"));
    }
    entry
        .mark_exported(OffsetDateTime::now_utc())
        .map_err(exchange_error)?;
    persist_escalation_journal(repo, session_id, &journal)?;
    Ok(path)
}

/// Imports advice bound to a packet and advances the durable journal entry.
pub fn import_manual_advice(
    repo: &Path,
    session_id: &SessionId,
    packet: &EscalationPacket,
    advice_path: &Path,
) -> MedusaResult<AdviceEnvelope> {
    validate_session_binding(session_id, packet)?;
    let envelope = import_advice(advice_path, packet).map_err(exchange_error)?;

    let mut journal = load_escalation_journal(repo, session_id)?;
    let entry = journal
        .entries
        .iter_mut()
        .find(|entry| entry.packet_id == packet.packet_id)
        .ok_or_else(|| exchange_error("escalation packet is not registered in the session journal"))?;
    if entry.packet_digest_sha256 != packet.digest_sha256 {
        return Err(exchange_error("journal packet digest does not match imported advice"));
    }
    entry
        .import_advice(envelope.advice_digest_sha256.clone(), envelope.imported_at)
        .map_err(exchange_error)?;
    persist_escalation_journal(repo, session_id, &journal)?;
    Ok(envelope)
}

fn validate_session_binding(
    session_id: &SessionId,
    packet: &EscalationPacket,
) -> MedusaResult<()> {
    if packet.session_id != session_id.as_str() {
        return Err(exchange_error("escalation packet belongs to another session"));
    }
    if !packet.verify_digest().map_err(exchange_error)? {
        return Err(exchange_error("escalation packet digest is invalid"));
    }
    Ok(())
}

fn exchange_error(message: impl ToString) -> MedusaError {
    MedusaError::new(
        ErrorCode::InvalidConfiguration,
        ErrorCategory::Validation,
        message.to_string(),
    )
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeSet, fs};

    use medusa_escalation::{AdviceEnvelope, EscalationMode, EscalationReason};

    use super::*;
    use crate::session::{
        EscalationJournal, EscalationStatus, SessionEscalation, persist_escalation_journal,
    };

    fn packet(session_id: &SessionId) -> EscalationPacket {
        EscalationPacket::new(
            "packet-1",
            session_id.as_str(),
            "task-1",
            EscalationMode::Manual,
            "fix parser",
            "Which invariant is violated?",
            BTreeSet::from([EscalationReason::ExplicitUserRequest]),
            OffsetDateTime::UNIX_EPOCH,
        )
        .expect("packet")
    }

    fn register(repo: &Path, session_id: &SessionId, packet: &EscalationPacket) {
        let mut journal = EscalationJournal::new(session_id.as_str());
        journal
            .push(
                SessionEscalation::new(
                    &packet.packet_id,
                    &packet.digest_sha256,
                    &packet.task_id,
                    packet.mode,
                    packet.reasons.iter().copied().collect(),
                    packet.created_at,
                )
                .expect("entry"),
            )
            .expect("push");
        persist_escalation_journal(repo, session_id, &journal).expect("persist");
    }

    #[test]
    fn export_and_import_advance_durable_lifecycle() {
        let directory = tempfile::tempdir().expect("tempdir");
        let session_id = SessionId::new();
        let packet = packet(&session_id);
        register(directory.path(), &session_id, &packet);

        let exported = export_manual_escalation(directory.path(), &session_id, &packet)
            .expect("export");
        assert!(exported.is_file());

        let envelope = AdviceEnvelope::new(
            &packet,
            "Inspect the shared parser helper.",
            OffsetDateTime::UNIX_EPOCH + time::Duration::seconds(1),
        )
        .expect("envelope");
        let advice_path = directory.path().join("advice.json");
        fs::write(&advice_path, serde_json::to_vec_pretty(&envelope).unwrap()).unwrap();
        import_manual_advice(directory.path(), &session_id, &packet, &advice_path)
            .expect("import");

        let journal = load_escalation_journal(directory.path(), &session_id).expect("load");
        assert_eq!(journal.entries[0].status, EscalationStatus::AdviceImported);
        assert_eq!(
            journal.entries[0].advice_digest_sha256.as_deref(),
            Some(envelope.advice_digest_sha256.as_str())
        );
    }

    #[test]
    fn cross_session_packet_is_rejected() {
        let directory = tempfile::tempdir().expect("tempdir");
        let packet_session = SessionId::new();
        let other_session = SessionId::new();
        let packet = packet(&packet_session);
        assert!(
            export_manual_escalation(directory.path(), &other_session, &packet)
                .expect_err("wrong session")
                .message
                .contains("another session")
        );
    }
}
