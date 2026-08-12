use std::{
    future::Future,
    sync::{
        OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use reqwest::{Client as AsyncClient, StatusCode, blocking::Client as BlockingClient};

const PROVIDER_REQUEST_TIMEOUT: Duration = Duration::from_secs(120);
const CANCELLATION_POLL_INTERVAL: Duration = Duration::from_millis(25);

pub(crate) fn shared_blocking_http_client() -> MedusaResult<BlockingClient> {
    static CLIENT: OnceLock<BlockingClient> = OnceLock::new();
    if let Some(client) = CLIENT.get() {
        return Ok(client.clone());
    }
    let client = BlockingClient::builder()
        .connect_timeout(Duration::from_secs(15))
        .timeout(PROVIDER_REQUEST_TIMEOUT)
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
    thread::scope(|scope| {
        let worker = scope.spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|error| {
                    provider_response_error(format!(
                        "could not start cancellable provider runtime: {error}"
                    ))
                })?;
            runtime.block_on(async move {
                tokio::select! {
                    biased;
                    () = wait_for_cancellation(cancel) => Err(cancelled_provider_error()),
                    result = future => result,
                }
            })
        });
        worker
            .join()
            .map_err(|_| provider_response_error("cancellable provider request worker panicked"))?
    })
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
    let body = response.text().unwrap_or_default();
    classify_status(status, body, retry_after_seconds)
}

pub(crate) async fn async_response_error(response: reqwest::Response) -> MedusaError {
    let status = response.status();
    let retry_after_seconds = retry_after_seconds(response.headers());
    let body = response.text().await.unwrap_or_default();
    classify_status(status, body, retry_after_seconds)
}

fn retry_after_seconds(headers: &reqwest::header::HeaderMap) -> Option<u64> {
    headers
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<u64>().ok())
}

pub(crate) fn classify_status(
    status: StatusCode,
    body: String,
    retry_after_seconds: Option<u64>,
) -> MedusaError {
    let retryable = status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error();
    let category = if retryable {
        ErrorCategory::Transient
    } else if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
        ErrorCategory::Policy
    } else {
        ErrorCategory::Validation
    };
    let mut error = MedusaError::new(
        ErrorCode::DependencyUnavailable,
        category,
        format!("provider returned HTTP {status}: {body}"),
    )
    .with_retryable(retryable);
    if let Some(seconds) = retry_after_seconds {
        error.context.insert(
            "retry_after_seconds".to_owned(),
            serde_json::Value::from(seconds),
        );
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
    use std::time::Instant;

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
}
