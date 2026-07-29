mod implementation;

#[allow(clippy::obfuscated_if_else)]
pub mod correction_signals;
pub mod lesson_inference;
pub mod scoped_memory;
#[allow(clippy::collapsible_if)]
pub mod solution_selection;
pub use implementation::*;
