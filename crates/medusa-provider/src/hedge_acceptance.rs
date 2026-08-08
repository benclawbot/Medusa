use serde::{Deserialize, Serialize};

/// Minimum evidence required before a hedge p95 comparison is authoritative.
pub const HEDGE_ACCEPTANCE_MIN_SAMPLES: usize = 20;

/// Deterministic acceptance result for an injected tail-latency hedge benchmark.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct HedgeLatencyAcceptance {
    pub passed: bool,
    pub baseline_samples: usize,
    pub hedged_samples: usize,
    pub baseline_p95_ms: Option<u64>,
    pub hedged_p95_ms: Option<u64>,
    /// Relative p95 reduction in basis points (10_000 = 100%), when measurable.
    pub improvement_bps: Option<u64>,
    pub reasons: Vec<String>,
}

/// Enforces the #688 tail-latency acceptance criterion using comparable measured samples.
///
/// The gate is deliberately independent of wall-clock benchmarking so the same measured fixture
/// data can be evaluated deterministically across CI platforms. Callers must supply baseline and
/// hedged completion-time samples from the same injected latency scenario. The gate fails closed
/// when the sample sets are too small or have different cardinality, and passes only when hedging
/// strictly lowers nearest-rank p95.
#[must_use]
pub fn assess_hedge_latency_acceptance(
    baseline_ms: &[u64],
    hedged_ms: &[u64],
) -> HedgeLatencyAcceptance {
    let mut reasons = Vec::new();
    if baseline_ms.len() < HEDGE_ACCEPTANCE_MIN_SAMPLES {
        reasons.push(format!(
            "baseline requires at least {HEDGE_ACCEPTANCE_MIN_SAMPLES} samples"
        ));
    }
    if hedged_ms.len() < HEDGE_ACCEPTANCE_MIN_SAMPLES {
        reasons.push(format!(
            "hedged run requires at least {HEDGE_ACCEPTANCE_MIN_SAMPLES} samples"
        ));
    }
    if baseline_ms.len() != hedged_ms.len() {
        reasons.push("baseline and hedged runs must contain the same number of samples".to_owned());
    }

    let baseline_p95_ms = percentile_95(baseline_ms);
    let hedged_p95_ms = percentile_95(hedged_ms);
    let improvement_bps = baseline_p95_ms.zip(hedged_p95_ms).and_then(|(baseline, hedged)| {
        if baseline == 0 || hedged >= baseline {
            return None;
        }
        Some(baseline.saturating_sub(hedged).saturating_mul(10_000) / baseline)
    });

    if reasons.is_empty() {
        match (baseline_p95_ms, hedged_p95_ms) {
            (Some(baseline), Some(hedged)) if hedged < baseline => {}
            (Some(baseline), Some(hedged)) => reasons.push(format!(
                "hedged p95 {hedged} ms did not improve baseline p95 {baseline} ms"
            )),
            _ => reasons.push("p95 could not be measured from the supplied samples".to_owned()),
        }
    }

    HedgeLatencyAcceptance {
        passed: reasons.is_empty(),
        baseline_samples: baseline_ms.len(),
        hedged_samples: hedged_ms.len(),
        baseline_p95_ms,
        hedged_p95_ms,
        improvement_bps,
        reasons,
    }
}

fn percentile_95(samples: &[u64]) -> Option<u64> {
    if samples.is_empty() {
        return None;
    }
    let mut ordered = samples.to_vec();
    ordered.sort_unstable();
    // Nearest-rank percentile: ceil(0.95 * N), converted to a zero-based index.
    let rank = ordered.len().saturating_mul(95).div_ceil(100).max(1);
    ordered.get(rank - 1).copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn injected_tail_latency_fixture_must_lower_p95() {
        let mut baseline = vec![40; 19];
        baseline.push(200);
        let mut hedged = vec![40; 19];
        hedged.push(80);

        let assessment = assess_hedge_latency_acceptance(&baseline, &hedged);
        assert!(assessment.passed, "{:?}", assessment.reasons);
        assert_eq!(assessment.baseline_p95_ms, Some(40));
        assert_eq!(assessment.hedged_p95_ms, Some(40));
    }

    #[test]
    fn acceptance_rejects_no_p95_improvement() {
        let baseline = vec![100; HEDGE_ACCEPTANCE_MIN_SAMPLES];
        let hedged = vec![100; HEDGE_ACCEPTANCE_MIN_SAMPLES];
        let assessment = assess_hedge_latency_acceptance(&baseline, &hedged);
        assert!(!assessment.passed);
        assert_eq!(assessment.improvement_bps, None);
    }

    #[test]
    fn acceptance_fails_closed_for_insufficient_or_unpaired_evidence() {
        let insufficient = assess_hedge_latency_acceptance(&[100; 4], &[50; 4]);
        assert!(!insufficient.passed);
        assert!(insufficient.reasons.len() >= 2);

        let mismatched = assess_hedge_latency_acceptance(
            &[100; HEDGE_ACCEPTANCE_MIN_SAMPLES],
            &[50; HEDGE_ACCEPTANCE_MIN_SAMPLES + 1],
        );
        assert!(!mismatched.passed);
        assert!(
            mismatched
                .reasons
                .iter()
                .any(|reason| reason.contains("same number"))
        );
    }

    #[test]
    fn nearest_rank_p95_captures_tail_fixture() {
        let mut samples = vec![10; 19];
        samples.push(500);
        assert_eq!(percentile_95(&samples), Some(10));

        let mut samples = vec![10; 18];
        samples.extend([400, 500]);
        assert_eq!(percentile_95(&samples), Some(400));
    }
}
