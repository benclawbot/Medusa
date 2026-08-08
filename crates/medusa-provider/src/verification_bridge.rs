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
    mutation_routes: Vec<PendingRouteObservation>,
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

/// Freezes the route that most recently completed when one of its tool calls commits a repository
/// mutation.
///
/// Later model calls may complete before final verification, and several mutation-producing model
/// turns may contribute to one combined verification. Each committed mutation therefore captures
/// the route completion that caused it instead of relying on whichever route happened to answer
/// last at verification time.
#[doc(hidden)]
pub fn mark_pending_route_mutation() {
    ROUTE_VERIFICATION_CONTEXT.with(|context| {
        let mut context = context.borrow_mut();
        if let Some(latest) = context.latest_completion.clone() {
            context.mutation_routes.push(latest);
        }
    });
}

/// Records one authoritative downstream verification result for every mutation-producing route
/// completion captured since the previous final verification.
///
/// Returns `true` when at least one attributed route observation was consumed. The context is
/// one-shot: all frozen mutation routes and the latest completion are cleared after every final
/// verification event so stale attribution cannot leak into a later turn.
pub fn record_pending_route_verification(passed: bool) -> MedusaResult<bool> {
    ROUTE_VERIFICATION_CONTEXT.with(|context| {
        let mut context = context.borrow_mut();
        let observations = std::mem::take(&mut context.mutation_routes);
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
    fn combined_verification_attributes_each_mutation_producing_completion() {
        let profiles = vec![profile("first"), profile("second")];
        let store = ProviderRouteLatencyStore::in_memory(&profiles);

        register_pending_route_completion(store.clone(), 0);
        mark_pending_route_mutation();
        register_pending_route_completion(store.clone(), 1);
        mark_pending_route_mutation();

        assert!(record_pending_route_verification(false).expect("verification observation"));
        let stats = store.stats().expect("stats");
        assert_eq!(stats[0].verified_failures, 1);
        assert_eq!(stats[1].verified_failures, 1);
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
