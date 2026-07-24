use medusa_escalation::{EscalationMode, EscalationReason};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

/// Durable lifecycle state for one external reasoning escalation.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EscalationStatus {
    Prepared,
    Exported,
    AwaitingAdvice,
    AdviceImported,
    Applied,
    Rejected,
    Superseded,
}

/// Provenance and control data persisted with an agent session.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SessionEscalation {
    pub packet_id: String,
    pub packet_digest_sha256: String,
    pub task_id: String,
    pub mode: EscalationMode,
    pub reasons: Vec<EscalationReason>,
    pub status: EscalationStatus,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub exported_at: Option<OffsetDateTime>,
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub advice_imported_at: Option<OffsetDateTime>,
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub resolved_at: Option<OffsetDateTime>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub advice_digest_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolution_summary: Option<String>,
}

impl SessionEscalation {
    pub fn new(
        packet_id: impl Into<String>,
        packet_digest_sha256: impl Into<String>,
        task_id: impl Into<String>,
        mode: EscalationMode,
        reasons: Vec<EscalationReason>,
        created_at: OffsetDateTime,
    ) -> Result<Self, &'static str> {
        let state = Self {
            packet_id: packet_id.into(),
            packet_digest_sha256: packet_digest_sha256.into(),
            task_id: task_id.into(),
            mode,
            reasons,
            status: EscalationStatus::Prepared,
            created_at,
            exported_at: None,
            advice_imported_at: None,
            resolved_at: None,
            advice_digest_sha256: None,
            resolution_summary: None,
        };
        state.validate()?;
        Ok(state)
    }

    pub fn validate(&self) -> Result<(), &'static str> {
        if self.packet_id.trim().is_empty() || self.task_id.trim().is_empty() {
            return Err("escalation identifiers cannot be empty");
        }
        if self.packet_digest_sha256.len() != 64
            || !self
                .packet_digest_sha256
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
        {
            return Err("packet digest must be a 64-character SHA-256 hex value");
        }
        if self.reasons.is_empty() {
            return Err("durable escalation requires at least one reason");
        }
        if self
            .advice_digest_sha256
            .as_ref()
            .is_some_and(|digest| digest.len() != 64 || !digest.bytes().all(|b| b.is_ascii_hexdigit()))
        {
            return Err("advice digest must be a 64-character SHA-256 hex value");
        }
        Ok(())
    }

    pub fn mark_exported(&mut self, at: OffsetDateTime) -> Result<(), &'static str> {
        if self.status != EscalationStatus::Prepared {
            return Err("only prepared escalation packets can be exported");
        }
        self.status = EscalationStatus::AwaitingAdvice;
        self.exported_at = Some(at);
        Ok(())
    }

    pub fn import_advice(
        &mut self,
        advice_digest_sha256: impl Into<String>,
        at: OffsetDateTime,
    ) -> Result<(), &'static str> {
        if self.status != EscalationStatus::AwaitingAdvice {
            return Err("advice can only be imported for an awaiting escalation");
        }
        self.advice_digest_sha256 = Some(advice_digest_sha256.into());
        self.advice_imported_at = Some(at);
        self.status = EscalationStatus::AdviceImported;
        self.validate()
    }

    pub fn resolve(
        &mut self,
        applied: bool,
        summary: impl Into<String>,
        at: OffsetDateTime,
    ) -> Result<(), &'static str> {
        if self.status != EscalationStatus::AdviceImported {
            return Err("only imported advice can be resolved");
        }
        let summary = summary.into();
        if summary.trim().is_empty() {
            return Err("escalation resolution summary cannot be empty");
        }
        self.status = if applied {
            EscalationStatus::Applied
        } else {
            EscalationStatus::Rejected
        };
        self.resolution_summary = Some(summary);
        self.resolved_at = Some(at);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn digest(character: char) -> String {
        std::iter::repeat_n(character, 64).collect()
    }

    #[test]
    fn lifecycle_round_trips_through_json() {
        let mut state = SessionEscalation::new(
            "packet-1",
            digest('a'),
            "task-1",
            EscalationMode::Manual,
            vec![EscalationReason::LowConfidence],
            OffsetDateTime::UNIX_EPOCH,
        )
        .expect("state");
        state
            .mark_exported(OffsetDateTime::UNIX_EPOCH + time::Duration::seconds(1))
            .expect("export");
        state
            .import_advice(
                digest('b'),
                OffsetDateTime::UNIX_EPOCH + time::Duration::seconds(2),
            )
            .expect("import");
        state
            .resolve(
                true,
                "validated locally before use",
                OffsetDateTime::UNIX_EPOCH + time::Duration::seconds(3),
            )
            .expect("resolve");

        let json = serde_json::to_string(&state).expect("serialize");
        let restored: SessionEscalation = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(restored, state);
        assert_eq!(restored.status, EscalationStatus::Applied);
    }

    #[test]
    fn invalid_transitions_are_rejected() {
        let mut state = SessionEscalation::new(
            "packet-1",
            digest('a'),
            "task-1",
            EscalationMode::Manual,
            vec![EscalationReason::LowConfidence],
            OffsetDateTime::UNIX_EPOCH,
        )
        .expect("state");
        assert_eq!(
            state.import_advice(digest('b'), OffsetDateTime::UNIX_EPOCH),
            Err("advice can only be imported for an awaiting escalation")
        );
    }
}
