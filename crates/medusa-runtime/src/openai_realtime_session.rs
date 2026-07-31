//! Runtime ownership for authenticated OpenAI Realtime sessions.
//!
//! Short-lived credentials stay inside the trusted runtime. The owner renews
//! credentials before expiry, rebuilds the wire when required, and preserves
//! activation and mute intent without exposing secrets to frontends.

use medusa_openai_realtime::{GatewayCapability, SessionConfig, Transport, TransportError, Wire};
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
        self.expires_at <= now_seconds.saturating_add(renew_before_seconds)
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

    #[must_use]
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
        let credential = self.issuer.issue().map_err(SessionOwnerError::Credential)?;
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
        let needs_renewal = self.credential.as_ref().is_none_or(|credential| {
            credential.needs_renewal(now_seconds, self.renew_before_seconds)
        });
        if needs_renewal {
            self.rebuild()?;
        }
        Ok(())
    }

    pub fn activate(&mut self, now_seconds: u64) -> Result<(), SessionOwnerError> {
        self.ensure_fresh(now_seconds)?;
        if !self.activated {
            self.transport_mut()?.activate()?;
            self.activated = true;
            self.muted = false;
        }
        Ok(())
    }

    pub fn set_muted(&mut self, muted: bool, now_seconds: u64) -> Result<(), SessionOwnerError> {
        self.ensure_fresh(now_seconds)?;
        self.transport_mut()?.set_muted(muted)?;
        self.muted = muted;
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
        let renew = self.credential.as_ref().is_none_or(|credential| {
            credential.needs_renewal(now_seconds, self.renew_before_seconds)
        });
        if renew {
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
        self.credential = None;
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

    use medusa_openai_realtime::WireKind;
    use serde_json::Value;

    use super::*;

    #[derive(Default)]
    struct FakeIssuer(VecDeque<Result<SessionCredential, String>>);

    impl CredentialIssuer for FakeIssuer {
        fn issue(&mut self) -> Result<SessionCredential, String> {
            self.0
                .pop_front()
                .unwrap_or_else(|| Err("no credential scripted".to_owned()))
        }
    }

    #[derive(Default)]
    struct FakeFactory;

    impl WireFactory<FakeWire> for FakeFactory {
        fn connect(
            &mut self,
            _capability: &GatewayCapability,
            _credential: &SessionCredential,
        ) -> Result<FakeWire, String> {
            Ok(FakeWire)
        }
    }

    struct FakeWire;

    impl Wire for FakeWire {
        fn send_json(&mut self, _payload: Value) -> Result<(), String> {
            Ok(())
        }

        fn receive_json(&mut self) -> Result<Option<Value>, String> {
            Ok(None)
        }

        fn reconnect(&mut self) -> Result<(), String> {
            Ok(())
        }

        fn close(&mut self) -> Result<(), String> {
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

    fn owner(
        issued: VecDeque<Result<SessionCredential, String>>,
    ) -> SessionOwner<FakeIssuer, FakeFactory, FakeWire> {
        SessionOwner::new(
            FakeIssuer(issued),
            FakeFactory,
            capability(),
            SessionConfig::default(),
        )
        .expect("valid owner")
        .with_renew_before_seconds(15)
    }

    #[test]
    fn activation_is_fail_closed_until_establishment_succeeds() {
        let mut owner = owner(VecDeque::from([Err("OAuth unavailable".to_owned())]));
        assert_eq!(
            owner.activate(100),
            Err(SessionOwnerError::Credential(
                "OAuth unavailable".to_owned()
            ))
        );
        assert!(!owner.is_established());
    }

    #[test]
    fn renewal_preserves_activation_and_mute_intent() {
        let mut owner = owner(VecDeque::from([
            Ok(credential("first", 120)),
            Ok(credential("second", 240)),
        ]));
        owner.activate(100).expect("activate");
        owner.set_muted(true, 101).expect("mute");
        owner.ensure_fresh(106).expect("renew");
        assert_eq!(owner.credential_expires_at(), Some(240));
        assert_eq!(
            owner.queue_input_audio("cGNt".to_owned(), 110),
            Err(SessionOwnerError::Transport(TransportError::Muted))
        );
    }

    #[test]
    fn reconnect_renews_only_when_credential_is_near_expiry() {
        let mut owner = owner(VecDeque::from([
            Ok(credential("first", 200)),
            Ok(credential("second", 400)),
        ]));
        owner.establish().expect("establish");
        owner.reconnect(100).expect("reuse fresh credential");
        assert_eq!(owner.credential_expires_at(), Some(200));
        owner.reconnect(190).expect("renew stale credential");
        assert_eq!(owner.credential_expires_at(), Some(400));
    }
}
