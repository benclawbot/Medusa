use std::{path::Path, sync::atomic::AtomicBool, time::Duration};

use medusa_browser_client::{BrowserClient, BrowserResponse};
use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use serde_json::Value;

use crate::output_envelope::{EnvelopeConfig, OutputFormat, wrap};
use crate::tools::browser_dispatch::{build, format_response};

pub(crate) fn run(
    _repo: &Path,
    client: &mut BrowserClient,
    envelope_config: &EnvelopeConfig,
    method: &str,
    input: &Value,
    timeout: Duration,
    cancellation: &AtomicBool,
) -> MedusaResult<String> {
    let request = build(method, input).map_err(invalid_input)?;
    let response = client.request_with_control(request, timeout, cancellation)?;
    if let BrowserResponse::Error { code, message } = response {
        return Err(MedusaError::new(
            ErrorCode::ToolExecutionFailed,
            ErrorCategory::Execution,
            format!("{method}: {code}: {message}"),
        ));
    }
    let (text, binary) = format_response(response);
    let format = if binary.is_empty() {
        OutputFormat::Plain
    } else {
        OutputFormat::Binary
    };
    let body = if binary.is_empty() {
        text.as_bytes()
    } else {
        binary.as_slice()
    };
    let envelope = wrap(method, body, format, envelope_config)?;
    Ok(format!("{envelope}"))
}

fn invalid_input(message: String) -> MedusaError {
    MedusaError::new(
        ErrorCode::InvalidConfiguration,
        ErrorCategory::Validation,
        message,
    )
}
