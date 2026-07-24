//! Event-driven wakeup routing for durable Medusa work.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

#[derive(Clone, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct SubscriptionId(String);

impl SubscriptionId {
    pub fn parse(value: impl Into<String>) -> Result<Self, &'static str> {
        let value = value.into();
        let trimmed = value.trim();
        if trimmed.is_empty() {
            return Err("subscription id cannot be empty");
        }
        if trimmed.len() > 128 {
            return Err("subscription id is too long");
        }
        Ok(Self(trimmed.to_owned()))
    }
}

#[derive(Clone, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "value")]
pub enum WakeupSource {
    ProcessExited(String),
    ProcessOrphaned(String),
    HeartbeatStale(String),
    FileChanged(String),
    Timer(String),
    ExternalSignal(String),
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct WakeupSubscription {
    pub id: SubscriptionId,
    pub owner: String,
    pub source: WakeupSource,
    #[serde(default)]
    pub one_shot: bool,
    #[serde(default)]
    pub enabled: bool,
}

impl WakeupSubscription {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.owner.trim().is_empty() {
            return Err("subscription owner cannot be empty");
        }
        match &self.source {
            WakeupSource::ProcessExited(v)
            | WakeupSource::ProcessOrphaned(v)
            | WakeupSource::HeartbeatStale(v)
            | WakeupSource::FileChanged(v)
            | WakeupSource::Timer(v)
            | WakeupSource::ExternalSignal(v) if v.trim().is_empty() => {
                Err("wakeup source value cannot be empty")
            }
            _ => Ok(()),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct WakeupEvent {
    pub sequence: u64,
    #[serde(with = "time::serde::rfc3339")]
    pub occurred_at: OffsetDateTime,
    pub source: WakeupSource,
    #[serde(default)]
    pub metadata: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct WakeupDelivery {
    pub event_sequence: u64,
    pub subscription_id: SubscriptionId,
    pub owner: String,
    pub source: WakeupSource,
    pub metadata: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct WakeupRouter {
    #[serde(default)]
    subscriptions: BTreeMap<SubscriptionId, WakeupSubscription>,
    #[serde(default)]
    delivered_events: BTreeSet<u64>,
    #[serde(default)]
    last_sequence: Option<u64>,
}

impl WakeupRouter {
    pub fn subscribe(&mut self, subscription: WakeupSubscription) -> Result<(), &'static str> {
        subscription.validate()?;
        if self.subscriptions.contains_key(&subscription.id) {
            return Err("duplicate wakeup subscription id");
        }
        self.subscriptions.insert(subscription.id.clone(), subscription);
        Ok(())
    }

    pub fn set_enabled(&mut self, id: &SubscriptionId, enabled: bool) -> Result<(), &'static str> {
        let subscription = self
            .subscriptions
            .get_mut(id)
            .ok_or("wakeup subscription not found")?;
        subscription.enabled = enabled;
        Ok(())
    }

    pub fn route(&mut self, event: WakeupEvent) -> Result<Vec<WakeupDelivery>, &'static str> {
        let expected = self.last_sequence.map_or(1, |value| value.saturating_add(1));
        if event.sequence != expected {
            return Err("wakeup event sequence must be contiguous and start at one");
        }
        if self.delivered_events.contains(&event.sequence) {
            return Err("wakeup event was already delivered");
        }

        let mut deliveries = Vec::new();
        let mut disable = Vec::new();
        for subscription in self.subscriptions.values() {
            if subscription.enabled && subscription.source == event.source {
                deliveries.push(WakeupDelivery {
                    event_sequence: event.sequence,
                    subscription_id: subscription.id.clone(),
                    owner: subscription.owner.clone(),
                    source: event.source.clone(),
                    metadata: event.metadata.clone(),
                });
                if subscription.one_shot {
                    disable.push(subscription.id.clone());
                }
            }
        }
        for id in disable {
            if let Some(subscription) = self.subscriptions.get_mut(&id) {
                subscription.enabled = false;
            }
        }
        self.delivered_events.insert(event.sequence);
        self.last_sequence = Some(event.sequence);
        Ok(deliveries)
    }

    pub fn validate(&self) -> Result<(), &'static str> {
        for (id, subscription) in &self.subscriptions {
            if id != &subscription.id {
                return Err("subscription map key does not match record id");
            }
            subscription.validate()?;
        }
        if let Some(last) = self.last_sequence {
            if !self.delivered_events.contains(&last) {
                return Err("last sequence is not present in delivered event set");
            }
            if self.delivered_events.iter().copied().ne(1..=last) {
                return Err("delivered event sequence contains gaps");
            }
        } else if !self.delivered_events.is_empty() {
            return Err("delivered events exist without a last sequence");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    fn id(value: &str) -> SubscriptionId {
        SubscriptionId::parse(value).expect("valid id")
    }

    fn subscription(id_value: &str, source: WakeupSource, one_shot: bool) -> WakeupSubscription {
        WakeupSubscription {
            id: id(id_value),
            owner: "session-1".to_owned(),
            source,
            one_shot,
            enabled: true,
        }
    }

    fn event(sequence: u64, source: WakeupSource) -> WakeupEvent {
        WakeupEvent {
            sequence,
            occurred_at: datetime!(2026-07-24 12:00 UTC),
            source,
            metadata: BTreeMap::new(),
        }
    }

    #[test]
    fn process_exit_wakes_matching_owner() {
        let mut router = WakeupRouter::default();
        router
            .subscribe(subscription(
                "exit-build",
                WakeupSource::ProcessExited("build".to_owned()),
                false,
            ))
            .expect("subscribe");
        let deliveries = router
            .route(event(
                1,
                WakeupSource::ProcessExited("build".to_owned()),
            ))
            .expect("route");
        assert_eq!(deliveries.len(), 1);
        assert_eq!(deliveries[0].owner, "session-1");
    }

    #[test]
    fn unrelated_events_do_not_wake_owner() {
        let mut router = WakeupRouter::default();
        router
            .subscribe(subscription(
                "exit-build",
                WakeupSource::ProcessExited("build".to_owned()),
                false,
            ))
            .expect("subscribe");
        assert!(router
            .route(event(1, WakeupSource::Timer("retry".to_owned())))
            .expect("route")
            .is_empty());
    }

    #[test]
    fn one_shot_subscription_disables_after_delivery() {
        let source = WakeupSource::ExternalSignal("approval".to_owned());
        let mut router = WakeupRouter::default();
        router
            .subscribe(subscription("approval", source.clone(), true))
            .expect("subscribe");
        assert_eq!(router.route(event(1, source.clone())).expect("route").len(), 1);
        assert!(router.route(event(2, source)).expect("route").is_empty());
    }

    #[test]
    fn sequence_gaps_are_rejected() {
        let mut router = WakeupRouter::default();
        assert_eq!(
            router.route(event(2, WakeupSource::Timer("retry".to_owned()))),
            Err("wakeup event sequence must be contiguous and start at one")
        );
    }

    #[test]
    fn duplicate_subscription_ids_are_rejected() {
        let source = WakeupSource::Timer("retry".to_owned());
        let mut router = WakeupRouter::default();
        router
            .subscribe(subscription("retry", source.clone(), false))
            .expect("subscribe");
        assert_eq!(
            router.subscribe(subscription("retry", source, false)),
            Err("duplicate wakeup subscription id")
        );
    }
}
