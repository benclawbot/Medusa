//! Sanitized diagnostics shared by the Windows contained-process boundary.

use std::{fmt, io};

/// The native stage at which a contained Windows launch failed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WindowsSandboxLaunchStage {
    ApiLoad,
    EnvironmentBlock,
    ProcessCreation,
}

impl WindowsSandboxLaunchStage {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ApiLoad => "api_load",
            Self::EnvironmentBlock => "environment_block",
            Self::ProcessCreation => "process_creation",
        }
    }
}

impl fmt::Display for WindowsSandboxLaunchStage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// A stable, sanitized classification for a native Windows launch failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WindowsSandboxLaunchFailureCategory {
    ApiUnavailable,
    EnvironmentBlockInvalid,
    EnvironmentBlockRejected,
    AccessDenied,
    InvalidParameters,
    ExecutableUnavailable,
    NativeFailure,
}

impl WindowsSandboxLaunchFailureCategory {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ApiUnavailable => "api_unavailable",
            Self::EnvironmentBlockInvalid => "environment_block_invalid",
            Self::EnvironmentBlockRejected => "environment_block_rejected",
            Self::AccessDenied => "access_denied",
            Self::InvalidParameters => "invalid_parameters",
            Self::ExecutableUnavailable => "executable_unavailable",
            Self::NativeFailure => "native_failure",
        }
    }

    #[must_use]
    const fn error_kind(self) -> io::ErrorKind {
        match self {
            Self::ApiUnavailable
            | Self::EnvironmentBlockInvalid
            | Self::EnvironmentBlockRejected => io::ErrorKind::Unsupported,
            Self::AccessDenied => io::ErrorKind::PermissionDenied,
            Self::InvalidParameters => io::ErrorKind::InvalidInput,
            Self::ExecutableUnavailable => io::ErrorKind::NotFound,
            Self::NativeFailure => io::ErrorKind::Other,
        }
    }
}

impl fmt::Display for WindowsSandboxLaunchFailureCategory {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Sanitized evidence retained when the contained Windows launch cannot start.
///
/// The display form intentionally contains only stable identifiers and the
/// numeric native error. It never includes a command, path, environment value,
/// or localized operating-system error text.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WindowsSandboxLaunchDiagnostic {
    pub stage: WindowsSandboxLaunchStage,
    pub native_error: Option<i32>,
    pub category: WindowsSandboxLaunchFailureCategory,
    pub support_requirement: &'static str,
    pub remediation: &'static str,
}

impl WindowsSandboxLaunchDiagnostic {
    #[must_use]
    pub const fn from_native_error(
        stage: WindowsSandboxLaunchStage,
        native_error: Option<i32>,
    ) -> Self {
        let category = classify_windows_sandbox_failure(stage, native_error);
        Self::for_category(stage, native_error, category)
    }

    #[must_use]
    pub const fn executable_unavailable(stage: WindowsSandboxLaunchStage) -> Self {
        Self::for_category(
            stage,
            None,
            WindowsSandboxLaunchFailureCategory::ExecutableUnavailable,
        )
    }

    #[must_use]
    pub const fn environment_block_invalid() -> Self {
        Self::for_category(
            WindowsSandboxLaunchStage::EnvironmentBlock,
            None,
            WindowsSandboxLaunchFailureCategory::EnvironmentBlockInvalid,
        )
    }

    #[must_use]
    const fn for_category(
        stage: WindowsSandboxLaunchStage,
        native_error: Option<i32>,
        category: WindowsSandboxLaunchFailureCategory,
    ) -> Self {
        let (support_requirement, remediation) = match category {
            WindowsSandboxLaunchFailureCategory::ApiUnavailable => (
                "windows_11_processmodel_sandbox",
                "use_a_supported_windows_build",
            ),
            WindowsSandboxLaunchFailureCategory::EnvironmentBlockInvalid => (
                "validated_isolated_environment_block",
                "repair_the_allowlisted_environment_block",
            ),
            WindowsSandboxLaunchFailureCategory::EnvironmentBlockRejected => (
                "validated_isolated_environment_block",
                "use_a_windows_build_that_accepts_the_environment_block",
            ),
            WindowsSandboxLaunchFailureCategory::AccessDenied => (
                "windows_sandbox_capability",
                "grant_the_required_sandbox_capability",
            ),
            WindowsSandboxLaunchFailureCategory::InvalidParameters => (
                "windows_sandbox_launch_contract",
                "correct_the_contained_launch_parameters",
            ),
            WindowsSandboxLaunchFailureCategory::ExecutableUnavailable => (
                "contained_executable_available",
                "install_the_requested_executable",
            ),
            WindowsSandboxLaunchFailureCategory::NativeFailure => (
                "windows_sandbox_launch_contract",
                "inspect_the_numeric_native_error_without_unrestricted_fallback",
            ),
        };
        Self {
            stage,
            native_error,
            category,
            support_requirement,
            remediation,
        }
    }

    #[must_use]
    pub fn into_io_error(self) -> io::Error {
        io::Error::new(self.category.error_kind(), self)
    }
}

impl fmt::Display for WindowsSandboxLaunchDiagnostic {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let native_error = self
            .native_error
            .map_or_else(|| "none".to_owned(), |error| error.to_string());
        write!(
            formatter,
            "windows_sandbox_launch_failure{{stage={},native_error={},category={},support_requirement={},remediation={},fallback=none}}",
            self.stage, native_error, self.category, self.support_requirement, self.remediation,
        )
    }
}

impl std::error::Error for WindowsSandboxLaunchDiagnostic {}

/// Maps a native Windows error code without exposing native text or paths.
#[must_use]
pub const fn classify_windows_sandbox_failure(
    stage: WindowsSandboxLaunchStage,
    native_error: Option<i32>,
) -> WindowsSandboxLaunchFailureCategory {
    match native_error {
        None if matches!(stage, WindowsSandboxLaunchStage::ApiLoad) => {
            WindowsSandboxLaunchFailureCategory::ApiUnavailable
        }
        Some(120) => WindowsSandboxLaunchFailureCategory::ApiUnavailable,
        Some(203) => WindowsSandboxLaunchFailureCategory::EnvironmentBlockRejected,
        Some(5) => WindowsSandboxLaunchFailureCategory::AccessDenied,
        Some(87) => WindowsSandboxLaunchFailureCategory::InvalidParameters,
        Some(2 | 3 | 126 | 127 | 193 | 1157)
            if matches!(stage, WindowsSandboxLaunchStage::ApiLoad) =>
        {
            WindowsSandboxLaunchFailureCategory::ApiUnavailable
        }
        Some(2 | 3 | 126 | 193) => WindowsSandboxLaunchFailureCategory::ExecutableUnavailable,
        _ => WindowsSandboxLaunchFailureCategory::NativeFailure,
    }
}

/// Structural validation errors for an isolated Windows environment block.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WindowsEnvironmentBlockError {
    MissingDoubleTerminator,
    EmptyRecord,
    InvalidUtf16,
    MissingEquals,
    EmptyName,
    NotSorted,
}

impl fmt::Display for WindowsEnvironmentBlockError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let value = match self {
            Self::MissingDoubleTerminator => "missing_double_terminator",
            Self::EmptyRecord => "empty_record",
            Self::InvalidUtf16 => "invalid_utf16",
            Self::MissingEquals => "missing_equals",
            Self::EmptyName => "empty_name",
            Self::NotSorted => "not_sorted",
        };
        formatter.write_str(value)
    }
}

impl std::error::Error for WindowsEnvironmentBlockError {}

/// Validates the UTF-16 structure required by `CreateProcess` environment blocks.
///
/// Records must be `NAME=VALUE`, sorted case-insensitively by name, and ended by
/// an additional NUL. This function is platform-independent so malformed
/// blocks can be rejected and tested without a Windows host.
pub fn validate_windows_environment_block(
    block: &[u16],
) -> Result<(), WindowsEnvironmentBlockError> {
    if block.len() < 2 || block[block.len() - 2..] != [0, 0] {
        return Err(WindowsEnvironmentBlockError::MissingDoubleTerminator);
    }
    if block == [0, 0] {
        return Ok(());
    }

    let mut start = 0usize;
    let mut previous_name = None::<String>;
    for end in 0..block.len() - 1 {
        if block[end] != 0 {
            continue;
        }
        if end == start {
            return Err(WindowsEnvironmentBlockError::EmptyRecord);
        }
        let record = String::from_utf16(&block[start..end])
            .map_err(|_| WindowsEnvironmentBlockError::InvalidUtf16)?;
        let Some(equals) = record.find('=') else {
            return Err(WindowsEnvironmentBlockError::MissingEquals);
        };
        if equals == 0 {
            return Err(WindowsEnvironmentBlockError::EmptyName);
        }
        let name = record[..equals].to_ascii_uppercase();
        if previous_name
            .as_ref()
            .is_some_and(|previous| previous >= &name)
        {
            return Err(WindowsEnvironmentBlockError::NotSorted);
        }
        previous_name = Some(name);
        start = end + 1;
    }
    if start != block.len() - 1 {
        return Err(WindowsEnvironmentBlockError::MissingDoubleTerminator);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(records: &[&str]) -> Vec<u16> {
        records
            .iter()
            .flat_map(|record| record.encode_utf16().chain([0]))
            .chain([0])
            .collect()
    }

    #[test]
    fn maps_native_errors_to_sanitized_categories() {
        assert_eq!(
            classify_windows_sandbox_failure(WindowsSandboxLaunchStage::ApiLoad, Some(120)),
            WindowsSandboxLaunchFailureCategory::ApiUnavailable
        );
        assert_eq!(
            classify_windows_sandbox_failure(WindowsSandboxLaunchStage::ApiLoad, None),
            WindowsSandboxLaunchFailureCategory::ApiUnavailable
        );
        assert_eq!(
            classify_windows_sandbox_failure(
                WindowsSandboxLaunchStage::EnvironmentBlock,
                Some(203)
            ),
            WindowsSandboxLaunchFailureCategory::EnvironmentBlockRejected
        );
        assert_eq!(
            classify_windows_sandbox_failure(WindowsSandboxLaunchStage::ProcessCreation, Some(5)),
            WindowsSandboxLaunchFailureCategory::AccessDenied
        );
        assert_eq!(
            classify_windows_sandbox_failure(WindowsSandboxLaunchStage::ProcessCreation, Some(87)),
            WindowsSandboxLaunchFailureCategory::InvalidParameters
        );
        assert_eq!(
            classify_windows_sandbox_failure(
                WindowsSandboxLaunchStage::ProcessCreation,
                Some(0xDEAD)
            ),
            WindowsSandboxLaunchFailureCategory::NativeFailure
        );
        assert_eq!(
            classify_windows_sandbox_failure(WindowsSandboxLaunchStage::ApiLoad, Some(126)),
            WindowsSandboxLaunchFailureCategory::ApiUnavailable
        );
        assert_eq!(
            classify_windows_sandbox_failure(WindowsSandboxLaunchStage::ProcessCreation, Some(2)),
            WindowsSandboxLaunchFailureCategory::ExecutableUnavailable
        );
        assert_eq!(
            classify_windows_sandbox_failure(WindowsSandboxLaunchStage::ProcessCreation, Some(193)),
            WindowsSandboxLaunchFailureCategory::ExecutableUnavailable
        );
    }

    #[test]
    fn diagnostic_display_is_sanitized_and_declares_no_fallback() {
        let diagnostic = WindowsSandboxLaunchDiagnostic::from_native_error(
            WindowsSandboxLaunchStage::ProcessCreation,
            Some(5),
        );
        let text = diagnostic.to_string();
        assert!(text.contains("stage=process_creation"));
        assert!(text.contains("native_error=5"));
        assert!(text.contains("category=access_denied"));
        assert!(text.contains("remediation=grant_the_required_sandbox_capability"));
        assert!(text.contains("fallback=none"));
        assert!(!text.contains("node"));
        assert!(!text.contains("git"));
        assert!(!text.contains("C:\\"));
    }

    #[test]
    fn validates_sorted_environment_records_and_double_terminator() {
        assert!(validate_windows_environment_block(&block(&["Path=one", "TEMP=two"])).is_ok());
        assert!(validate_windows_environment_block(&[0, 0]).is_ok());
        assert_eq!(
            validate_windows_environment_block(&block(&["TEMP=two", "Path=one"])),
            Err(WindowsEnvironmentBlockError::NotSorted)
        );
        assert_eq!(
            validate_windows_environment_block(&[b'P' as u16, b'a' as u16, 0]),
            Err(WindowsEnvironmentBlockError::MissingDoubleTerminator)
        );
        assert_eq!(
            validate_windows_environment_block(&block(&["PATH"])),
            Err(WindowsEnvironmentBlockError::MissingEquals)
        );
        assert_eq!(
            validate_windows_environment_block(&block(&["=value"])),
            Err(WindowsEnvironmentBlockError::EmptyName)
        );
        assert_eq!(
            validate_windows_environment_block(&block(&["PATH=one", "=two"])),
            Err(WindowsEnvironmentBlockError::EmptyName)
        );
        assert_eq!(
            validate_windows_environment_block(&[0xD800, b'=' as u16, b'x' as u16, 0, 0]),
            Err(WindowsEnvironmentBlockError::InvalidUtf16)
        );
    }
}
