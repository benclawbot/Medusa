use std::{cell::RefCell, collections::BTreeMap};

use medusa_core::MedusaResult;

use crate::ProviderRouteLatencyStore;

#[derive(Clone)]
struct PendingRouteObservation {
    completion_id: u64,
    store: ProviderRouteLatencyStore,
    index: usize,
}

#[derive(Default)]
struct RouteVerificationContext {
    next_completion_id: u64,
    latest_completion: Option<PendingRouteObservation>,
    mutation_routes: BTreeMap<String, Vec<PendingRouteObservation>>,
}

thread_local! {
    static ROUTE_VERIFICATION_CONTEXT: RefCell<RouteVerificationContext> =
        RefCell::new(RouteVerificationContext::default());
}

pub(crate) fn register_pending_route_completion(store: ProviderRouteLatencyStore, index: usize) {
    ROUTE_VERIFICATION_CONTEXT.with(|context| {
        let mut context = context.borrow_mut();
        context.next_completion_id = context.next_completion_id.saturating_add(1);
        context.latest_completion = Some(PendingRouteObservation {
            completion_id: context.next_completion_id,
            store,
            index,
        });
    });
}

/// Freezes the route completion that produced a successful repository mutation for one session.
///
/// Several mutating tool calls can originate from one model response, so each provider completion
/// is frozen at most once. Distinct mutation-producing model responses are retained independently
/// until that session reaches authoritative final verification.
#[doc(hidden)]
pub fn mark_pending_route_mutation(session_id: &str) {
    ROUTE_VERIFICATION_CONTEXT.with(|context| {
        let mut context = context.borrow_mut();
        let Some(latest) = context.latest_completion.clone() else {
            return;
        };
        let observations = context
            .mutation_routes
            .entry(session_id.to_owned())
            .or_default();
        if observations
            .iter()
            .any(|observation| observation.completion_id == latest.completion_id)
        {
            return;
        }
        observations.push(latest);
    });
}

/// Records one authoritative downstream verification result for every distinct mutation-producing
/// provider completion captured for `session_id`.
///
/// Only the named session is consumed, so an abandoned session can never leak attribution into a
/// later session sharing the same worker thread.
pub fn record_pending_route_verification(session_id: &str, passed: bool) -> MedusaResult<bool> {
    ROUTE_VERIFICATION_CONTEXT.with(|context| {
        let mut context = context.borrow_mut();
        let observations = context.mutation_routes.remove(session_id).unwrap_or_default();
        context.latest_completion = None;
        if observations.is_empty() {
            return Ok(false);
        }
        for observation in observations {
            observation
                .store
                .record_verified_success(observation.index, passed)?;
        }
        Ok(true)
    })
}

/// Clears pending attribution for a session that will not reach final verification.
#[doc(hidden)]
pub fn clear_pending_route_verification(session_id: &str) {
    ROUTE_VERIFICATION_CONTEXT.with(|context| {
        let mut context = context.borrow_mut();
        context.mutation_routes.remove(session_id);
        context.latest_completion = None;
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ProviderRouteProfile, RouteRetryPolicy};

    fn profile(id: &str) -> ProviderRouteProfile {
        ProviderRouteProfile {
            id: id.to_owned(),
            provider: "openai".to_owned(),
            model: id.to_owned(),
            protocol: "openai".to_owned(),
            endpoint: None,
            auth_source: "test".to_owned(),
            tool_calling: true,
            streaming: true,
            retry: RouteRetryPolicy::default(),
        }
    }

    #[test]
    fn verification_is_attributed_to_mutation_route_not_later_completion() {
        let profiles = vec![profile("mutation"), profile("summary")];
        let store = ProviderRouteLatencyStore::in_memory(&profiles);

        register_pending_route_completion(store.clone(), 0);
        mark_pending_route_mutation("session-a");
        register_pending_route_completion(store.clone(), 1);

        assert!(
            record_pending_route_verification("session-a", true)
                .expect("verification observation")
        );
        let stats = store.stats().expect("stats");
        assert_eq!(stats[0].verified_successes, 1);
        assert_eq!(stats[0].verified_failures, 0);
        assert_eq!(stats[1].verified_successes, 0);
    }

    #[test]
    fn multiple_mutations_from_one_completion_count_once() {
        let profiles = vec![profile("mutation")];
        let store = ProviderRouteLatencyStore::in_memory(&profiles);

        register_pending_route_completion(store.clone(), 0);
        mark_pending_route_mutation("session-a");
        mark_pending_route_mutation("session-a");
        mark_pending_route_mutation("session-a");

        assert!(
            record_pending_route_verification("session-a", true)
                .expect("verification observation")
        );
        assert_eq!(store.stats().expect("stats")[0].verified_successes, 1);
    }

    #[test]
    fn combined_verification_attributes_distinct_mutation_completions() {
        let profiles = vec![profile("first"), profile("second")];
        let store = ProviderRouteLatencyStore::in_memory(&profiles);

        register_pending_route_completion(store.clone(), 0);
        mark_pending_route_mutation("session-a");
        register_pending_route_completion(store.clone(), 1);
        mark_pending_route_mutation("session-a");

        assert!(
            record_pending_route_verification("session-a", false)
                .expect("verification observation")
        );
        let stats = store.stats().expect("stats");
        assert_eq!(stats[0].verified_failures, 1);
        assert_eq!(stats[1].verified_failures, 1);
    }

    #[test]
    fn abandoned_session_cannot_leak_into_later_session() {
        let profiles = vec![profile("abandoned"), profile("later")];
        let store = ProviderRouteLatencyStore::in_memory(&profiles);

        register_pending_route_completion(store.clone(), 0);
        mark_pending_route_mutation("session-abandoned");
        register_pending_route_completion(store.clone(), 1);
        mark_pending_route_mutation("session-later");

        assert!(
            record_pending_route_verification("session-later", true)
                .expect("later verification")
        );
        let stats = store.stats().expect("stats");
        assert_eq!(stats[0].verified_successes, 0);
        assert_eq!(stats[1].verified_successes, 1);
        clear_pending_route_verification("session-abandoned");
    }

    #[test]
    fn verification_without_committed_mutation_is_not_attributed() {
        let profiles = vec![profile("read-only")];
        let store = ProviderRouteLatencyStore::in_memory(&profiles);
        register_pending_route_completion(store.clone(), 0);

        assert!(
            !record_pending_route_verification("session-read-only", false)
                .expect("no mutation attribution")
        );
        let stats = store.stats().expect("stats")[0];
        assert_eq!(stats.verified_successes, 0);
        assert_eq!(stats.verified_failures, 0);
    }
}
