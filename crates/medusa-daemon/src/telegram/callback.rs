use std::collections::BTreeMap;

use medusa_protocol::frontend::{
    ApprovalDecision, FRONTEND_PROTOCOL_VERSION, FrontendCommand, FrontendCommandEnvelope,
    FrontendKind,
};
use serde::{Deserialize, Serialize};
use time::{Duration, OffsetDateTime};
use ulid::Ulid;

use super::{TelegramGatewayError, TelegramIdentity, command::client_id};

const CALLBACK_PREFIX: &str = "m1:";
const MAX_CALLBACK_RECORDS: usize = 4_096;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TelegramInlineButton {
    pub label: String,
    pub callback_data: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub(crate) struct CallbackStore {
    records: BTreeMap<String, CallbackRecord>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct CallbackRecord {
    nonce: String,
    user_id: i64,
    chat_id: i64,
    topic_id: Option<i64>,
    session_id: String,
    turn_id: Option<String>,
    approval_id: String,
    decision: ApprovalDecision,
    #[serde(with = "time::serde::rfc3339")]
    expires_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    issued_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option")]
    consumed_at: Option<OffsetDateTime>,
}

struct ResolvedCallback {
    nonce: String,
    session_id: String,
    turn_id: Option<String>,
    approval_id: String,
    decision: ApprovalDecision,
}

impl CallbackStore {
    pub(crate) fn issue_approval(
        &mut self,
        identity: &TelegramIdentity,
        session_id: &str,
        turn_id: Option<&str>,
        approval_id: &str,
        expires_at: OffsetDateTime,
        now: OffsetDateTime,
    ) -> Result<Vec<TelegramInlineButton>, TelegramGatewayError> {
        if session_id.trim().is_empty() || approval_id.trim().is_empty() || expires_at <= now {
            return Err(TelegramGatewayError::InvalidCallbackRequest);
        }
        let approve = self.issue(
            identity,
            session_id,
            turn_id,
            approval_id,
            ApprovalDecision::ApproveOnce,
            expires_at,
            now,
        );
        let deny = self.issue(
            identity,
            session_id,
            turn_id,
            approval_id,
            ApprovalDecision::Deny,
            expires_at,
            now,
        );
        Ok(vec![
            TelegramInlineButton {
                label: "Approve once".to_owned(),
                callback_data: approve,
            },
            TelegramInlineButton {
                label: "Deny".to_owned(),
                callback_data: deny,
            },
        ])
    }

    pub(crate) fn resolve(
        &mut self,
        identity: &TelegramIdentity,
        callback_data: &str,
        now: OffsetDateTime,
    ) -> Result<FrontendCommandEnvelope, TelegramGatewayError> {
        let nonce = callback_data
            .strip_prefix(CALLBACK_PREFIX)
            .ok_or(TelegramGatewayError::InvalidCallback)?;
        if nonce.is_empty() || callback_data.len() > 64 {
            return Err(TelegramGatewayError::InvalidCallback);
        }
        let record = self
            .records
            .get(nonce)
            .ok_or(TelegramGatewayError::InvalidCallback)?;
        if record.user_id != identity.user_id
            || record.chat_id != identity.chat_id
            || record.topic_id != identity.topic_id
        {
            return Err(TelegramGatewayError::CallbackIdentityMismatch);
        }
        if record.consumed_at.is_some() {
            return Err(TelegramGatewayError::CallbackAlreadyResolved);
        }
        if record.expires_at <= now {
            return Err(TelegramGatewayError::CallbackExpired);
        }
        let resolved = ResolvedCallback {
            nonce: record.nonce.clone(),
            session_id: record.session_id.clone(),
            turn_id: record.turn_id.clone(),
            approval_id: record.approval_id.clone(),
            decision: record.decision,
        };
        let envelope = FrontendCommandEnvelope {
            protocol_version: FRONTEND_PROTOCOL_VERSION,
            command_id: format!("telegram-callback-{}", resolved.nonce),
            idempotency_key: format!("telegram-callback:{}", resolved.nonce),
            frontend: FrontendKind::Telegram,
            client_id: client_id(identity),
            session_id: Some(resolved.session_id.clone()),
            turn_id: resolved.turn_id.clone(),
            timestamp: now,
            command: FrontendCommand::ResolveApproval {
                approval_id: resolved.approval_id.clone(),
                decision: resolved.decision,
            },
        };
        envelope
            .validate()
            .map_err(|error| TelegramGatewayError::Protocol(error.to_owned()))?;
        for record in self.records.values_mut() {
            if record.user_id == identity.user_id
                && record.chat_id == identity.chat_id
                && record.topic_id == identity.topic_id
                && record.session_id == resolved.session_id
                && record.turn_id == resolved.turn_id
                && record.approval_id == resolved.approval_id
            {
                record.consumed_at = Some(now);
            }
        }
        Ok(envelope)
    }

    #[allow(clippy::too_many_arguments)]
    fn issue(
        &mut self,
        identity: &TelegramIdentity,
        session_id: &str,
        turn_id: Option<&str>,
        approval_id: &str,
        decision: ApprovalDecision,
        expires_at: OffsetDateTime,
        now: OffsetDateTime,
    ) -> String {
        self.prune(now);
        let nonce = Ulid::new().to_string();
        self.records.insert(
            nonce.clone(),
            CallbackRecord {
                nonce: nonce.clone(),
                user_id: identity.user_id,
                chat_id: identity.chat_id,
                topic_id: identity.topic_id,
                session_id: session_id.to_owned(),
                turn_id: turn_id.map(str::to_owned),
                approval_id: approval_id.to_owned(),
                decision,
                expires_at,
                issued_at: now,
                consumed_at: None,
            },
        );
        format!("{CALLBACK_PREFIX}{nonce}")
    }

    fn prune(&mut self, now: OffsetDateTime) {
        self.records.retain(|_, record| {
            record.expires_at >= now
                || record
                    .consumed_at
                    .is_some_and(|consumed| consumed + Duration::days(1) >= now)
        });
        while self.records.len() >= MAX_CALLBACK_RECORDS {
            let Some(oldest) = self
                .records
                .iter()
                .min_by_key(|(_, record)| record.issued_at)
                .map(|(nonce, _)| nonce.clone())
            else {
                break;
            };
            self.records.remove(&oldest);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::telegram::{TelegramChatKind, TelegramIdentity};
    use time::macros::datetime;

    fn identity() -> TelegramIdentity {
        TelegramIdentity {
            user_id: 42,
            chat_id: 42,
            topic_id: None,
            chat_kind: TelegramChatKind::Private,
            bot_mentioned: false,
        }
    }

    #[test]
    fn callbacks_are_opaque_bound_expiring_and_one_shot() {
        let mut store = CallbackStore::default();
        let now = datetime!(2026-07-30 16:00 UTC);
        let buttons = store
            .issue_approval(
                &identity(),
                "session-1",
                Some("turn-1"),
                "approval-1",
                now + Duration::minutes(5),
                now,
            )
            .expect("callbacks");
        assert_eq!(buttons.len(), 2);
        assert!(buttons[0].callback_data.starts_with(CALLBACK_PREFIX));
        assert!(!buttons[0].callback_data.contains("approval-1"));
        let command = store
            .resolve(&identity(), &buttons[0].callback_data, now)
            .expect("resolve");
        assert!(matches!(
            command.command,
            FrontendCommand::ResolveApproval {
                decision: ApprovalDecision::ApproveOnce,
                ..
            }
        ));
        assert!(matches!(
            store.resolve(&identity(), &buttons[0].callback_data, now),
            Err(TelegramGatewayError::CallbackAlreadyResolved)
        ));
        assert!(matches!(
            store.resolve(&identity(), &buttons[1].callback_data, now),
            Err(TelegramGatewayError::CallbackAlreadyResolved)
        ));
    }
}
