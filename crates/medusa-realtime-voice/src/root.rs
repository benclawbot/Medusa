//! Provider-neutral realtime voice session state and events.

use std::collections::VecDeque;

use medusa_core::SessionId;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Realtime voice lifecycle shared by all frontends and transports.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VoiceState {
    Idle,
    Connecting,
    Listening,
    UserSpeaking,
    Thinking,
    ToolRunning,
    ApprovalRequired,
    AssistantSpeaking,
    Interrupted,
    Reconnecting,
    Failed,
    Closed,
}

/// Availability exposed to desktop and TUI clients.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct VoiceAvailability {
    pub available: bool,
    pub reason: Option<String>,
}

/// Transcript role in the shared conversation.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Speaker {
    User,
    Assistant,
}

/// A finalized conversation turn. Partial transcripts are never persisted here.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ConversationTurn {
    pub speaker: Speaker,
    pub text: String,
    pub source: TurnSource,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnSource {
    Typed,
    Voice,
}

/// Frontend-neutral events emitted by the voice core.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum VoiceEvent {
    StateChanged { from: VoiceState, to: VoiceState },
    AudioInput { bytes: usize },
    AudioOutput { bytes: usize },
    PartialTranscript { speaker: Speaker, text: String },
    FinalTranscript { speaker: Speaker, text: String },
    VoiceActivity { active: bool },
    Interrupted,
    TransportStatus { connected: bool },
    Error { message: String },
    ToolActivity { active: bool, label: Option<String> },
    ApprovalRequired { summary: String },
    Closed,
}

/// Transport abstraction; concrete WebRTC/WebSocket implementations live elsewhere.
pub trait VoiceTransport {
    fn close(&mut self);
}

/// Device abstraction; microphone and speaker implementations live in frontends.
pub trait VoiceDevice {
    fn stop(&mut self);
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum VoiceError {
    #[error("invalid voice state transition from {from:?} to {to:?}")]
    InvalidTransition { from: VoiceState, to: VoiceState },
    #[error("voice session is closed")]
    Closed,
}

/// Shared realtime voice session with bounded transient audio metadata.
pub struct RealtimeVoiceSession<T: VoiceTransport, D: VoiceDevice> {
    session_id: SessionId,
    state: VoiceState,
    availability: VoiceAvailability,
    transport: T,
    device: D,
    audio_window: VecDeque<usize>,
    max_audio_chunks: usize,
    partial_user: Option<String>,
    partial_assistant: Option<String>,
    conversation: Vec<ConversationTurn>,
}

impl<T: VoiceTransport, D: VoiceDevice> RealtimeVoiceSession<T, D> {
    pub fn new(
        session_id: SessionId,
        transport: T,
        device: D,
        max_audio_chunks: usize,
        availability: VoiceAvailability,
    ) -> Self {
        Self {
            session_id,
            state: VoiceState::Idle,
            availability,
            transport,
            device,
            audio_window: VecDeque::with_capacity(max_audio_chunks.max(1)),
            max_audio_chunks: max_audio_chunks.max(1),
            partial_user: None,
            partial_assistant: None,
            conversation: Vec::new(),
        }
    }

    #[must_use]
    pub fn state(&self) -> VoiceState { self.state }

    #[must_use]
    pub fn session_id(&self) -> &SessionId { &self.session_id }

    #[must_use]
    pub fn availability(&self) -> &VoiceAvailability { &self.availability }

    #[must_use]
    pub fn conversation(&self) -> &[ConversationTurn] { &self.conversation }

    #[must_use]
    pub fn buffered_audio_chunks(&self) -> usize { self.audio_window.len() }

    pub fn transition(&mut self, to: VoiceState) -> Result<VoiceEvent, VoiceError> {
        if self.state == VoiceState::Closed { return Err(VoiceError::Closed); }
        if !valid_transition(self.state, to) {
            return Err(VoiceError::InvalidTransition { from: self.state, to });
        }
        let from = self.state;
        self.state = to;
        Ok(VoiceEvent::StateChanged { from, to })
    }

    pub fn push_audio_chunk(&mut self, bytes: usize) -> Result<VoiceEvent, VoiceError> {
        if self.state == VoiceState::Closed { return Err(VoiceError::Closed); }
        if self.audio_window.len() == self.max_audio_chunks { self.audio_window.pop_front(); }
        self.audio_window.push_back(bytes);
        Ok(VoiceEvent::AudioInput { bytes })
    }

    pub fn update_partial(&mut self, speaker: Speaker, text: impl Into<String>) -> Result<VoiceEvent, VoiceError> {
        if self.state == VoiceState::Closed { return Err(VoiceError::Closed); }
        let text = text.into();
        match speaker {
            Speaker::User => self.partial_user = Some(text.clone()),
            Speaker::Assistant => self.partial_assistant = Some(text.clone()),
        }
        Ok(VoiceEvent::PartialTranscript { speaker, text })
    }

    pub fn finalize_transcript(&mut self, speaker: Speaker, text: impl Into<String>) -> Result<VoiceEvent, VoiceError> {
        if self.state == VoiceState::Closed { return Err(VoiceError::Closed); }
        let text = text.into();
        match speaker {
            Speaker::User => self.partial_user = None,
            Speaker::Assistant => self.partial_assistant = None,
        }
        self.conversation.push(ConversationTurn { speaker, text: text.clone(), source: TurnSource::Voice });
        Ok(VoiceEvent::FinalTranscript { speaker, text })
    }

    pub fn add_typed_turn(&mut self, speaker: Speaker, text: impl Into<String>) -> Result<(), VoiceError> {
        if self.state == VoiceState::Closed { return Err(VoiceError::Closed); }
        self.conversation.push(ConversationTurn { speaker, text: text.into(), source: TurnSource::Typed });
        Ok(())
    }

    pub fn close(&mut self) -> VoiceEvent {
        if self.state != VoiceState::Closed {
            self.transport.close();
            self.device.stop();
            self.audio_window.clear();
            self.partial_user = None;
            self.partial_assistant = None;
            self.state = VoiceState::Closed;
        }
        VoiceEvent::Closed
    }
}

impl<T: VoiceTransport, D: VoiceDevice> Drop for RealtimeVoiceSession<T, D> {
    fn drop(&mut self) {
        if self.state != VoiceState::Closed {
            self.transport.close();
            self.device.stop();
        }
    }
}

fn valid_transition(from: VoiceState, to: VoiceState) -> bool {
    use VoiceState::*;
    matches!((from, to),
        (Idle, Connecting) | (Idle, Closed) |
        (Connecting, Listening) | (Connecting, Failed) | (Connecting, Closed) |
        (Listening, UserSpeaking) | (Listening, Reconnecting) | (Listening, Closed) |
        (UserSpeaking, Thinking) | (UserSpeaking, Interrupted) | (UserSpeaking, Closed) |
        (Thinking, ToolRunning) | (Thinking, ApprovalRequired) | (Thinking, AssistantSpeaking) | (Thinking, Interrupted) | (Thinking, Failed) | (Thinking, Closed) |
        (ToolRunning, Thinking) | (ToolRunning, ApprovalRequired) | (ToolRunning, AssistantSpeaking) | (ToolRunning, Interrupted) | (ToolRunning, Failed) | (ToolRunning, Closed) |
        (ApprovalRequired, Thinking) | (ApprovalRequired, ToolRunning) | (ApprovalRequired, Interrupted) | (ApprovalRequired, Closed) |
        (AssistantSpeaking, Listening) | (AssistantSpeaking, Interrupted) | (AssistantSpeaking, Closed) |
        (Interrupted, Listening) | (Interrupted, Thinking) | (Interrupted, Closed) |
        (Reconnecting, Listening) | (Reconnecting, Failed) | (Reconnecting, Closed) |
        (Failed, Reconnecting) | (Failed, Closed)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};

    struct Resource(Arc<AtomicUsize>);
    impl VoiceTransport for Resource { fn close(&mut self) { self.0.fetch_add(1, Ordering::SeqCst); } }
    impl VoiceDevice for Resource { fn stop(&mut self) { self.0.fetch_add(1, Ordering::SeqCst); } }

    fn session(counter: Arc<AtomicUsize>) -> RealtimeVoiceSession<Resource, Resource> {
        RealtimeVoiceSession::new(SessionId::new(), Resource(counter.clone()), Resource(counter), 2, VoiceAvailability { available: true, reason: None })
    }

    #[test]
    fn deterministic_transitions_cover_tool_approval_interrupt_and_reconnect() {
        let counter = Arc::new(AtomicUsize::new(0));
        let mut session = session(counter);
        for state in [VoiceState::Connecting, VoiceState::Listening, VoiceState::UserSpeaking, VoiceState::Thinking, VoiceState::ToolRunning, VoiceState::ApprovalRequired, VoiceState::Interrupted, VoiceState::Listening, VoiceState::Reconnecting, VoiceState::Listening] {
            session.transition(state).expect("valid transition");
        }
    }

    #[test]
    fn invalid_transition_is_rejected() {
        let counter = Arc::new(AtomicUsize::new(0));
        let mut session = session(counter);
        assert_eq!(session.transition(VoiceState::AssistantSpeaking), Err(VoiceError::InvalidTransition { from: VoiceState::Idle, to: VoiceState::AssistantSpeaking }));
    }

    #[test]
    fn partial_transcripts_do_not_duplicate_conversation() {
        let counter = Arc::new(AtomicUsize::new(0));
        let mut session = session(counter);
        session.update_partial(Speaker::User, "hel").expect("partial");
        session.update_partial(Speaker::User, "hello").expect("partial");
        assert!(session.conversation().is_empty());
        session.finalize_transcript(Speaker::User, "hello").expect("final");
        assert_eq!(session.conversation().len(), 1);
    }

    #[test]
    fn typed_and_voice_turns_share_ordered_history() {
        let counter = Arc::new(AtomicUsize::new(0));
        let mut session = session(counter);
        session.add_typed_turn(Speaker::User, "typed").expect("typed");
        session.finalize_transcript(Speaker::Assistant, "spoken").expect("voice");
        assert_eq!(session.conversation()[0].source, TurnSource::Typed);
        assert_eq!(session.conversation()[1].source, TurnSource::Voice);
    }

    #[test]
    fn audio_buffer_is_bounded_and_close_releases_resources() {
        let counter = Arc::new(AtomicUsize::new(0));
        let mut session = session(counter.clone());
        for bytes in [1, 2, 3] { session.push_audio_chunk(bytes).expect("audio"); }
        assert_eq!(session.buffered_audio_chunks(), 2);
        session.close();
        assert_eq!(session.buffered_audio_chunks(), 0);
        assert_eq!(counter.load(Ordering::SeqCst), 2);
        assert_eq!(session.transition(VoiceState::Idle), Err(VoiceError::Closed));
    }
}
