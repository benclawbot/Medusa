//! Shared learning admission authority.
//!
//! The policy lives in `medusa-core` so low-level session persistence and frontend-neutral runtime
//! retrieval use exactly the same fail-closed interpretation without creating dependency cycles.

pub use medusa_core::learning_policy::{LearningAdmissionPolicy, LearningPrivacyPolicy};
