extern crate self as hex;

mod implementation;

#[allow(clippy::obfuscated_if_else)]
pub mod correction_signals;
pub mod learning;
pub mod learning_admission;
pub mod learning_review;
pub mod lesson_inference;
pub mod refinement_authority;
mod refinement_persistence;
#[allow(clippy::expect_used)]
pub mod regression_replay;
pub mod retrieval;
pub mod scoped_memory;
#[allow(clippy::collapsible_if)]
pub mod solution_selection;
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
