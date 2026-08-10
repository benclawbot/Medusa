//! Production bridge from `medusa-wakeup` deliveries to the durable session action plane.
//! The wakeup router remains authoritative for subscription matching and event sequencing; this
//! module only translates an already-authorized delivery into one idempotent session action.

use medusa_protocol::SessionActionWakePolicy;
use medusa_wakeup::{WakeupDelivery, WakeupSource};
use serde_json::Value;
use time::OffsetDateTime;

use crate::{
    RuntimeController, RuntimeError,
    scheduled_actions::{
        MissedRunPolicy, TriggerDeliveryMode, TriggerDispatchRecord, TriggerDispatchRequest,
        TriggerSourceKind,
    },
};

#[derive(Clone, Debug)]
pub struct WakeupActionBinding {
    pub prompt: String,
    pub delivery_mode: TriggerDeliveryMode,
    pub wake_policy: SessionActionWakePolicy,
    pub authorized: bool,
    pub enabled: bool,
    pub persistent_goal: bool,
    pub recurrence_seconds: Option<u64>,
    pub missed_run_policy: MissedRunPolicy,
}

impl RuntimeController {
    /// Converts one delivery emitted by the canonical wakeup router into one durable SessionAction.
    /// Replaying the same delivery produces the same trigger occurrence and therefore coalesces.
    pub fn dispatch_wakeup_delivery(
        &self,
        delivery: &WakeupDelivery,
        binding: WakeupActionBinding,
        scheduled_for: OffsetDateTime,
        observed_at: OffsetDateTime,
    ) -> Result<TriggerDispatchRecord, RuntimeError> {
        self.dispatch_trigger_action(trigger_request(
            delivery,
            binding,
            scheduled_for,
            observed_at,
        )?)
    }
}

fn trigger_request(
    delivery: &WakeupDelivery,
    binding: WakeupActionBinding,
    scheduled_for: OffsetDateTime,
    observed_at: OffsetDateTime,
) -> Result<TriggerDispatchRequest, RuntimeError> {
    let schedule_id = subscription_id(delivery)?;
    Ok(TriggerDispatchRequest {
        occurrence_id: format!("{schedule_id}:{}", delivery.event_sequence),
        schedule_id,
        source_kind: source_kind(&delivery.source),
        occurrence_sequence: delivery.event_sequence,
        scheduled_for,
        observed_at,
        target_session_id: delivery.owner.clone(),
        delivery_mode: binding.delivery_mode,
        wake_policy: binding.wake_policy,
        prompt: binding.prompt,
        authorized: binding.authorized,
        enabled: binding.enabled,
        persistent_goal: binding.persistent_goal,
        recurrence_seconds: binding.recurrence_seconds,
        missed_run_policy: binding.missed_run_policy,
    })
}

fn subscription_id(delivery: &WakeupDelivery) -> Result<String, RuntimeError> {
    let value = serde_json::to_value(&delivery.subscription_id).map_err(RuntimeError::agent)?;
    match value {
        Value::String(value) if !value.trim().is_empty() => Ok(value),
        _ => Err(RuntimeError::InvalidCommand(
            "wakeup delivery has an invalid subscription id".to_owned(),
        )),
    }
}

fn source_kind(source: &WakeupSource) -> TriggerSourceKind {
    match source {
        WakeupSource::Timer(_) => TriggerSourceKind::Timer,
        WakeupSource::HeartbeatStale(_) => TriggerSourceKind::Heartbeat,
        WakeupSource::FileChanged(_) => TriggerSourceKind::File,
        WakeupSource::ProcessExited(_) | WakeupSource::ProcessOrphaned(_) => {
            TriggerSourceKind::Process
        }
        WakeupSource::ExternalSignal(_) => TriggerSourceKind::ExternalSignal,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use medusa_wakeup::SubscriptionId;
    use time::OffsetDateTime;

    use super::*;

    fn binding() -> WakeupActionBinding {
        WakeupActionBinding {
            prompt: "inspect the wakeup".to_owned(),
            delivery_mode: TriggerDeliveryMode::FollowUp,
            wake_policy: SessionActionWakePolicy::OnBoundary,
            authorized: true,
            enabled: true,
            persistent_goal: false,
            recurrence_seconds: Some(60),
            missed_run_policy: MissedRunPolicy::Coalesce,
        }
    }

    fn delivery(source: WakeupSource) -> WakeupDelivery {
        WakeupDelivery {
            event_sequence: 9,
            subscription_id: SubscriptionId::parse("subscription-1").expect("id"),
            owner: "session-1".to_owned(),
            source,
            metadata: BTreeMap::new(),
        }
    }

    #[test]
    fn every_production_wakeup_family_uses_the_same_trigger_request_path() {
        let families = [
            (WakeupSource::Timer("timer".to_owned()), TriggerSourceKind::Timer),
            (
                WakeupSource::HeartbeatStale("worker".to_owned()),
                TriggerSourceKind::Heartbeat,
            ),
            (
                WakeupSource::FileChanged("src/lib.rs".to_owned()),
                TriggerSourceKind::File,
            ),
            (
                WakeupSource::ProcessExited("build".to_owned()),
                TriggerSourceKind::Process,
            ),
            (
                WakeupSource::ProcessOrphaned("build".to_owned()),
                TriggerSourceKind::Process,
            ),
            (
                WakeupSource::ExternalSignal("approval".to_owned()),
                TriggerSourceKind::ExternalSignal,
            ),
        ];
        for (source, expected) in families {
            let request = trigger_request(
                &delivery(source),
                binding(),
                OffsetDateTime::UNIX_EPOCH,
                OffsetDateTime::UNIX_EPOCH,
            )
            .expect("request");
            assert_eq!(request.source_kind, expected);
            assert_eq!(request.schedule_id, "subscription-1");
            assert_eq!(request.occurrence_sequence, 9);
            assert_eq!(request.occurrence_id, "subscription-1:9");
            assert_eq!(request.target_session_id, "session-1");
        }
    }

    #[test]
    fn replayed_wakeup_delivery_has_identical_occurrence_identity() {
        let delivery = delivery(WakeupSource::Timer("retry".to_owned()));
        let first = trigger_request(
            &delivery,
            binding(),
            OffsetDateTime::UNIX_EPOCH,
            OffsetDateTime::UNIX_EPOCH,
        )
        .expect("first");
        let second = trigger_request(
            &delivery,
            binding(),
            OffsetDateTime::UNIX_EPOCH,
            OffsetDateTime::UNIX_EPOCH,
        )
        .expect("second");
        assert_eq!(first.schedule_id, second.schedule_id);
        assert_eq!(first.occurrence_id, second.occurrence_id);
        assert_eq!(first.occurrence_sequence, second.occurrence_sequence);
    }
}
