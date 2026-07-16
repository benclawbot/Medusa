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
    let mut json = serde_json::to_string(request)
        .map_err(|e| transport_err(format!("serialize request: {e}")))?;
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
    MedusaError::new(
        ErrorCode::DependencyUnavailable,
        ErrorCategory::Transient,
        message,
    )
    .with_retryable(true)
}

#[cfg(test)]
mod tests {
    use std::io::{self, Write};

    use super::{Transport, send_and_receive};
    use crate::protocol::{BrowserRequest, BrowserResponse};

    #[derive(Default)]
    struct FakeTransport {
        written: Vec<u8>,
        response: Option<String>,
        fail_write: bool,
        fail_flush: bool,
        fail_read: bool,
    }

    impl Write for FakeTransport {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            if self.fail_write {
                return Err(io::Error::other("write failed"));
            }
            self.written.extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            if self.fail_flush {
                return Err(io::Error::other("flush failed"));
            }
            Ok(())
        }
    }

    impl Transport for FakeTransport {
        fn read_frame(&mut self, buf: &mut String) -> io::Result<usize> {
            if self.fail_read {
                return Err(io::Error::other("read failed"));
            }
            let Some(response) = self.response.take() else {
                return Ok(0);
            };
            buf.push_str(&response);
            Ok(response.len())
        }
    }

    #[test]
    fn successful_round_trip_serializes_one_newline_terminated_frame() {
        let mut transport = FakeTransport {
            response: Some("{\"kind\":\"ok\"}\n".to_owned()),
            ..FakeTransport::default()
        };

        let response = send_and_receive(&mut transport, &BrowserRequest::Ping).unwrap();

        assert!(matches!(response, BrowserResponse::Ok));
        assert_eq!(transport.written, b"{\"method\":\"ping\"}\n");
    }

    #[test]
    fn write_failure_is_retryable_and_contextual() {
        let mut transport = FakeTransport {
            fail_write: true,
            ..FakeTransport::default()
        };

        let error = send_and_receive(&mut transport, &BrowserRequest::Ping).unwrap_err();

        assert!(error.to_string().contains("write request"));
        assert!(error.to_string().contains("write failed"));
    }

    #[test]
    fn flush_failure_is_retryable_and_contextual() {
        let mut transport = FakeTransport {
            fail_flush: true,
            ..FakeTransport::default()
        };

        let error = send_and_receive(&mut transport, &BrowserRequest::Ping).unwrap_err();

        assert!(error.to_string().contains("flush request"));
        assert!(error.to_string().contains("flush failed"));
    }

    #[test]
    fn read_failure_is_retryable_and_contextual() {
        let mut transport = FakeTransport {
            fail_read: true,
            ..FakeTransport::default()
        };

        let error = send_and_receive(&mut transport, &BrowserRequest::Ping).unwrap_err();

        assert!(error.to_string().contains("read response"));
        assert!(error.to_string().contains("read failed"));
    }

    #[test]
    fn eof_is_reported_as_closed_sidecar() {
        let mut transport = FakeTransport::default();

        let error = send_and_receive(&mut transport, &BrowserRequest::Ping).unwrap_err();

        assert!(error.to_string().contains("sidecar closed the connection"));
    }

    #[test]
    fn malformed_response_is_reported_with_parse_context() {
        let mut transport = FakeTransport {
            response: Some("not-json\n".to_owned()),
            ..FakeTransport::default()
        };

        let error = send_and_receive(&mut transport, &BrowserRequest::Ping).unwrap_err();

        assert!(error.to_string().contains("parse response"));
    }
}
