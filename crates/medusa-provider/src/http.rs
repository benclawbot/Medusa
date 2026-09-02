use std::{
    future::Future,
    io::Read,
    sync::{
        OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use reqwest::{Client as AsyncClient, StatusCode, blocking::Client as BlockingClient};
use serde::de::DeserializeOwned;

use crate::{ProviderPlanUsage, plan_usage::observe_provider_plan_headers};

const PROVIDER_REQUEST_TIMEOUT: Duration = Duration::from_secs(120);
const CANCELLATION_POLL_INTERVAL: Duration = Duration::from_millis(25);
/// Maximum provider error-body bytes retained in diagnostics.
const PROVIDER_ERROR_BODY_LIMIT_BYTES: usize = 64 * 1024;
/// Maximum non-streaming successful provider response accepted for JSON parsing.
const PROVIDER_SUCCESS_BODY_LIMIT_BYTES: usize = 16 * 1024 * 1024;
const BODY_READ_CHUNK_BYTES: usize = 8 * 1024;

#[derive(Debug, Default)]
struct BoundedBody {
    bytes: Vec<u8>,
    truncated: bool,
}

pub(crate) fn shared_blocking_http_client() -> MedusaResult<BlockingClient> {
    static CLIENT: OnceLock<BlockingClient> = OnceLock::new();
    if let Some(client) = CLIENT.get() {
        return Ok(client.clone());
    }
    let client = BlockingClient::builder()
        .connect_timeout(Duration::from_secs(15))
        .timeout(PROVIDER_REQUEST_TIMEOUT)
        // MiniMax's OpenAI-compatible endpoint has intermittently reset HTTP/2
        // connections on Windows. Keep provider traffic on HTTP/1.1, which is
        // the protocol used by the documented curl/Python integrations.
        .http1_only()
        .tcp_nodelay(true)
        .pool_max_idle_per_host(8)
        .build()
        .map_err(provider_error)?;
    let _ = CLIENT.set(client.clone());
    Ok(CLIENT.get().cloned().unwrap_or(client))
}

pub(crate) fn shared_async_http_client() -> MedusaResult<AsyncClient> {
    static CLIENT: OnceLock<AsyncClient> = OnceLock::new();
    if let Some(client) = CLIENT.get() {
        return Ok(client.clone());
    }
    let client = AsyncClient::builder()
        .connect_timeout(Duration::from_secs(15))
        .timeout(PROVIDER_REQUEST_TIMEOUT)
        .http1_only()
        .tcp_nodelay(true)
        .pool_max_idle_per_host(8)
        .build()
        .map_err(provider_error)?;
    let _ = CLIENT.set(client.clone());
    Ok(CLIENT.get().cloned().unwrap_or(client))
}

pub(crate) fn run_cancellable_request<T, F>(cancel: &AtomicBool, future: F) -> MedusaResult<T>
where
    T: Send,
    F: Future<Output = MedusaResult<T>> + Send,
{
    if cancel.load(Ordering::SeqCst) {
        return Err(cancelled_provider_error());
    }
    let runtime = shared_provider_runtime()?;
    runtime.block_on(async move {
        tokio::select! {
            biased;
            () = wait_for_cancellation(cancel) => Err(cancelled_provider_error()),
            result = future => result,
        }
    })
}

fn shared_provider_runtime() -> MedusaResult<&'static tokio::runtime::Runtime> {
    static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    if let Some(runtime) = RUNTIME.get() {
        return Ok(runtime);
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .worker_threads(2)
        .thread_name("medusa-provider")
        .build()
        .map_err(|error| {
            provider_response_error(format!("could not start shared provider runtime: {error}"))
        })?;
    let _ = RUNTIME.set(runtime);
    RUNTIME
        .get()
        .ok_or_else(|| provider_response_error("shared provider runtime was not initialized"))
}

async fn wait_for_cancellation(cancel: &AtomicBool) {
    while !cancel.load(Ordering::SeqCst) {
        tokio::time::sleep(CANCELLATION_POLL_INTERVAL).await;
    }
}

pub(crate) fn cancelled_provider_error() -> MedusaError {
    MedusaError::new(
        ErrorCode::DependencyUnavailable,
        ErrorCategory::Transient,
        "provider request cancelled",
    )
}

pub(crate) fn blocking_response_error(response: reqwest::blocking::Response) -> MedusaError {
    let status = response.status();
    let retry_after_seconds = retry_after_seconds(response.headers());
    let plan_usage = observe_provider_plan_headers(response.headers());
    let body = read_blocking_bounded(response, PROVIDER_ERROR_BODY_LIMIT_BYTES).unwrap_or_default();
    classify_status_with_body(status, body, retry_after_seconds, plan_usage)
}

pub(crate) async fn async_response_error(response: reqwest::Response) -> MedusaError {
    let status = response.status();
    let retry_after_seconds = retry_after_seconds(response.headers());
    let plan_usage = observe_provider_plan_headers(response.headers());
    let body = read_async_bounded(response, PROVIDER_ERROR_BODY_LIMIT_BYTES)
        .await
        .unwrap_or_default();
    classify_status_with_body(status, body, retry_after_seconds, plan_usage)
}

pub(crate) fn blocking_response_json<T: DeserializeOwned>(
    response: reqwest::blocking::Response,
) -> MedusaResult<T> {
    let _ = observe_provider_plan_headers(response.headers());
    let body =
        read_blocking_bounded(response, PROVIDER_SUCCESS_BODY_LIMIT_BYTES).map_err(|error| {
            provider_response_error(format!("could not read provider response body: {error}"))
        })?;
    parse_bounded_json(body)
}

pub(crate) async fn async_response_json<T: DeserializeOwned>(
    response: reqwest::Response,
) -> MedusaResult<T> {
    let _ = observe_provider_plan_headers(response.headers());
    let body = read_async_bounded(response, PROVIDER_SUCCESS_BODY_LIMIT_BYTES)
        .await
        .map_err(provider_error)?;
    parse_bounded_json(body)
}

fn parse_bounded_json<T: DeserializeOwned>(body: BoundedBody) -> MedusaResult<T> {
    if body.truncated {
        return Err(provider_response_error(format!(
            "provider response body exceeded {PROVIDER_SUCCESS_BODY_LIMIT_BYTES} byte limit"
        )));
    }
    serde_json::from_slice(&body.bytes).map_err(provider_response_error)
}

fn read_blocking_bounded<R: Read>(mut reader: R, limit: usize) -> std::io::Result<BoundedBody> {
    let mut bytes = Vec::with_capacity(limit.min(BODY_READ_CHUNK_BYTES));
    let mut buffer = [0_u8; BODY_READ_CHUNK_BYTES];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            return Ok(BoundedBody {
                bytes,
                truncated: false,
            });
        }
        let remaining = limit.saturating_sub(bytes.len());
        let retained = read.min(remaining);
        bytes.extend_from_slice(&buffer[..retained]);
        if retained < read {
            return Ok(BoundedBody {
                bytes,
                truncated: true,
            });
        }
    }
}

async fn read_async_bounded(
    mut response: reqwest::Response,
    limit: usize,
) -> Result<BoundedBody, reqwest::Error> {
    let mut bytes = Vec::with_capacity(limit.min(BODY_READ_CHUNK_BYTES));
    while let Some(chunk) = response.chunk().await? {
        let remaining = limit.saturating_sub(bytes.len());
        let retained = chunk.len().min(remaining);
        bytes.extend_from_slice(&chunk[..retained]);
        if retained < chunk.len() {
            return Ok(BoundedBody {
                bytes,
                truncated: true,
            });
        }
    }
    Ok(BoundedBody {
        bytes,
        truncated: false,
    })
}

fn retry_after_seconds(headers: &reqwest::header::HeaderMap) -> Option<u64> {
    headers
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<u64>().ok())
}

#[cfg(test)]
pub(crate) fn classify_status(
    status: StatusCode,
    body: String,
    retry_after_seconds: Option<u64>,
) -> MedusaError {
    classify_status_with_body(
        status,
        BoundedBody {
            bytes: body.into_bytes(),
            truncated: false,
        },
        retry_after_seconds,
        None,
    )
}

fn classify_status_with_body(
    status: StatusCode,
    body: BoundedBody,
    retry_after_seconds: Option<u64>,
    plan_usage: Option<ProviderPlanUsage>,
) -> MedusaError {
    let retryable = status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error();
    let category = if retryable {
        ErrorCategory::Transient
    } else if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
        ErrorCategory::Policy
    } else {
        ErrorCategory::Validation
    };
    let excerpt = String::from_utf8_lossy(&body.bytes);
    let truncation = if body.truncated {
        format!(" [provider error body truncated at {PROVIDER_ERROR_BODY_LIMIT_BYTES} bytes]")
    } else {
        String::new()
    };
    let mut error = MedusaError::new(
        ErrorCode::DependencyUnavailable,
        category,
        format!("provider returned HTTP {status}: {excerpt}{truncation}"),
    )
    .with_retryable(retryable);
    error.context.insert(
        "provider_error_body_limit_bytes".to_owned(),
        serde_json::Value::from(PROVIDER_ERROR_BODY_LIMIT_BYTES as u64),
    );
    error.context.insert(
        "provider_error_body_truncated".to_owned(),
        serde_json::Value::from(body.truncated),
    );
    if let Some(seconds) = retry_after_seconds {
        error.context.insert(
            "retry_after_seconds".to_owned(),
            serde_json::Value::from(seconds),
        );
    }
    if status == StatusCode::TOO_MANY_REQUESTS
        && let Some(plan_usage) = plan_usage
        && plan_usage.exhausted()
        && let Some(reset_at) = plan_usage.reset_at_unix
    {
        error.context.insert(
            "provider_plan_limit".to_owned(),
            serde_json::Value::Bool(true),
        );
        error.context.insert(
            "provider_plan_reset_at_unix".to_owned(),
            serde_json::Value::from(reset_at),
        );
        if let Ok(value) = serde_json::to_value(&plan_usage) {
            error
                .context
                .insert("provider_plan_usage".to_owned(), value);
        }
    }
    error
}

pub(crate) fn provider_error(error: reqwest::Error) -> MedusaError {
    let message = if error.is_connect() {
        let endpoint = error
            .url()
            .map_or_else(|| "the configured endpoint".to_owned(), ToString::to_string);
        format!(
            "provider endpoint is unavailable at {endpoint}; start the local or gateway service, configure a reachable provider with `medusa config`, or configure model.fallback_providers: {error}"
        )
    } else {
        format!("provider request failed: {error}")
    };
    MedusaError::new(
        ErrorCode::DependencyUnavailable,
        ErrorCategory::Transient,
        message,
    )
    .with_retryable(true)
}

pub(crate) fn provider_response_error(error: impl std::fmt::Display) -> MedusaError {
    MedusaError::new(
        ErrorCode::DependencyUnavailable,
        ErrorCategory::Validation,
        format!("provider returned an invalid response: {error}"),
    )
}

#[cfg(test)]
mod tests {
    use std::{
        io::{Read as _, Write as _},
        net::TcpListener,
        thread,
        time::Instant,
    };

    use reqwest::header::{HeaderMap, HeaderValue};

    use super::*;

    #[test]
    fn cancellable_runtime_drops_pending_request_future() {
        let cancel = AtomicBool::new(false);
        thread::scope(|scope| {
            scope.spawn(|| {
                thread::sleep(Duration::from_millis(75));
                cancel.store(true, Ordering::SeqCst);
            });
            let started = Instant::now();
            let error = run_cancellable_request(&cancel, async {
                tokio::time::sleep(Duration::from_secs(30)).await;
                Ok::<(), MedusaError>(())
            })
            .expect_err("pending future should be cancelled");
            assert!(error.to_string().contains("cancelled"));
            assert!(started.elapsed() < Duration::from_secs(2));
        });
    }

    #[test]
    fn rate_limit_is_retryable() {
        assert!(classify_status(StatusCode::TOO_MANY_REQUESTS, "slow down".into(), None).retryable);
    }

    #[test]
    fn retry_after_seconds_are_preserved_for_the_manager() {
        let error = classify_status(StatusCode::TOO_MANY_REQUESTS, "slow down".into(), Some(7));
        assert_eq!(
            error.context.get("retry_after_seconds"),
            Some(&serde_json::Value::from(7_u64))
        );
    }

    #[test]
    fn provider_plan_limit_is_distinct_from_generic_429() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-codex-primary-used-percent",
            HeaderValue::from_static("100"),
        );
        headers.insert(
            "x-codex-primary-reset-at",
            HeaderValue::from_static("2000000000"),
        );
        let usage = crate::plan_usage::infer_provider_plan_usage(&headers).expect("plan usage");
        let error = classify_status_with_body(
            StatusCode::TOO_MANY_REQUESTS,
            BoundedBody {
                bytes: b"limit".to_vec(),
                truncated: false,
            },
            Some(60),
            Some(usage),
        );
        assert_eq!(
            error.context.get("provider_plan_limit"),
            Some(&serde_json::Value::Bool(true))
        );
        assert_eq!(
            error.context.get("provider_plan_reset_at_unix"),
            Some(&serde_json::Value::from(2_000_000_000_i64))
        );
    }

    #[test]
    fn ordinary_429_stays_short_retry_even_when_plan_usage_is_partial() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-codex-primary-used-percent",
            HeaderValue::from_static("42"),
        );
        headers.insert(
            "x-codex-primary-reset-at",
            HeaderValue::from_static("2000000000"),
        );
        let usage = crate::plan_usage::infer_provider_plan_usage(&headers).expect("plan usage");
        let error = classify_status_with_body(
            StatusCode::TOO_MANY_REQUESTS,
            BoundedBody {
                bytes: b"rpm".to_vec(),
                truncated: false,
            },
            Some(2),
            Some(usage),
        );
        assert_eq!(error.context.get("provider_plan_limit"), None);
        assert_eq!(
            error.context.get("retry_after_seconds"),
            Some(&serde_json::Value::from(2_u64))
        );
    }

    #[test]
    fn oversized_provider_error_response_is_bounded_and_marked() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind test server");
        let address = listener.local_addr().expect("test server address");
        let body_bytes = PROVIDER_ERROR_BODY_LIMIT_BYTES * 8;
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept test connection");
            let mut request = [0_u8; 1024];
            let _ = stream.read(&mut request);
            write!(
                stream,
                "HTTP/1.1 500 Internal Server Error\r\nContent-Length: {body_bytes}\r\nConnection: close\r\n\r\n"
            )
            .expect("write response headers");
            let chunk = [b'x'; BODY_READ_CHUNK_BYTES];
            let mut remaining = body_bytes;
            while remaining > 0 {
                let write = remaining.min(chunk.len());
                if stream.write_all(&chunk[..write]).is_err() {
                    break;
                }
                remaining -= write;
            }
        });

        let response = BlockingClient::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .expect("build test client")
            .get(format!("http://{address}/oversized"))
            .send()
            .expect("receive oversized error response");
        let error = blocking_response_error(response);
        server.join().expect("test server join");

        assert_eq!(
            error.context.get("provider_error_body_truncated"),
            Some(&serde_json::Value::Bool(true))
        );
        assert_eq!(
            error.context.get("provider_error_body_limit_bytes"),
            Some(&serde_json::Value::from(
                PROVIDER_ERROR_BODY_LIMIT_BYTES as u64
            ))
        );
        assert!(error.to_string().contains("provider error body truncated"));
        assert!(error.to_string().len() < PROVIDER_ERROR_BODY_LIMIT_BYTES * 2);
    }

    #[test]
    fn bounded_reader_stops_after_limit() {
        let source = std::io::Cursor::new(vec![b'x'; PROVIDER_ERROR_BODY_LIMIT_BYTES * 4]);
        let body = read_blocking_bounded(source, PROVIDER_ERROR_BODY_LIMIT_BYTES)
            .expect("bounded body read");
        assert!(body.truncated);
        assert_eq!(body.bytes.len(), PROVIDER_ERROR_BODY_LIMIT_BYTES);
    }

    #[test]
    fn successful_json_rejects_truncated_body() {
        let error = parse_bounded_json::<serde_json::Value>(BoundedBody {
            bytes: br#"{"ok":true}"#.to_vec(),
            truncated: true,
        })
        .expect_err("truncated success body must be rejected");
        assert!(error.to_string().contains("exceeded"));
    }
}
