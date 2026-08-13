use std::io::{self, BufRead, Write};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};

use crate::protocol::{
    BrowserRequest, BrowserResponse, BrowserRpcRequest, BrowserRpcResponse,
    MAX_BROWSER_REQUEST_FRAME_BYTES, MAX_BROWSER_RESPONSE_FRAME_BYTES,
};

pub trait Transport: Write + Send {
    fn read_frame(&mut self, buf: &mut Vec<u8>, max_bytes: usize) -> io::Result<usize>;
}

pub fn read_bounded_frame<R: BufRead + ?Sized>(
    reader: &mut R,
    buf: &mut Vec<u8>,
    max_bytes: usize,
) -> io::Result<usize> {
    buf.clear();
    let rejection_limit = max_bytes.saturating_add(1);
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            if buf.is_empty() {
                return Ok(0);
            }
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "browser frame is not newline terminated",
            ));
        }
        let frame_chunk = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(available.len(), |index| index + 1);
        let remaining = rejection_limit.saturating_sub(buf.len());
        let copy_len = frame_chunk.min(remaining);
        buf.extend_from_slice(&available[..copy_len]);
        reader.consume(copy_len);
        if buf.len() > max_bytes {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("browser frame exceeds {max_bytes} bytes"),
            ));
        }
        if copy_len == frame_chunk && buf.last() == Some(&b'\n') {
            return Ok(buf.len());
        }
    }
}

pub fn send_and_receive<T: Transport + ?Sized>(
    transport: &mut T,
    request_id: u64,
    request: &BrowserRequest,
) -> MedusaResult<BrowserResponse> {
    let wire = BrowserRpcRequest {
        request_id,
        request: request.clone(),
    };
    let mut json = serde_json::to_vec(&wire)
        .map_err(|error| protocol_error("request_serialization", error.to_string()))?;
    json.push(b'\n');
    if json.len() > MAX_BROWSER_REQUEST_FRAME_BYTES {
        return Err(protocol_error(
            "request_too_large",
            format!("browser request frame exceeds {MAX_BROWSER_REQUEST_FRAME_BYTES} bytes"),
        ));
    }
    transport
        .write_all(&json)
        .map_err(|error| io_error("request_write", error))?;
    transport
        .flush()
        .map_err(|error| io_error("request_flush", error))?;

    let mut frame = Vec::with_capacity(4096);
    let count = transport
        .read_frame(&mut frame, MAX_BROWSER_RESPONSE_FRAME_BYTES)
        .map_err(|error| io_error("response_frame", error))?;
    if count == 0 {
        return Err(io_error(
            "sidecar_closed",
            io::Error::new(io::ErrorKind::BrokenPipe, "sidecar closed the connection"),
        ));
    }
    let response: BrowserRpcResponse = serde_json::from_slice(&frame)
        .map_err(|error| protocol_error("response_parse", error.to_string()))?;
    if response.request_id != request_id {
        return Err(protocol_error(
            "request_id_mismatch",
            format!(
                "browser response id {} did not match request id {request_id}",
                response.request_id
            ),
        ));
    }
    Ok(response.response)
}

fn protocol_error(kind: &'static str, message: String) -> MedusaError {
    let mut error = MedusaError::new(
        ErrorCode::IncompatibleProtocol,
        ErrorCategory::Execution,
        message,
    );
    error
        .context
        .insert("browser_error_kind".to_owned(), serde_json::json!(kind));
    error
}

fn io_error(kind: &'static str, source: io::Error) -> MedusaError {
    let framing = matches!(
        source.kind(),
        io::ErrorKind::InvalidData | io::ErrorKind::UnexpectedEof
    );
    let mut error = MedusaError::new(
        if framing {
            ErrorCode::IncompatibleProtocol
        } else {
            ErrorCode::DependencyUnavailable
        },
        ErrorCategory::Transient,
        format!("browser {kind}: {source}"),
    )
    .with_retryable(!framing);
    error
        .context
        .insert("browser_error_kind".to_owned(), serde_json::json!(kind));
    error
}

#[cfg(test)]
mod tests {
    use std::io::{self, BufReader, Cursor, Write};

    use super::{Transport, read_bounded_frame, send_and_receive};
    use crate::protocol::{BrowserRequest, BrowserResponse};

    #[derive(Default)]
    struct FakeTransport {
        written: Vec<u8>,
        response: Option<Vec<u8>>,
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
        fn read_frame(&mut self, buf: &mut Vec<u8>, max_bytes: usize) -> io::Result<usize> {
            if self.fail_read {
                return Err(io::Error::other("read failed"));
            }
            let Some(response) = self.response.take() else {
                return Ok(0);
            };
            let mut reader = BufReader::new(Cursor::new(response));
            read_bounded_frame(&mut reader, buf, max_bytes)
        }
    }

    #[test]
    fn successful_round_trip_serializes_correlated_frame() {
        let mut transport = FakeTransport {
            response: Some(b"{\"request_id\":7,\"kind\":\"ok\"}\n".to_vec()),
            ..FakeTransport::default()
        };
        let response = send_and_receive(&mut transport, 7, &BrowserRequest::Ping).unwrap();
        assert!(matches!(response, BrowserResponse::Ok));
        assert_eq!(
            transport.written,
            b"{\"request_id\":7,\"method\":\"ping\"}\n"
        );
    }

    #[test]
    fn mismatched_response_id_fails_closed() {
        let mut transport = FakeTransport {
            response: Some(b"{\"request_id\":8,\"kind\":\"ok\"}\n".to_vec()),
            ..FakeTransport::default()
        };
        let error = send_and_receive(&mut transport, 7, &BrowserRequest::Ping).unwrap_err();
        assert!(error.message.contains("did not match request id"));
    }

    #[test]
    fn bounded_reader_rejects_oversized_frame_at_max_plus_one() {
        let input = format!("{}\n", "x".repeat(9));
        let mut reader = BufReader::new(Cursor::new(input.into_bytes()));
        let mut frame = Vec::new();
        let error = read_bounded_frame(&mut reader, &mut frame, 8).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert_eq!(frame.len(), 9);
    }

    #[test]
    fn bounded_reader_rejects_unterminated_frame() {
        let mut reader = BufReader::new(Cursor::new(b"partial".to_vec()));
        let mut frame = Vec::new();
        let error = read_bounded_frame(&mut reader, &mut frame, 32).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof);
    }

    #[test]
    fn write_flush_read_and_eof_failures_are_contextual() {
        let mut transport = FakeTransport {
            fail_write: true,
            ..FakeTransport::default()
        };
        assert!(
            send_and_receive(&mut transport, 1, &BrowserRequest::Ping)
                .unwrap_err()
                .message
                .contains("request_write")
        );
        let mut transport = FakeTransport {
            fail_flush: true,
            ..FakeTransport::default()
        };
        assert!(
            send_and_receive(&mut transport, 1, &BrowserRequest::Ping)
                .unwrap_err()
                .message
                .contains("request_flush")
        );
        let mut transport = FakeTransport {
            fail_read: true,
            ..FakeTransport::default()
        };
        assert!(
            send_and_receive(&mut transport, 1, &BrowserRequest::Ping)
                .unwrap_err()
                .message
                .contains("response_frame")
        );
        let mut transport = FakeTransport::default();
        assert!(
            send_and_receive(&mut transport, 1, &BrowserRequest::Ping)
                .unwrap_err()
                .message
                .contains("sidecar closed")
        );
    }
}
