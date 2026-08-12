//! Production WebSocket ownership for authenticated OpenAI Realtime sessions.
//!
//! The short-lived bearer credential remains inside this runtime-owned wire.
//! Production connections require TLS, while deterministic tests may opt into
//! insecure loopback sockets. Debug and error output never includes the token or
//! endpoint.

use std::{fmt, net::TcpStream};

use medusa_openai_realtime::{GatewayCapability, SessionConfig, Wire, WireKind};
use serde_json::Value;
use tungstenite::{
    WebSocket,
    client::IntoClientRequest,
    error::Error as WebSocketError,
    http::{HeaderValue, Uri, header::AUTHORIZATION},
    protocol::Message,
    stream::MaybeTlsStream,
};

use crate::{
    openai_realtime::OpenAiRealtimeRoute,
    openai_realtime_session::{SessionCredential, SessionOwner, SessionOwnerError, WireFactory},
};

type Socket = WebSocket<MaybeTlsStream<TcpStream>>;

pub struct OpenAiWebSocketWire {
    socket: Option<Socket>,
    endpoint: String,
    bearer_token: String,
    allow_insecure_loopback: bool,
    closed: bool,
}

impl fmt::Debug for OpenAiWebSocketWire {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OpenAiWebSocketWire")
            .field("endpoint", &"[REDACTED]")
            .field("bearer_token", &"[REDACTED]")
            .field("closed", &self.closed)
            .finish()
    }
}

impl OpenAiWebSocketWire {
    pub fn connect(endpoint: &str, bearer_token: &str) -> Result<Self, String> {
        Self::connect_with_policy(endpoint, bearer_token, false)
    }

    fn connect_with_policy(
        endpoint: &str,
        bearer_token: &str,
        allow_insecure_loopback: bool,
    ) -> Result<Self, String> {
        validate_endpoint(endpoint, allow_insecure_loopback)?;
        let socket = open_socket(endpoint, bearer_token)?;
        Ok(Self {
            socket: Some(socket),
            endpoint: endpoint.to_owned(),
            bearer_token: bearer_token.to_owned(),
            allow_insecure_loopback,
            closed: false,
        })
    }

    fn socket_mut(&mut self) -> Result<&mut Socket, String> {
        if self.closed {
            return Err("Realtime WebSocket is closed".to_owned());
        }
        self.socket
            .as_mut()
            .ok_or_else(|| "Realtime WebSocket is unavailable".to_owned())
    }
}

impl Wire for OpenAiWebSocketWire {
    fn send_json(&mut self, payload: Value) -> Result<(), String> {
        let text = serde_json::to_string(&payload)
            .map_err(|_| "Realtime WebSocket payload serialization failed".to_owned())?;
        self.socket_mut()?
            .send(Message::Text(text.into()))
            .map_err(safe_websocket_error)
    }

    fn receive_json(&mut self) -> Result<Option<Value>, String> {
        loop {
            let message = match self.socket_mut()?.read() {
                Ok(message) => message,
                Err(WebSocketError::ConnectionClosed) => {
                    self.closed = true;
                    self.socket = None;
                    return Ok(None);
                }
                Err(error) => return Err(safe_websocket_error(error)),
            };
            match message {
                Message::Text(text) => {
                    return serde_json::from_str(text.as_ref())
                        .map(Some)
                        .map_err(|_| "Realtime WebSocket returned invalid JSON".to_owned());
                }
                Message::Close(_) => {
                    self.closed = true;
                    self.socket = None;
                    return Ok(None);
                }
                Message::Ping(_) => {
                    self.socket_mut()?.flush().map_err(safe_websocket_error)?;
                }
                Message::Pong(_) | Message::Frame(_) => {}
                Message::Binary(_) => {
                    return Err("Realtime WebSocket returned an unexpected binary frame".to_owned());
                }
            }
        }
    }

    fn reconnect(&mut self) -> Result<(), String> {
        if self.closed {
            return Err("Realtime WebSocket is closed".to_owned());
        }
        validate_endpoint(&self.endpoint, self.allow_insecure_loopback)?;
        let replacement = open_socket(&self.endpoint, &self.bearer_token)?;
        if let Some(mut existing) = self.socket.take() {
            let _ = existing.close(None);
        }
        self.socket = Some(replacement);
        Ok(())
    }

    fn close(&mut self) -> Result<(), String> {
        if self.closed {
            return Ok(());
        }
        self.closed = true;
        self.bearer_token.clear();
        let Some(mut socket) = self.socket.take() else {
            return Ok(());
        };
        match socket.close(None) {
            Ok(()) | Err(WebSocketError::ConnectionClosed | WebSocketError::AlreadyClosed) => {
                Ok(())
            }
            Err(error) => Err(safe_websocket_error(error)),
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct OpenAiWebSocketFactory;

impl WireFactory<OpenAiWebSocketWire> for OpenAiWebSocketFactory {
    fn connect(
        &mut self,
        capability: &GatewayCapability,
        credential: &SessionCredential,
    ) -> Result<OpenAiWebSocketWire, String> {
        if capability.wire != Some(WireKind::WebSocket) {
            return Err("authenticated Realtime route does not select WebSocket".to_owned());
        }
        OpenAiWebSocketWire::connect(credential.websocket_url(), credential.authorization_token())
    }
}

pub type OpenAiWebSocketSessionOwner =
    SessionOwner<OpenAiRealtimeRoute, OpenAiWebSocketFactory, OpenAiWebSocketWire>;

pub fn websocket_session_owner(
    route: OpenAiRealtimeRoute,
    capability: GatewayCapability,
    config: SessionConfig,
) -> Result<OpenAiWebSocketSessionOwner, SessionOwnerError> {
    SessionOwner::new(route, OpenAiWebSocketFactory, capability, config)
}

fn open_socket(endpoint: &str, bearer_token: &str) -> Result<Socket, String> {
    let mut request = endpoint
        .into_client_request()
        .map_err(|_| "Realtime WebSocket endpoint is invalid".to_owned())?;
    let authorization = HeaderValue::from_str(&format!("Bearer {bearer_token}"))
        .map_err(|_| "Realtime WebSocket credential is invalid".to_owned())?;
    request.headers_mut().insert(AUTHORIZATION, authorization);
    tungstenite::connect(request)
        .map(|(socket, _response)| socket)
        .map_err(safe_websocket_error)
}

fn validate_endpoint(endpoint: &str, allow_insecure_loopback: bool) -> Result<(), String> {
    let uri = endpoint
        .parse::<Uri>()
        .map_err(|_| "Realtime WebSocket endpoint is invalid".to_owned())?;
    let scheme = uri
        .scheme_str()
        .ok_or_else(|| "Realtime WebSocket endpoint omitted a scheme".to_owned())?;
    let host = uri
        .host()
        .ok_or_else(|| "Realtime WebSocket endpoint omitted a host".to_owned())?;
    if scheme == "wss" {
        return Ok(());
    }
    if allow_insecure_loopback
        && scheme == "ws"
        && matches!(host, "127.0.0.1" | "localhost" | "::1")
    {
        return Ok(());
    }
    Err("Realtime WebSocket endpoint must use wss://".to_owned())
}

fn safe_websocket_error(error: WebSocketError) -> String {
    match error {
        WebSocketError::ConnectionClosed => "Realtime WebSocket connection closed".to_owned(),
        WebSocketError::AlreadyClosed => "Realtime WebSocket is already closed".to_owned(),
        WebSocketError::Io(error) => {
            format!("Realtime WebSocket I/O failed ({:?})", error.kind())
        }
        WebSocketError::Http(response) => format!(
            "Realtime WebSocket handshake returned HTTP {}",
            response.status()
        ),
        WebSocketError::Tls(_) => "Realtime WebSocket TLS negotiation failed".to_owned(),
        WebSocketError::Url(_) => "Realtime WebSocket endpoint is invalid".to_owned(),
        _ => "Realtime WebSocket protocol operation failed".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use std::{net::TcpListener, sync::mpsc, thread};

    use serde_json::json;
    use tungstenite::{
        accept_hdr,
        handshake::server::{Request, Response},
        http::header::AUTHORIZATION,
    };

    use super::*;

    #[test]
    fn production_rejects_insecure_websocket_routes() {
        let error = OpenAiWebSocketWire::connect("ws://example.com/realtime", "short-secret")
            .expect_err("production route must require TLS");
        assert_eq!(error, "Realtime WebSocket endpoint must use wss://");
        assert!(!error.contains("short-secret"));
    }

    #[test]
    fn loopback_contract_sends_bearer_and_exchanges_json() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
        let endpoint = format!("ws://{}/realtime", listener.local_addr().expect("address"));
        let (observed_tx, observed_rx) = mpsc::channel();
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().expect("accept");
            let mut socket = accept_hdr(stream, move |request: &Request, response: Response| {
                let authorization = request
                    .headers()
                    .get(AUTHORIZATION)
                    .and_then(|value| value.to_str().ok())
                    .unwrap_or_default()
                    .to_owned();
                observed_tx.send(authorization).expect("send header");
                Ok(response)
            })
            .expect("handshake");
            let message = socket.read().expect("read request");
            let payload = message.into_text().expect("text request");
            assert_eq!(
                serde_json::from_str::<Value>(payload.as_ref()).expect("request JSON"),
                json!({ "type": "input_audio_buffer.commit" })
            );
            socket
                .send(Message::Text(
                    serde_json::to_string(&json!({ "type": "session.created" }))
                        .expect("response JSON")
                        .into(),
                ))
                .expect("send response");
        });

        let mut wire = OpenAiWebSocketWire::connect_with_policy(&endpoint, "short-secret", true)
            .expect("loopback connection");
        assert_eq!(
            observed_rx.recv().expect("authorization"),
            "Bearer short-secret"
        );
        wire.send_json(json!({ "type": "input_audio_buffer.commit" }))
            .expect("send JSON");
        assert_eq!(
            wire.receive_json().expect("receive JSON"),
            Some(json!({ "type": "session.created" }))
        );
        server.join().expect("server");
    }

    #[test]
    fn reconnect_reauthenticates_without_exposing_token() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
        let endpoint = format!("ws://{}/realtime", listener.local_addr().expect("address"));
        let (observed_tx, observed_rx) = mpsc::channel();
        let server = thread::spawn(move || {
            for _ in 0..2 {
                let (stream, _) = listener.accept().expect("accept");
                let observed_tx = observed_tx.clone();
                let _socket = accept_hdr(stream, move |request: &Request, response: Response| {
                    let authorization = request
                        .headers()
                        .get(AUTHORIZATION)
                        .and_then(|value| value.to_str().ok())
                        .unwrap_or_default()
                        .to_owned();
                    observed_tx.send(authorization).expect("send header");
                    Ok(response)
                })
                .expect("handshake");
            }
        });

        let mut wire = OpenAiWebSocketWire::connect_with_policy(&endpoint, "renewed-secret", true)
            .expect("first connection");
        assert_eq!(
            observed_rx.recv().expect("first authorization"),
            "Bearer renewed-secret"
        );
        wire.reconnect().expect("reconnect");
        assert_eq!(
            observed_rx.recv().expect("second authorization"),
            "Bearer renewed-secret"
        );
        let debug = format!("{wire:?}");
        assert!(!debug.contains("renewed-secret"));
        assert!(!debug.contains(&endpoint));
        server.join().expect("server");
    }

    #[test]
    fn connection_errors_do_not_include_credentials() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
        let endpoint = format!("ws://{}/realtime", listener.local_addr().expect("address"));
        drop(listener);
        let error =
            OpenAiWebSocketWire::connect_with_policy(&endpoint, "never-log-this-secret", true)
                .expect_err("connection must fail");
        assert!(!error.contains("never-log-this-secret"));
        assert!(!error.contains(&endpoint));
    }
}
