//! Loopback Telegram webhook receiver for reverse-proxied deployments.
//!
//! TLS termination remains outside the local daemon. The receiver binds only to loopback, requires
//! Telegram's secret-token header, bounds every request, and hands typed updates to the same runtime
//! path used by long polling.

use std::{
    fmt::Display,
    io::{Read, Write},
    net::{IpAddr, SocketAddr, TcpListener, TcpStream},
    sync::atomic::{AtomicBool, Ordering},
    thread,
    time::Duration,
};

use serde::{Deserialize, Serialize};

use super::bot_api::TelegramUpdate;

const MAX_WEBHOOK_BODY_BYTES: usize = 1024 * 1024;
const MAX_WEBHOOK_HEADER_BYTES: usize = 16 * 1024;
const ACCEPT_POLL_INTERVAL: Duration = Duration::from_millis(100);
const CONNECTION_TIMEOUT: Duration = Duration::from_secs(5);
const SECRET_HEADER: &str = "x-telegram-bot-api-secret-token";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TelegramWebhookConfig {
    pub bind: SocketAddr,
    pub path: String,
    pub secret_token: String,
}

impl TelegramWebhookConfig {
    pub fn validate(&self) -> Result<(), TelegramWebhookError> {
        if !self.bind.ip().is_loopback() {
            return Err(TelegramWebhookError::NonLoopbackBind(self.bind.ip()));
        }
        if self.path.len() < 2
            || self.path.len() > 240
            || !self.path.starts_with('/')
            || self.path.contains('?')
            || self.path.contains('#')
            || self.path.bytes().any(|byte| byte.is_ascii_control())
        {
            return Err(TelegramWebhookError::InvalidPath);
        }
        if self.secret_token.is_empty()
            || self.secret_token.len() > 256
            || !self
                .secret_token
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        {
            return Err(TelegramWebhookError::InvalidSecret);
        }
        Ok(())
    }
}

pub struct TelegramWebhookServer {
    listener: TcpListener,
    config: TelegramWebhookConfig,
}

impl TelegramWebhookServer {
    pub fn bind(config: TelegramWebhookConfig) -> Result<Self, TelegramWebhookError> {
        config.validate()?;
        let listener = TcpListener::bind(config.bind)?;
        listener.set_nonblocking(true)?;
        Ok(Self { listener, config })
    }

    pub fn local_addr(&self) -> Result<SocketAddr, TelegramWebhookError> {
        self.listener.local_addr().map_err(Into::into)
    }

    pub fn run_until_cancelled<F, E>(
        &self,
        cancelled: &AtomicBool,
        mut handler: F,
    ) -> Result<(), TelegramWebhookError>
    where
        F: FnMut(TelegramUpdate) -> Result<(), E>,
        E: Display,
    {
        while !cancelled.load(Ordering::Acquire) {
            match self.listener.accept() {
                Ok((mut stream, peer)) => {
                    // Accepted sockets can inherit the listener's nonblocking mode on BSD/macOS.
                    // Request parsing is synchronous and timeout-bounded, so normalize every
                    // connection back to blocking mode before reading any bytes.
                    stream.set_nonblocking(false)?;
                    if !peer.ip().is_loopback() {
                        let _ = write_response(&mut stream, 403, "forbidden");
                        continue;
                    }
                    stream.set_read_timeout(Some(CONNECTION_TIMEOUT))?;
                    stream.set_write_timeout(Some(CONNECTION_TIMEOUT))?;
                    let response = match read_request(&mut stream, &self.config) {
                        Ok(update) => match handler(update) {
                            Ok(()) => (200, "ok"),
                            Err(_) => (500, "handler failed"),
                        },
                        Err(RequestRejection::Unauthorized) => (401, "unauthorized"),
                        Err(RequestRejection::NotFound) => (404, "not found"),
                        Err(RequestRejection::MethodNotAllowed) => (405, "method not allowed"),
                        Err(RequestRejection::TooLarge) => (413, "payload too large"),
                        Err(RequestRejection::Malformed) => (400, "bad request"),
                    };
                    write_response(&mut stream, response.0, response.1)?;
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(ACCEPT_POLL_INTERVAL);
                }
                Err(error) => return Err(error.into()),
            }
        }
        Ok(())
    }
}

fn read_request(
    stream: &mut TcpStream,
    config: &TelegramWebhookConfig,
) -> Result<TelegramUpdate, RequestRejection> {
    let mut bytes = Vec::with_capacity(4_096);
    let header_end = loop {
        if bytes.len() >= MAX_WEBHOOK_HEADER_BYTES {
            return Err(RequestRejection::TooLarge);
        }
        let mut chunk = [0_u8; 1_024];
        let read = stream
            .read(&mut chunk)
            .map_err(|_| RequestRejection::Malformed)?;
        if read == 0 {
            return Err(RequestRejection::Malformed);
        }
        bytes.extend_from_slice(&chunk[..read]);
        if let Some(index) = find_bytes(&bytes, b"\r\n\r\n") {
            break index + 4;
        }
    };
    let headers =
        std::str::from_utf8(&bytes[..header_end]).map_err(|_| RequestRejection::Malformed)?;
    let mut lines = headers.split("\r\n");
    let request_line = lines.next().ok_or(RequestRejection::Malformed)?;
    let mut request_parts = request_line.split_ascii_whitespace();
    let method = request_parts.next().ok_or(RequestRejection::Malformed)?;
    let path = request_parts.next().ok_or(RequestRejection::Malformed)?;
    let version = request_parts.next().ok_or(RequestRejection::Malformed)?;
    if request_parts.next().is_some() || !matches!(version, "HTTP/1.0" | "HTTP/1.1") {
        return Err(RequestRejection::Malformed);
    }
    if method != "POST" {
        return Err(RequestRejection::MethodNotAllowed);
    }
    if path != config.path {
        return Err(RequestRejection::NotFound);
    }

    let mut content_length = None;
    let mut secret = None;
    for line in lines.filter(|line| !line.is_empty()) {
        let (name, value) = line.split_once(':').ok_or(RequestRejection::Malformed)?;
        let name = name.trim().to_ascii_lowercase();
        let value = value.trim();
        match name.as_str() {
            "content-length" => {
                if content_length.is_some() {
                    return Err(RequestRejection::Malformed);
                }
                content_length = Some(
                    value
                        .parse::<usize>()
                        .map_err(|_| RequestRejection::Malformed)?,
                );
            }
            SECRET_HEADER => secret = Some(value),
            "transfer-encoding" if !value.eq_ignore_ascii_case("identity") => {
                return Err(RequestRejection::Malformed);
            }
            _ => {}
        }
    }
    let content_length = content_length.ok_or(RequestRejection::Malformed)?;
    if content_length == 0 || content_length > MAX_WEBHOOK_BODY_BYTES {
        return Err(RequestRejection::TooLarge);
    }
    if !secret
        .is_some_and(|value| constant_time_eq(value.as_bytes(), config.secret_token.as_bytes()))
    {
        return Err(RequestRejection::Unauthorized);
    }
    let body_start = header_end;
    let total = body_start
        .checked_add(content_length)
        .ok_or(RequestRejection::TooLarge)?;
    while bytes.len() < total {
        let remaining = total - bytes.len();
        let mut chunk = vec![0_u8; remaining.min(8_192)];
        let read = stream
            .read(&mut chunk)
            .map_err(|_| RequestRejection::Malformed)?;
        if read == 0 {
            return Err(RequestRejection::Malformed);
        }
        bytes.extend_from_slice(&chunk[..read]);
    }
    if bytes.len() != total {
        return Err(RequestRejection::Malformed);
    }
    serde_json::from_slice(&bytes[body_start..total]).map_err(|_| RequestRejection::Malformed)
}

fn write_response(
    stream: &mut TcpStream,
    status: u16,
    body: &str,
) -> Result<(), TelegramWebhookError> {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        413 => "Payload Too Large",
        _ => "Internal Server Error",
    };
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(response.as_bytes())?;
    stream.flush()?;
    Ok(())
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    let mut difference = left.len() ^ right.len();
    let length = left.len().max(right.len());
    for index in 0..length {
        let left_byte = left.get(index).copied().unwrap_or_default();
        let right_byte = right.get(index).copied().unwrap_or_default();
        difference |= usize::from(left_byte ^ right_byte);
    }
    difference == 0
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RequestRejection {
    Unauthorized,
    NotFound,
    MethodNotAllowed,
    TooLarge,
    Malformed,
}

#[derive(Debug, thiserror::Error)]
pub enum TelegramWebhookError {
    #[error("Telegram webhook listener must bind to loopback, not {0}")]
    NonLoopbackBind(IpAddr),
    #[error("Telegram webhook path is invalid")]
    InvalidPath,
    #[error("Telegram webhook secret is invalid")]
    InvalidSecret,
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_comparison_handles_length_and_content() {
        assert!(constant_time_eq(b"secret", b"secret"));
        assert!(!constant_time_eq(b"secret", b"Secret"));
        assert!(!constant_time_eq(b"secret", b"secret-longer"));
    }

    #[test]
    fn webhook_config_is_loopback_only() {
        assert!(
            TelegramWebhookConfig {
                bind: "127.0.0.1:0".parse().expect("address"),
                path: "/telegram/webhook".to_owned(),
                secret_token: "valid_secret-42".to_owned(),
            }
            .validate()
            .is_ok()
        );
        assert!(matches!(
            TelegramWebhookConfig {
                bind: "0.0.0.0:8080".parse().expect("address"),
                path: "/telegram/webhook".to_owned(),
                secret_token: "valid_secret-42".to_owned(),
            }
            .validate(),
            Err(TelegramWebhookError::NonLoopbackBind(_))
        ));
    }
}
