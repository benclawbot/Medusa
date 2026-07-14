use std::path::Path;

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use serde_json::Value;

use crate::output_envelope::{EnvelopeConfig, OutputFormat, wrap};
use crate::session_browser::SessionBrowser;
use crate::tools::browser_dispatch::{build, format_response};

#[allow(dead_code)]
pub(crate) fn run(
    _repo: &Path,
    session: &mut SessionBrowser,
    envelope_config: &EnvelopeConfig,
    method: &str,
    input: &Value,
) -> MedusaResult<String> {
    let request = build(method, input).map_err(invalid_input)?;
    let client = session.client()?;
    let response = client.request(request)?;
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

#[allow(dead_code)]
fn invalid_input(message: String) -> MedusaError {
    MedusaError::new(
        ErrorCode::InvalidConfiguration,
        ErrorCategory::Validation,
        message,
    )
}