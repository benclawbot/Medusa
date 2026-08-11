//! Deterministic resilience fixtures shared by Medusa certification tests.
//!
//! This module intentionally avoids ambient randomness. A failure can always be
//! reproduced from the recorded seed, case index, and fault point.

/// Stable seeds used by bounded pull-request resilience smoke tests.
pub const SMOKE_SEEDS: [u64; 8] = [
    0x0000_0000_0000_0001,
    0x0123_4567_89ab_cdef,
    0x5eed_5eed_5eed_5eed,
    0x8000_0000_0000_0001,
    0xa5a5_a5a5_5a5a_5a5a,
    0xcafe_f00d_dead_beef,
    0xfedc_ba98_7654_3210,
    0xffff_ffff_ffff_ffff,
];

/// Authoritative boundaries where crash/fault injection is useful.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum FaultPoint {
    BeforeDurableAppend,
    AfterDurableAppend,
    BeforeSync,
    AfterSync,
    BeforeEventPublish,
    AfterEventPublish,
    BeforeProcessSpawn,
    AfterProcessRegistration,
    DuringCancellation,
    BeforeCandidatePromotion,
    AfterCandidatePromotion,
    BeforeVerificationReceipt,
    AfterVerificationReceipt,
    BeforeSnapshotPersist,
    AfterSnapshotPersist,
    BeforeActionDelivery,
    AfterActionDelivery,
}

impl FaultPoint {
    /// Stable numeric discriminator suitable for deterministic seed mixing.
    #[must_use]
    pub const fn discriminator(self) -> u64 {
        match self {
            Self::BeforeDurableAppend => 1,
            Self::AfterDurableAppend => 2,
            Self::BeforeSync => 3,
            Self::AfterSync => 4,
            Self::BeforeEventPublish => 5,
            Self::AfterEventPublish => 6,
            Self::BeforeProcessSpawn => 7,
            Self::AfterProcessRegistration => 8,
            Self::DuringCancellation => 9,
            Self::BeforeCandidatePromotion => 10,
            Self::AfterCandidatePromotion => 11,
            Self::BeforeVerificationReceipt => 12,
            Self::AfterVerificationReceipt => 13,
            Self::BeforeSnapshotPersist => 14,
            Self::AfterSnapshotPersist => 15,
            Self::BeforeActionDelivery => 16,
            Self::AfterActionDelivery => 17,
        }
    }
}

/// A deterministic fault schedule.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FaultPlan {
    seed: u64,
    fail_every: u64,
}

impl FaultPlan {
    /// Builds a reproducible schedule. `fail_every` is clamped to at least one.
    #[must_use]
    pub const fn new(seed: u64, fail_every: u64) -> Self {
        Self {
            seed,
            fail_every: if fail_every == 0 { 1 } else { fail_every },
        }
    }

    /// Seed that must be recorded with a failing case.
    #[must_use]
    pub const fn seed(&self) -> u64 {
        self.seed
    }

    /// Returns whether the requested invocation should inject a failure.
    ///
    /// The result depends only on `(seed, fault point, invocation)`, so test
    /// scheduling and wall-clock timing cannot change the selected case.
    #[must_use]
    pub fn injects(&self, point: FaultPoint, invocation: u64) -> bool {
        splitmix64(
            self.seed
                ^ point.discriminator().wrapping_mul(0x9e37_79b9_7f4a_7c15)
                ^ invocation.wrapping_mul(0xbf58_476d_1ce4_e5b9),
        ) % self.fail_every
            == 0
    }
}

/// Produces bounded deterministic corruptions of an input buffer.
///
/// Cases include truncation, a single-bit flip, and one inserted byte. The
/// input is never mutated and each returned case is capped at `max_len`.
#[must_use]
pub fn corruption_cases(input: &[u8], seed: u64, max_len: usize) -> Vec<Vec<u8>> {
    let cap = max_len.max(1);
    let bounded = &input[..input.len().min(cap)];
    let mut cases = Vec::with_capacity(4);

    cases.push(bounded[..bounded.len() / 2].to_vec());
    cases.push(bounded[..bounded.len().saturating_sub(1)].to_vec());

    if !bounded.is_empty() {
        let mut flipped = bounded.to_vec();
        let mixed = splitmix64(seed);
        let index = (mixed as usize) % flipped.len();
        let bit = 1_u8 << ((mixed >> 8) % 8);
        flipped[index] ^= bit;
        cases.push(flipped);
    }

    let mut inserted = bounded.to_vec();
    if inserted.len() < cap {
        let mixed = splitmix64(seed ^ 0xa076_1d64_78bd_642f);
        let index = (mixed as usize) % (inserted.len() + 1);
        inserted.insert(index, (mixed >> 16) as u8);
        cases.push(inserted);
    }

    cases
}

#[must_use]
fn splitmix64(mut value: u64) -> u64 {
    value = value.wrapping_add(0x9e37_79b9_7f4a_7c15);
    value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^ (value >> 31)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fault_plan_is_reproducible() {
        for seed in SMOKE_SEEDS {
            let left = FaultPlan::new(seed, 5);
            let right = FaultPlan::new(seed, 5);
            for invocation in 0..128 {
                assert_eq!(
                    left.injects(FaultPoint::BeforeDurableAppend, invocation),
                    right.injects(FaultPoint::BeforeDurableAppend, invocation)
                );
            }
        }
    }

    #[test]
    fn fault_points_have_unique_discriminators() {
        let points = [
            FaultPoint::BeforeDurableAppend,
            FaultPoint::AfterDurableAppend,
            FaultPoint::BeforeSync,
            FaultPoint::AfterSync,
            FaultPoint::BeforeEventPublish,
            FaultPoint::AfterEventPublish,
            FaultPoint::BeforeProcessSpawn,
            FaultPoint::AfterProcessRegistration,
            FaultPoint::DuringCancellation,
            FaultPoint::BeforeCandidatePromotion,
            FaultPoint::AfterCandidatePromotion,
            FaultPoint::BeforeVerificationReceipt,
            FaultPoint::AfterVerificationReceipt,
            FaultPoint::BeforeSnapshotPersist,
            FaultPoint::AfterSnapshotPersist,
            FaultPoint::BeforeActionDelivery,
            FaultPoint::AfterActionDelivery,
        ];
        let mut discriminators: Vec<_> =
            points.into_iter().map(FaultPoint::discriminator).collect();
        discriminators.sort_unstable();
        discriminators.dedup();
        assert_eq!(discriminators.len(), points.len());
    }

    #[test]
    fn corruptions_are_bounded_and_reproducible() {
        let input = b"0123456789abcdef";
        for seed in SMOKE_SEEDS {
            let first = corruption_cases(input, seed, 12);
            let second = corruption_cases(input, seed, 12);
            assert_eq!(first, second);
            assert!(first.iter().all(|case| case.len() <= 12));
            assert!(first.iter().all(|case| case.as_slice() != input));
        }
    }
}
