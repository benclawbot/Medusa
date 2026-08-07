//! Safe adapter from the agent policy boundary to Windows process containment.
//!
//! The adapter preserves fail-closed containment errors and reports the
//! effective Windows boundary in structured diagnostics.

use std::{path::Path, process::Output, sync::atomic::AtomicBool};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use medusa_process_containment::{
    WindowsSandboxRestrictions, run_appcontainer, run_appcontainer_cancellable,
};

pub(crate) fn run(repo: &Path, program: &str, args: &[String]) -> MedusaResult<Output> {
    run_appcontainer(repo, program, args).map_err(unavailable)
}

pub(crate) fn run_cancellable(
    repo: &Path,
    program: &str,
    args: &[String],
    cancellation: &AtomicBool,
) -> MedusaResult<Output> {
    run_appcontainer_cancellable(repo, program, args, cancellation).map_err(|error| {
        if error.kind() == std::io::ErrorKind::Interrupted {
            cancelled(error)
        } else {
            unavailable(error)
        }
    })
}

fn cancelled(error: std::io::Error) -> MedusaError {
    let mut result = MedusaError::new(
        ErrorCode::ToolExecutionFailed,
        ErrorCategory::Execution,
        error.to_string(),
    );
    result
        .context
        .insert("cancelled".into(), serde_json::Value::Bool(true));
    result
}

fn unavailable(error: std::io::Error) -> MedusaError {
    let restrictions = WindowsSandboxRestrictions::default();
    let mut result = MedusaError::new(
        ErrorCode::SandboxUnavailable,
        ErrorCategory::Environment,
        format!("Windows composable sandbox unavailable: {error}"),
    );
    result.context.insert(
        "sandbox_backend".into(),
        serde_json::Value::String(restrictions.backend.into()),
    );
    result.context.insert(
        "effective_restrictions".into(),
        serde_json::json!(restrictions.restrictions),
    );
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unavailable_errors_report_the_effective_boundary() {
        let error = unavailable(std::io::Error::other("fixture"));
        assert_eq!(error.code, ErrorCode::SandboxUnavailable);
        assert_eq!(
            error.context.get("sandbox_backend"),
            Some(&serde_json::Value::String("windows_base_container".into()))
        );
        assert!(
            error
                .context
                .get("effective_restrictions")
                .is_some_and(|value| value.to_string().contains("network_denied"))
        );
    }
}
