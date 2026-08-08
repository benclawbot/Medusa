use std::cell::RefCell;

use medusa_core::MedusaResult;

use crate::ProviderRouteLatencyStore;

#[derive(Clone)]
struct PendingRouteObservation {
    store: ProviderRouteLatencyStore,
    index: usize,
}

#[derive(Default)]
struct RouteVerificationContext {
    latest_completion: Option<PendingRouteObservation>,
    mutation_route: Option<PendingRouteObservation>,
}

thread_local! {
    static ROUTE_VERIFICATION_CONTEXT: RefCell<RouteVerificationContext> =
        RefCell::new(RouteVerificationContext::default());
}

pub(crate) fn register_pending_route_completion(store: ProviderRouteLatencyStore, index: usize) {
    ROUTE_VERIFICATION_CONTEXT.with(|context| {
        context.borrow_mut().latest_completion = Some(PendingRouteObservation { store, index });
    });
}

/// Freezes the route that most recently completed when a repository mutation is committed.
///
/// Later model calls may complete before final verification (for example, a summarization turn),
/// so verification attribution must retain the route that actually produced the mutation rather
/// than whichever route happened to answer last.
#[doc(hidden)]
pub fn mark_pending_route_mutation() {
    ROUTE_VERIFICATION_CONTEXT.with(|context| {
        let mut context = context.borrow_mut();
        if let Some(latest) = context.latest_completion.clone() {
            context.mutation_route = Some(latest);
        }
    });
}

/// Records one authoritative downstream verification result for the route that produced the
/// mutation under verification.
///
/// Returns `true` when an attributed route observation was consumed. The context is one-shot: the
/// frozen mutation route and latest completion are cleared after every final verification event so
/// stale attribution cannot leak into a later turn.
pub fn record_pending_route_verification(passed: bool) -> MedusaResult<bool> {
    ROUTE_VERIFICATION_CONTEXT.with(|context| {
        let mut context = context.borrow_mut();
        let observation = context.mutation_route.take();
        context.latest_completion = None;
        let Some(observation) = observation else {
            return Ok(false);
        };
        observation
            .store
            .record_verified_success(observation.index, passed)?;
        Ok(true)
    })
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
        mark_pending_route_mutation();
        register_pending_route_completion(store.clone(), 1);

        assert!(record_pending_route_verification(true).expect("verification observation"));
        let stats = store.stats().expect("stats");
        assert_eq!(stats[0].verified_successes, 1);
        assert_eq!(stats[0].verified_failures, 0);
        assert_eq!(stats[1].verified_successes, 0);
    }

    #[test]
    fn verification_without_committed_mutation_is_not_attributed() {
        let profiles = vec![profile("read-only")];
        let store = ProviderRouteLatencyStore::in_memory(&profiles);
        register_pending_route_completion(store.clone(), 0);

        assert!(!record_pending_route_verification(false).expect("no mutation attribution"));
        let stats = store.stats().expect("stats")[0];
        assert_eq!(stats.verified_successes, 0);
        assert_eq!(stats.verified_failures, 0);
    }
}
