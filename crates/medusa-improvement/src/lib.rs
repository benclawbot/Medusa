extern crate self as hex;

mod implementation;

pub mod behavioral_metrics;
pub mod behavioral_outcome;
pub mod correction_loop;
#[allow(clippy::obfuscated_if_else)]
pub mod correction_signals;
pub mod learning;
pub mod learning_admission;
#[path = "learning_monitor_v2.rs"]
pub mod learning_monitor;
pub mod learning_review;
pub mod lesson_inference;
pub mod meta_improvement;
pub mod provenance;
pub mod refinement_authority;
pub mod refinement_migration;
mod refinement_persistence;
pub mod regression_replay;
pub mod retrieval;
pub mod scoped_memory;
#[allow(clippy::collapsible_if)]
pub mod solution_selection;
pub mod tool_learning;
pub use implementation::*;

#[must_use]
pub fn encode(bytes: impl AsRef<[u8]>) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let bytes = bytes.as_ref();
    let mut encoded = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        encoded.push(char::from(DIGITS[usize::from(byte >> 4)]));
        encoded.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    encoded
}
