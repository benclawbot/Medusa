mod implementation;

#[allow(clippy::obfuscated_if_else)]
pub mod correction_signals;
pub mod lesson_inference;
#[allow(clippy::collapsible_if)]
pub mod solution_selection;
pub use implementation::*;
