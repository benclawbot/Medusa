#[path = "lib.rs"]
mod implementation;

#[allow(clippy::obfuscated_if_else)]
pub mod correction_signals;
pub use implementation::*;
