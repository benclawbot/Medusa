#!/usr/bin/env python3
from pathlib import Path

path = Path("crates/medusa-provider/src/lib.rs")
text = path.read_text()

replacements = [
    (
        'use std::{env, sync::OnceLock, thread, time::Duration};',
        'use std::{env, sync::OnceLock, time::Duration};',
    ),
    ('const MAX_PROVIDER_RETRIES: u8 = 2;\n', ''),
    ('    max_retries: u8,\n', ''),
    ('            max_retries: MAX_PROVIDER_RETRIES,\n', ''),
]

for old, new in replacements:
    count = text.count(old)
    if count == 0:
        raise SystemExit(f"expected source fragment missing: {old!r}")
    text = text.replace(old, new)

anthropic_old = '''        let body = self.request_body(request);
        let mut attempt = 0_u8;
        loop {
            let response = self
                .client
                .post(&endpoint)
                .header("x-api-key", &self.api_key)
                .header("anthropic-version", "2023-06-01")
                .json(&body)
                .send();
            match response {
                Ok(response) if response.status().is_success() => {
                    let wire: WireResponse = response.json().map_err(provider_error)?;
                    return Ok(wire.into_model_response());
                }
                Ok(response) => {
                    let status = response.status();
                    let text = response.text().unwrap_or_default();
                    let error = classify_status(status, text);
                    if !error.retryable || attempt >= self.max_retries {
                        return Err(error);
                    }
                }
                Err(error) => {
                    if attempt >= self.max_retries {
                        return Err(provider_error(error));
                    }
                }
            }
            attempt = attempt.saturating_add(1);
            thread::sleep(Duration::from_millis(250 * u64::from(attempt)));
        }
'''
anthropic_new = '''        let response = self
            .client
            .post(&endpoint)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .json(&self.request_body(request))
            .send()
            .map_err(provider_error)?;
        if response.status().is_success() {
            let wire: WireResponse = response.json().map_err(provider_error)?;
            return Ok(wire.into_model_response());
        }
        Err(response_error(response))
'''
if text.count(anthropic_old) != 1:
    raise SystemExit("Anthropic retry loop source mismatch")
text = text.replace(anthropic_old, anthropic_new)

openai_old = '''        let body = self.request_body(request);
        let mut attempt = 0_u8;
        loop {
            let mut builder = self.client.post(&endpoint).json(&body);
            if let Some(key) = &self.api_key {
                builder = builder.bearer_auth(key);
            }
            match builder.send() {
                Ok(response) if response.status().is_success() => {
                    let wire: OpenAiWireResponse = response.json().map_err(provider_error)?;
                    return wire.into_model_response();
                }
                Ok(response) => {
                    let status = response.status();
                    let text = response.text().unwrap_or_default();
                    let error = classify_status(status, text);
                    if !error.retryable || attempt >= self.max_retries {
                        return Err(error);
                    }
                }
                Err(error) if attempt >= self.max_retries => return Err(provider_error(error)),
                Err(_) => {}
            }
            attempt = attempt.saturating_add(1);
            thread::sleep(Duration::from_millis(250 * u64::from(attempt)));
        }
'''
openai_new = '''        let mut builder = self.client.post(&endpoint).json(&self.request_body(request));
        if let Some(key) = &self.api_key {
            builder = builder.bearer_auth(key);
        }
        let response = builder.send().map_err(provider_error)?;
        if response.status().is_success() {
            let wire: OpenAiWireResponse = response.json().map_err(provider_error)?;
            return wire.into_model_response();
        }
        Err(response_error(response))
'''
if text.count(openai_old) != 1:
    raise SystemExit("OpenAI retry loop source mismatch")
text = text.replace(openai_old, openai_new)

classify_old = '''fn classify_status(status: StatusCode, body: String) -> MedusaError {
    let retryable = status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error();
    let category = if retryable {
        ErrorCategory::Transient
    } else if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
        ErrorCategory::Policy
    } else {
        ErrorCategory::Validation
    };
    MedusaError::new(
        ErrorCode::DependencyUnavailable,
        category,
        format!("provider returned HTTP {status}: {body}"),
    )
    .with_retryable(retryable)
}
'''
classify_new = '''fn response_error(response: reqwest::blocking::Response) -> MedusaError {
    let status = response.status();
    let retry_after_seconds = response
        .headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<u64>().ok());
    let body = response.text().unwrap_or_default();
    classify_status(status, body, retry_after_seconds)
}

fn classify_status(
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
'''
if text.count(classify_old) != 1:
    raise SystemExit("status classification source mismatch")
text = text.replace(classify_old, classify_new)

text = text.replace(
    'classify_status(StatusCode::TOO_MANY_REQUESTS, "slow down".into()).retryable',
    'classify_status(StatusCode::TOO_MANY_REQUESTS, "slow down".into(), None).retryable',
)

needle = '''    fn rate_limit_is_retryable() {
        assert!(classify_status(StatusCode::TOO_MANY_REQUESTS, "slow down".into(), None).retryable);
    }
'''
addition = needle + '''
    #[test]
    fn retry_after_seconds_are_preserved_for_the_manager() {
        let error = classify_status(
            StatusCode::TOO_MANY_REQUESTS,
            "slow down".into(),
            Some(7),
        );
        assert_eq!(
            error.context.get("retry_after_seconds"),
            Some(&serde_json::Value::from(7_u64))
        );
    }
'''
if text.count(needle) != 1:
    raise SystemExit("rate-limit test source mismatch")
text = text.replace(needle, addition)

if 'max_retries' in text or 'thread::sleep' in text:
    raise SystemExit("adapter retry authority was not fully removed")

path.write_text(text)
# Trigger the verified branch-only patch workflow.
