use std::io::Write;

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};

use crate::protocol::{BrowserRequest, BrowserResponse};

/// A sidecar transport: write a serialized request, read one serialized
/// response. Implementations only need `Write` plus a way to read one
/// newline-terminated frame.
pub trait Transport: Write {
    /// Read up to one newline-terminated frame into `buf`. Returns the
    /// number of bytes read (including the newline, if present) or 0 at
    /// EOF.
    fn read_frame(&mut self, buf: &mut String) -> std::io::Result<usize>;
}

pub fn send_and_receive<T: Transport + ?Sized>(
    transport: &mut T,
    request: &BrowserRequest,
) -> MedusaResult<BrowserResponse> {
    let mut json =
        serde_json::to_string(request).map_err(|e| transport_err(format!("serialize request: {e}")))?;
    json.push('\n');
    transport
        .write_all(json.as_bytes())
        .map_err(|e| transport_err(format!("write request: {e}")))?;
    transport
        .flush()
        .map_err(|e| transport_err(format!("flush request: {e}")))?;
    let mut line = String::new();
    let n = transport
        .read_frame(&mut line)
        .map_err(|e| transport_err(format!("read response: {e}")))?;
    if n == 0 {
        return Err(transport_err("sidecar closed the connection".to_owned()));
    }
    serde_json::from_str(&line).map_err(|e| transport_err(format!("parse response: {e}")))
}

fn transport_err(message: String) -> MedusaError {
    MedusaError::new(ErrorCode::DependencyUnavailable, ErrorCategory::Transient, message)
        .with_retryable(true)
}