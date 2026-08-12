//! Loopback HTTP surface for the authenticated Telegram Mini App.
//!
//! The server verifies signed launch tickets and Telegram `initData`, mints short-lived OpenAI
//! Realtime WebRTC credentials, and places final transcripts on a bounded channel. The authoritative
//! Telegram runtime drains that channel and submits through its existing session service.

use std::{
    io::{Read, Write},
    net::{IpAddr, SocketAddr, TcpListener, TcpStream},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{Receiver, SyncSender, TrySendError, sync_channel},
    },
    thread,
    time::Duration,
};

use medusa_config::Config;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use ulid::Ulid;

use super::{TelegramIdentity, TelegramMiniAppBridge, TelegramMiniAppError};

const MAX_HEADER_BYTES: usize = 16 * 1024;
const MAX_BODY_BYTES: usize = 64 * 1024;
const COMMAND_QUEUE_CAPACITY: usize = 64;
const ACCEPT_POLL_INTERVAL: Duration = Duration::from_millis(100);
const CONNECTION_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_TRANSCRIPT_CHARS: usize = 32_000;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TelegramMiniAppHttpConfig {
    pub bind: SocketAddr,
    pub path_prefix: String,
}

impl TelegramMiniAppHttpConfig {
    pub fn validate(&self) -> Result<(), TelegramMiniAppHttpError> {
        if !self.bind.ip().is_loopback() {
            return Err(TelegramMiniAppHttpError::NonLoopbackBind(self.bind.ip()));
        }
        if self.path_prefix.len() < 2
            || self.path_prefix.len() > 200
            || !self.path_prefix.starts_with('/')
            || self.path_prefix.ends_with('/')
            || self.path_prefix.contains('?')
            || self.path_prefix.contains('#')
            || self.path_prefix.bytes().any(|byte| byte.is_ascii_control())
        {
            return Err(TelegramMiniAppHttpError::InvalidPathPrefix);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TelegramMiniAppCommand {
    pub command_id: String,
    pub identity: TelegramIdentity,
    pub session_id: String,
    pub transcript: String,
    pub received_at: OffsetDateTime,
}

pub struct TelegramMiniAppHttpServer {
    listener: TcpListener,
    config: TelegramMiniAppHttpConfig,
    bridge: TelegramMiniAppBridge,
    medusa_config: Config,
    sender: SyncSender<TelegramMiniAppCommand>,
}

impl TelegramMiniAppHttpServer {
    pub fn bind(
        config: TelegramMiniAppHttpConfig,
        bridge: TelegramMiniAppBridge,
        medusa_config: Config,
    ) -> Result<(Self, Receiver<TelegramMiniAppCommand>), TelegramMiniAppHttpError> {
        config.validate()?;
        let listener = TcpListener::bind(config.bind)?;
        listener.set_nonblocking(true)?;
        let (sender, receiver) = sync_channel(COMMAND_QUEUE_CAPACITY);
        Ok((
            Self {
                listener,
                config,
                bridge,
                medusa_config,
                sender,
            },
            receiver,
        ))
    }

    pub fn local_addr(&self) -> Result<SocketAddr, TelegramMiniAppHttpError> {
        self.listener.local_addr().map_err(Into::into)
    }

    pub fn run_until_cancelled(
        &self,
        cancelled: &AtomicBool,
    ) -> Result<(), TelegramMiniAppHttpError> {
        while !cancelled.load(Ordering::Acquire) {
            match self.listener.accept() {
                Ok((mut stream, peer)) => {
                    // BSD-family kernels can propagate O_NONBLOCK from the listener to the
                    // accepted socket. Request handling is deliberately bounded by timeouts, so
                    // normalize the connection to blocking mode before reading headers or bodies.
                    stream.set_nonblocking(false)?;
                    if !peer.ip().is_loopback() {
                        write_response(&mut stream, 403, "text/plain", b"forbidden")?;
                        continue;
                    }
                    stream.set_read_timeout(Some(CONNECTION_TIMEOUT))?;
                    stream.set_write_timeout(Some(CONNECTION_TIMEOUT))?;
                    let response = match read_request(&mut stream) {
                        Ok(request) => self.handle(request, OffsetDateTime::now_utc()),
                        Err(RequestRejection::TooLarge) => Response::text(413, "payload too large"),
                        Err(RequestRejection::Malformed) => Response::text(400, "bad request"),
                    };
                    write_response(
                        &mut stream,
                        response.status,
                        response.content_type,
                        &response.body,
                    )?;
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(ACCEPT_POLL_INTERVAL);
                }
                Err(error) => return Err(error.into()),
            }
        }
        Ok(())
    }

    fn handle(&self, request: Request, now: OffsetDateTime) -> Response {
        let root = format!("{}/", self.config.path_prefix);
        let auth = format!("{}/auth", self.config.path_prefix);
        let realtime = format!("{}/realtime", self.config.path_prefix);
        let transcript = format!("{}/transcript", self.config.path_prefix);
        match (request.method.as_str(), request.path.as_str()) {
            ("GET", path) if path == root || path == self.config.path_prefix => Response {
                status: 200,
                content_type: "text/html; charset=utf-8",
                body: TelegramMiniAppBridge::client_html(&self.config.path_prefix).into_bytes(),
            },
            ("POST", path) if path == auth => self.handle_auth(&request.body, now),
            ("POST", path) if path == realtime => {
                self.handle_realtime(request.authorization.as_deref(), now)
            }
            ("POST", path) if path == transcript => {
                self.handle_transcript(request.authorization.as_deref(), &request.body, now)
            }
            ("OPTIONS", _) => Response {
                status: 204,
                content_type: "text/plain",
                body: Vec::new(),
            },
            _ => Response::text(404, "not found"),
        }
    }

    fn handle_auth(&self, body: &[u8], now: OffsetDateTime) -> Response {
        let request: AuthRequest = match serde_json::from_slice(body) {
            Ok(request) => request,
            Err(_) => return Response::text(400, "invalid request"),
        };
        let binding = match self.bridge.inspect_launch_ticket(&request.ticket, now) {
            Ok(binding) => binding,
            Err(_) => return Response::text(401, "invalid launch ticket"),
        };
        if self
            .bridge
            .verify_init_data(&request.init_data, &binding.identity, now)
            .is_err()
        {
            return Response::text(401, "Telegram authentication failed");
        }
        let authenticated = match self.bridge.issue_authenticated_token(&binding, now) {
            Ok(authenticated) => authenticated,
            Err(_) => return Response::text(401, "Telegram authentication failed"),
        };
        Response::json(
            200,
            &AuthResponse {
                token: authenticated.token,
                expires_at: authenticated.expires_at,
                session_id: binding.session_id,
            },
        )
    }

    fn handle_realtime(&self, authorization: Option<&str>, now: OffsetDateTime) -> Response {
        let token = match bearer_token(authorization) {
            Some(token) => token,
            None => return Response::text(401, "missing bearer token"),
        };
        let binding = match self.bridge.inspect_authenticated_token(token, now) {
            Ok(binding) => binding,
            Err(_) => return Response::text(401, "invalid bearer token"),
        };
        match self.bridge.establish_realtime_session(
            token,
            &binding.identity,
            &self.medusa_config,
            now,
        ) {
            Ok(session) => Response::json(200, &session),
            Err(_) => Response::text(503, "Realtime unavailable"),
        }
    }

    fn handle_transcript(
        &self,
        authorization: Option<&str>,
        body: &[u8],
        now: OffsetDateTime,
    ) -> Response {
        let token = match bearer_token(authorization) {
            Some(token) => token,
            None => return Response::text(401, "missing bearer token"),
        };
        let binding = match self.bridge.inspect_authenticated_token(token, now) {
            Ok(binding) => binding,
            Err(_) => return Response::text(401, "invalid bearer token"),
        };
        let request: TranscriptRequest = match serde_json::from_slice(body) {
            Ok(request) => request,
            Err(_) => return Response::text(400, "invalid request"),
        };
        let transcript = request.transcript.trim();
        if transcript.is_empty() || transcript.chars().count() > MAX_TRANSCRIPT_CHARS {
            return Response::text(400, "invalid transcript");
        }
        let command = TelegramMiniAppCommand {
            command_id: Ulid::new().to_string(),
            identity: binding.identity,
            session_id: binding.session_id,
            transcript: transcript.to_owned(),
            received_at: now,
        };
        match self.sender.try_send(command) {
            Ok(()) => Response::json(202, &QueuedResponse { queued: true }),
            Err(TrySendError::Full(_)) => Response::text(429, "transcript queue is full"),
            Err(TrySendError::Disconnected(_)) => {
                Response::text(503, "Telegram runtime unavailable")
            }
        }
    }
}

fn read_request(stream: &mut TcpStream) -> Result<Request, RequestRejection> {
    let mut bytes = Vec::with_capacity(4_096);
    let header_end = loop {
        if bytes.len() >= MAX_HEADER_BYTES {
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
    let header_text =
        std::str::from_utf8(&bytes[..header_end]).map_err(|_| RequestRejection::Malformed)?;
    let mut lines = header_text.split("\r\n");
    let request_line = lines.next().ok_or(RequestRejection::Malformed)?;
    let mut request_parts = request_line.split_ascii_whitespace();
    let method = request_parts
        .next()
        .ok_or(RequestRejection::Malformed)?
        .to_owned();
    let target = request_parts.next().ok_or(RequestRejection::Malformed)?;
    // issue-568-owned-request-path
    let path = target
        .split('?')
        .next()
        .ok_or(RequestRejection::Malformed)?
        .to_owned();
    let version = request_parts.next().ok_or(RequestRejection::Malformed)?;
    if request_parts.next().is_some() || !matches!(version, "HTTP/1.0" | "HTTP/1.1") {
        return Err(RequestRejection::Malformed);
    }
    let mut content_length = 0_usize;
    let mut authorization = None;
    for line in lines.filter(|line| !line.is_empty()) {
        let (name, value) = line.split_once(':').ok_or(RequestRejection::Malformed)?;
        match name.trim().to_ascii_lowercase().as_str() {
            "content-length" => {
                content_length = value
                    .trim()
                    .parse::<usize>()
                    .map_err(|_| RequestRejection::Malformed)?;
            }
            "authorization" => authorization = Some(value.trim().to_owned()),
            "transfer-encoding" if !value.trim().eq_ignore_ascii_case("identity") => {
                return Err(RequestRejection::Malformed);
            }
            _ => {}
        }
    }
    if content_length > MAX_BODY_BYTES {
        return Err(RequestRejection::TooLarge);
    }
    let total = header_end
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
    Ok(Request {
        method,
        path,
        authorization,
        body: bytes[header_end..total].to_vec(),
    })
}

fn bearer_token(value: Option<&str>) -> Option<&str> {
    value?
        .strip_prefix("Bearer ")
        .filter(|token| !token.is_empty())
}

fn write_response(
    stream: &mut TcpStream,
    status: u16,
    content_type: &str,
    body: &[u8],
) -> Result<(), TelegramMiniAppHttpError> {
    let reason = match status {
        200 => "OK",
        202 => "Accepted",
        204 => "No Content",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        413 => "Payload Too Large",
        429 => "Too Many Requests",
        _ => "Service Unavailable",
    };
    let headers = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nReferrer-Policy: no-referrer\r\nX-Content-Type-Options: nosniff\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(headers.as_bytes())?;
    stream.write_all(body)?;
    stream.flush()?;
    Ok(())
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

struct Request {
    method: String,
    path: String,
    authorization: Option<String>,
    body: Vec<u8>,
}

struct Response {
    status: u16,
    content_type: &'static str,
    body: Vec<u8>,
}

impl Response {
    fn text(status: u16, value: &str) -> Self {
        Self {
            status,
            content_type: "text/plain; charset=utf-8",
            body: value.as_bytes().to_vec(),
        }
    }

    fn json<T: Serialize>(status: u16, value: &T) -> Self {
        match serde_json::to_vec(value) {
            Ok(body) => Self {
                status,
                content_type: "application/json",
                body,
            },
            Err(_) => Self::text(503, "response encoding failed"),
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AuthRequest {
    ticket: String,
    init_data: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AuthResponse {
    token: String,
    expires_at: i64,
    session_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TranscriptRequest {
    transcript: String,
}

#[derive(Serialize)]
struct QueuedResponse {
    queued: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RequestRejection {
    TooLarge,
    Malformed,
}

#[derive(Debug, thiserror::Error)]
pub enum TelegramMiniAppHttpError {
    #[error("Telegram Mini App listener must bind to loopback, not {0}")]
    NonLoopbackBind(IpAddr),
    #[error("Telegram Mini App path prefix is invalid")]
    InvalidPathPrefix,
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    MiniApp(#[from] TelegramMiniAppError),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configuration_is_loopback_only() {
        assert!(
            TelegramMiniAppHttpConfig {
                bind: "127.0.0.1:0".parse().expect("bind"),
                path_prefix: "/telegram/mini-app".to_owned(),
            }
            .validate()
            .is_ok()
        );
        assert!(
            TelegramMiniAppHttpConfig {
                bind: "0.0.0.0:8080".parse().expect("bind"),
                path_prefix: "/telegram/mini-app".to_owned(),
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn bearer_tokens_are_strict() {
        assert_eq!(bearer_token(Some("Bearer abc")), Some("abc"));
        assert_eq!(bearer_token(Some("bearer abc")), None);
        assert_eq!(bearer_token(Some("Bearer ")), None);
    }
}
