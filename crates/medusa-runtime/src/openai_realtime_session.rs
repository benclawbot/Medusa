//! Runtime ownership for authenticated OpenAI Realtime sessions.
//!
//! This layer joins the trusted credential issuer in `openai_realtime` with the
//! provider-isolated transport in `medusa-openai-realtime`. It keeps short-lived
//! credentials out of frontend code, renews them before expiry, rebuilds the wire
//! on reconnect, and preserves the user's activation and mute intent.

use medusa_openai_realtime::{
    GatewayCapability, SessionConfig, Transport, TransportError, Wire,
};
use thiserror::Error;

use crate::openai_realtime::{
    OpenAiRealtimeEstablishError, OpenAiRealtimeRoute, OpenAiRealtimeSessionCredential,
};

const DEFAULT_RENEW_BEFORE_SECONDS: u64 = 15;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionCredential {
    token: String,
    expires_at: u64,
    model: String,
    websocket_url: String,
    webrtc_call_url: String,
}

impl SessionCredential {
    #[must_use]
    pub fn authorization_token(&self) -> &str {
        &self.token
    }

    #[must_use]
    pub fn expires_at(&self) -> u64 {
        self.expires_at
    }

    #[must_use]
    pub fn model(&self) -> &str {
        &self.model
    }

    #[must_use]
    pub fn websocket_url(&self) -> &str {
        &self.websocket_url
    }

    #[must_use]
    pub fn webrtc_call_url(&self) -> &str {
        &self.webrtc_call_url
    }

    #[must_use]
    pub fn needs_renewal(&self, now_seconds: u64, renew_before_seconds: u64) -> bool {
        self.expires_at
            <= now_seconds.saturating_add(renew_before_seconds)
    }
}

impl From<OpenAiRealtimeSessionCredential> for SessionCredential {
    fn from(credential: OpenAiRealtimeSessionCredential) -> Self {
        Self {
            token: credential.authorization_token().to_owned(),
            expires_at: credential.expires_at(),
            model: credential.model().to_owned(),
            websocket_url: credential.websocket_url().to_owned(),
            webrtc_call_url: credential.webrtc_call_url().to_owned(),
        }
    }
}

pub trait CredentialIssuer {
    fn issue(&mut self) -> Result<SessionCredential, String>;
}

impl CredentialIssuer for OpenAiRealtimeRoute {
    fn issue(&mut self) -> Result<SessionCredential, String> {
        self.establish_session()
            .map(SessionCredential::from)
            .map_err(|error: OpenAiRealtimeEstablishError| error.to_string())
    }
}

pub trait WireFactory<W: Wire> {
    fn connect(
        &mut self,
        capability: &GatewayCapability,
        credential: &SessionCredential,
    ) -> Result<W, String>;
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum SessionOwnerError {
    #[error("Realtime credential establishment failed: {0}")]
    Credential(String),
    #[error("Realtime wire establishment failed: {0}")]
    Wire(String),
    #[error(transparent)]
    Transport(#[from] TransportError),
    #[error("Realtime session has not been established")]
    NotEstablished,
}

pub struct SessionOwner<I, F, W>
where
    I: CredentialIssuer,
    F: WireFactory<W>,
    W: Wire,
{
    issuer: I,
    wire_factory: F,
    capability: GatewayCapability,
    config: SessionConfig,
    credential: Option<SessionCredential>,
    transport: Option<Transport<W>>,
    renew_before_seconds: u64,
    activated: bool,
    muted: bool,
}

impl<I, F, W> SessionOwner<I, F, W>
where
    I: CredentialIssuer,
    F: WireFactory<W>,
    W: Wire,
{
    pub fn new(
        issuer: I,
        wire_factory: F,
        capability: GatewayCapability,
        config: SessionConfig,
    ) -> Result<Self, SessionOwnerError> {
        capability.validate()?;
        Ok(Self {
            issuer,
            wire_factory,
            capability,
            config,
            credential: None,
            transport: None,
            renew_before_seconds: DEFAULT_RENEW_BEFORE_SECONDS,
            activated: false,
            muted: true,
        })
    }

    pub fn with_renew_before_seconds(mut self, seconds: u64) -> Self {
        self.renew_before_seconds = seconds;
        self
    }

    #[must_use]
    pub fn is_established(&self) -> bool {
        self.transport.is_some()
    }

    #[must_use]
    pub fn credential_expires_at(&self) -> Option<u64> {
        self.credential.as_ref().map(SessionCredential::expires_at)
    }

    pub fn establish(&mut self) -> Result<(), SessionOwnerError> {
        let credential = self
            .issuer
            .issue()
            .map_err(SessionOwnerError::Credential)?;
        let wire = self
            .wire_factory
            .connect(&self.capability, &credential)
            .map_err(SessionOwnerError::Wire)?;
        let mut transport = Transport::new(wire, self.capability.clone(), self.config.clone())?;
        self.apply_user_state(&mut transport)?;
        self.credential = Some(credential);
        self.transport = Some(transport);
        Ok(())
    }

    pub fn ensure_fresh(&mut self, now_seconds: u64) -> Result<(), SessionOwnerError> {
        let needs_renewal = self
            .credential
            .as_ref()
            .is_none_or(|credential| {
                credential.needs_renewal(now_seconds, self.renew_before_seconds)
            });
        if needs_renewal {
            self.rebuild()?;
        }
        Ok(())
    }

    pub fn activate(&mut self, now_seconds: u64) -> Result<(), SessionOwnerError> {
        self.activated = true;
        self.muted = false;
        self.ensure_fresh(now_seconds)?;
        self.transport_mut()?.activate()?;
        Ok(())
    }

    pub fn set_muted(
        &mut self,
        muted: bool,
        now_seconds: u64,
    ) -> Result<(), SessionOwnerError> {
        self.muted = muted;
        self.ensure_fresh(now_seconds)?;
        self.transport_mut()?.set_muted(muted)?;
        Ok(())
    }

    pub fn queue_input_audio(
        &mut self,
        audio_base64: String,
        now_seconds: u64,
    ) -> Result<(), SessionOwnerError> {
        self.ensure_fresh(now_seconds)?;
        self.transport_mut()?.queue_input_audio(audio_base64)?;
        Ok(())
    }

    pub fn reconnect(&mut self, now_seconds: u64) -> Result<(), SessionOwnerError> {
        let needs_new_credential = self
            .credential
            .as_ref()
            .is_none_or(|credential| {
                credential.needs_renewal(now_seconds, self.renew_before_seconds)
            });
        if needs_new_credential {
            self.rebuild()
        } else {
            self.transport_mut()?.reconnect()?;
            Ok(())
        }
    }

    pub fn close(&mut self) -> Result<(), SessionOwnerError> {
        self.activated = false;
        self.muted = true;
        self.credential = None;
        if let Some(mut transport) = self.transport.take() {
            transport.close()?;
        }
        Ok(())
    }

    fn rebuild(&mut self) -> Result<(), SessionOwnerError> {
        if let Some(mut transport) = self.transport.take() {
            transport.close()?;
        }
        self.establish()
    }

    fn apply_user_state(&self, transport: &mut Transport<W>) -> Result<(), SessionOwnerError> {
        if self.activated {
            transport.activate()?;
            if self.muted {
                transport.set_muted(true)?;
            }
        }
        Ok(())
    }

    fn transport_mut(&mut self) -> Result<&mut Transport<W>, SessionOwnerError> {
        self.transport
            .as_mut()
            .ok_or(SessionOwnerError::NotEstablished)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use serde_json::Value;

    use super::*;
    use medusa_openai_realtime::WireKind;

    #[derive(Default)]
    struct FakeIssuer {
        issued: VecDeque<Result<SessionCredential, String>>,
    }

    impl CredentialIssuer for FakeIssuer {
        fn issue(&mut self) -> Result<SessionCredential, String> {
            self.issued
                .pop_front()
                .unwrap_or_else(|| Err("no credential scripted".to_owned()))
        }
    }

    #[derive(Default)]
    struct FakeFactory {
        tokens: Vec<String>,
    }

    impl WireFactory<FakeWire> for FakeFactory {
        fn connect(
            &mut self,
            _capability: &GatewayCapability,
            credential: &SessionCredential,
        ) -> Result<FakeWire, String> {
            self.tokens
                .push(credential.authorization_token().to_owned());
            Ok(FakeWire::default())
        }
    }

    #[derive(Default)]
    struct FakeWire {
        sent: Vec<Value>,
        reconnects: usize,
        closed: bool,
    }

    impl Wire for FakeWire {
        fn send_json(&mut self, payload: Value) -> Result<(), String> {
            self.sent.push(payload);
            Ok(())
        }

        fn receive_json(&mut self) -> Result<Option<Value>, String> {
            Ok(None)
        }

        fn reconnect(&mut self) -> Result<(), String> {
            self.reconnects += 1;
            Ok(())
        }

        fn close(&mut self) -> Result<(), String> {
            self.closed = true;
            Ok(())
        }
    }

    fn credential(token: &str, expires_at: u64) -> SessionCredential {
        SessionCredential {
            token: token.to_owned(),
            expires_at,
            model: "gpt-realtime".to_owned(),
            websocket_url: "wss://api.openai.com/v1/realtime?model=gpt-realtime".to_owned(),
            webrtc_call_url: "https://api.openai.com/v1/realtime/calls".to_owned(),
        }
    }

    fn capability() -> GatewayCapability {
        GatewayCapability {
            available: true,
            reason: None,
            endpoint: Some("wss://api.openai.com/v1/realtime".to_owned()),
            model: Some("gpt-realtime".to_owned()),
            wire: Some(WireKind::WebSocket),
            supports_input_audio: true,
            supports_output_audio: true,
            supports_barge_in: true,
        }
    }

    #[test]
    fn activation_is_fail_closed_until_credential_and_wire_exist() {
        let issuer = FakeIssuer {
            issued: VecDeque::from([Err("OAuth session unavailable".to_owned())]),
        };
        let mut owner = SessionOwner::new(
            issuer,
            FakeFactory::default(),
            capability(),
            SessionConfig::default(),
        )
        .expect("valid owner");

        let error = owner.activate(100).expect_err("activation must fail");

        assert_eq!(
            error,
            SessionOwnerError::Credential("OAuth session unavailable".to_owned())
        );
        assert!(!owner.is_established());
    }

    #[test]
    fn renewal_rebuilds_transport_and_preserves_activation() {
        let issuer = FakeIssuer {
            issued: VecDeque::from([
                Ok(credential("first-secret", 120)),
                Ok(credential("second-secret", 240)),
            ]),
        };
        let mut owner = SessionOwner::new(
            issuer,
            FakeFactory::default(),
            capability(),
            SessionConfig::default(),
        )
        .expect("valid owner")
        .with_renew_before_seconds(15);

        owner.activate(100).expect("first activation");
        assert_eq!(owner.credential_expires_at(), Some(120));

        owner.ensure_fresh(106).expect("renew before expiry");

        assert_eq!(owner.credential_expires_at(), Some(240));
        owner
            .queue_input_audio("cGNt".to_owned(), 110)
            .expect("audio remains active after renewal");
    }

    #[test]
    fn reconnect_reuses_fresh_credential_but_renews_stale_one() {
        let issuer = FakeIssuer {
            issued: VecDeque::from([
                Ok(credential("first-secret", 200)),
                Ok(credential("second-secret", 400)),
            ]),
        };
        let mut owner = SessionOwner::new(
            issuer,
            FakeFactory::default(),
            capability(),
            SessionConfig::default(),
        )
        .expect("valid owner")
        .with_renew_before_seconds(15);

        owner.establish().expect("establish");
        owner.reconnect(100).expect("reuse fresh credential");
        assert_eq!(owner.credential_expires_at(), Some(200));

        owner.reconnect(190).expect("renew stale credential");
        assert_eq!(owner.credential_expires_at(), Some(400));
    }

    #[test]
    fn mute_intent_survives_credential_renewal() {
        let issuer = FakeIssuer {
            issued: VecDeque::from([
                Ok(credential("first-secret", 120)),
                Ok(credential("second-secret", 240)),
            ]),
        };
        let mut owner = SessionOwner::new(
            issuer,
            FakeFactory::default(),
            capability(),
            SessionConfig::default(),
        )
        .expect("valid owner")
        .with_renew_before_seconds(15);

        owner.activate(100).expect("activate");
        owner.set_muted(true, 101).expect("mute");
        owner.ensure_fresh(106).expect("renew");

        let error = owner
            .queue_input_audio("cGNt".to_owned(), 110)
            .expect_err("renewed transport must remain muted");
        assert_eq!(error, SessionOwnerError::Transport(TransportError::Muted));
    }
}
