//! Safe adapter from the agent policy boundary to Windows process containment.
//!
//! The adapter preserves fail-closed containment errors and reports the
//! effective Windows boundary in structured diagnostics.

use std::{path::Path, process::Output, sync::atomic::AtomicBool};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use medusa_process_containment::{
    WindowsSandboxLimits, WindowsSandboxRestrictions, run_appcontainer_cancellable_observed,
};

use super::analysis_process_tracker::AnalysisProcessTracker;

pub(crate) fn run_cancellable(
    repo: &Path,
    program: &str,
    args: &[String],
    cancellation: &AtomicBool,
) -> MedusaResult<Output> {
    let analysis = repo
        .components()
        .any(|component| component.as_os_str() == "analysis-workspace-v1");
    let limits = if analysis {
        WindowsSandboxLimits::analysis()
    } else {
        WindowsSandboxLimits::default()
    };
    let mut tracker = None;
    let result = run_appcontainer_cancellable_observed(
        repo,
        program,
        args,
        cancellation,
        limits,
        |receipt| {
            if analysis {
                tracker = Some(
                    AnalysisProcessTracker::started(repo, program, args, receipt)
                        .map_err(|error| std::io::Error::other(error.to_string()))?,
                );
            }
            Ok(())
        },
    );
    match result {
        Ok(output) => {
            if let Some(tracker) = tracker.take() {
                tracker.exited(output.status.code())?;
            }
            Ok(output)
        }
        Err(error) => {
            if let Some(tracker) = tracker.take() {
                let _ = tracker.failed(&error.to_string());
            }
            Err(if error.kind() == std::io::ErrorKind::Interrupted {
                cancelled(error)
            } else {
                unavailable(error)
            })
        }
    }
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
