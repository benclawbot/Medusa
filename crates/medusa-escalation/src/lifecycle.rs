use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::{EscalationMode, EscalationReason};

/// Append-only lifecycle record for one escalation packet.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum EscalationLifecycleEvent {
    Prepared {
        packet_id: String,
        packet_digest_sha256: String,
        session_id: String,
        task_id: String,
        mode: EscalationMode,
        reasons: Vec<EscalationReason>,
        #[serde(with = "time::serde::rfc3339")]
        occurred_at: OffsetDateTime,
    },
    Exported {
        packet_id: String,
        packet_digest_sha256: String,
        artifact_ref: String,
        #[serde(with = "time::serde::rfc3339")]
        occurred_at: OffsetDateTime,
    },
    AdviceImported {
        packet_id: String,
        packet_digest_sha256: String,
        advice_digest_sha256: String,
        #[serde(with = "time::serde::rfc3339")]
        occurred_at: OffsetDateTime,
    },
    AdviceRejected {
        packet_id: String,
        packet_digest_sha256: String,
        reason: String,
        stale_paths: Vec<String>,
        #[serde(with = "time::serde::rfc3339")]
        occurred_at: OffsetDateTime,
    },
    Resolved {
        packet_id: String,
        packet_digest_sha256: String,
        applied: bool,
        summary: String,
        #[serde(with = "time::serde::rfc3339")]
        occurred_at: OffsetDateTime,
    },
    Superseded {
        packet_id: String,
        packet_digest_sha256: String,
        replacement_packet_id: Option<String>,
        reason: String,
        #[serde(with = "time::serde::rfc3339")]
        occurred_at: OffsetDateTime,
    },
}

impl EscalationLifecycleEvent {
    #[must_use]
    pub fn packet_id(&self) -> &str {
        match self {
            Self::Prepared { packet_id, .. }
            | Self::Exported { packet_id, .. }
            | Self::AdviceImported { packet_id, .. }
            | Self::AdviceRejected { packet_id, .. }
            | Self::Resolved { packet_id, .. }
            | Self::Superseded { packet_id, .. } => packet_id,
        }
    }

    #[must_use]
    pub fn packet_digest_sha256(&self) -> &str {
        match self {
            Self::Prepared {
                packet_digest_sha256,
                ..
            }
            | Self::Exported {
                packet_digest_sha256,
                ..
            }
            | Self::AdviceImported {
                packet_digest_sha256,
                ..
            }
            | Self::AdviceRejected {
                packet_digest_sha256,
                ..
            }
            | Self::Resolved {
                packet_digest_sha256,
                ..
            }
            | Self::Superseded {
                packet_digest_sha256,
                ..
            } => packet_digest_sha256,
        }
    }

    pub fn validate(&self) -> Result<(), &'static str> {
        if self.packet_id().trim().is_empty() {
            return Err("escalation lifecycle packet identifier cannot be empty");
        }
        let digest = self.packet_digest_sha256();
        if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err("escalation lifecycle packet digest must be SHA-256 hex");
        }
        match self {
            Self::Prepared {
                session_id,
                task_id,
                reasons,
                ..
            } => {
                if session_id.trim().is_empty() || task_id.trim().is_empty() {
                    return Err("prepared escalation lifecycle identifiers cannot be empty");
                }
                if reasons.is_empty() {
                    return Err("prepared escalation lifecycle event requires a reason");
                }
            }
            Self::Exported { artifact_ref, .. } if artifact_ref.trim().is_empty() => {
                return Err("exported escalation lifecycle event requires an artifact reference");
            }
            Self::AdviceImported {
                advice_digest_sha256,
                ..
            } if advice_digest_sha256.len() != 64
                || !advice_digest_sha256
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit()) =>
            {
                return Err("advice lifecycle digest must be SHA-256 hex");
            }
            Self::AdviceRejected { reason, .. } | Self::Superseded { reason, .. }
                if reason.trim().is_empty() =>
            {
                return Err("rejected or superseded lifecycle event requires a reason");
            }
            Self::Resolved { summary, .. } if summary.trim().is_empty() => {
                return Err("resolved escalation lifecycle event requires a summary");
            }
            _ => {}
        }
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
    fn lifecycle_events_round_trip() {
        let event = EscalationLifecycleEvent::AdviceImported {
            packet_id: "packet-1".to_owned(),
            packet_digest_sha256: digest('a'),
            advice_digest_sha256: digest('b'),
            occurred_at: OffsetDateTime::UNIX_EPOCH,
        };
        event.validate().expect("valid lifecycle event");
        let json = serde_json::to_string(&event).expect("serialize");
        assert_eq!(
            serde_json::from_str::<EscalationLifecycleEvent>(&json).expect("deserialize"),
            event
        );
    }

    #[test]
    fn invalid_digest_is_rejected() {
        let event = EscalationLifecycleEvent::Exported {
            packet_id: "packet-1".to_owned(),
            packet_digest_sha256: "bad".to_owned(),
            artifact_ref: ".medusa/escalations/exchange/packet-1.packet.json".to_owned(),
            occurred_at: OffsetDateTime::UNIX_EPOCH,
        };
        assert_eq!(
            event.validate(),
            Err("escalation lifecycle packet digest must be SHA-256 hex")
        );
    }
}
