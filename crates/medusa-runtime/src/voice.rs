//! Provider-neutral realtime voice session state and events.
//!
//! This module owns no microphone, speaker, transport, UI, or provider
//! implementation. Frontends and transports drive the same state machine and
//! register resources that are deterministically released on close.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Default upper bound for queued audio frames in either direction.
pub const DEFAULT_MAX_AUDIO_FRAMES: usize = 64;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RealtimeVoiceState {
    Idle,
    Connecting,
    Listening,
    UserSpeaking,
    Thinking,
    ToolRunning,
    AwaitingApproval,
    AssistantSpeaking,
    Interrupted,
    Reconnecting,
    Failed,
    Closed,
}

impl RealtimeVoiceState {
    #[must_use]
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Closed)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct VoiceAvailability {
    pub available: bool,
    pub reason: Option<String>,
    pub supports_input_audio: bool,
    pub supports_output_audio: bool,
    pub supports_barge_in: bool,
}

impl VoiceAvailability {
    #[must_use]
    pub fn unavailable(reason: impl Into<String>) -> Self {
        Self {
            available: false,
            reason: Some(reason.into()),
            supports_input_audio: false,
            supports_output_audio: false,
            supports_barge_in: false,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TranscriptSpeaker {
    User,
    Assistant,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TranscriptUpdate {
    pub turn_id: String,
    pub speaker: TranscriptSpeaker,
    pub text: String,
    pub is_final: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct VoiceError {
    pub code: String,
    pub message: String,
    pub retryable: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RealtimeVoiceEvent {
    StateChanged {
        from: RealtimeVoiceState,
        to: RealtimeVoiceState,
    },
    AvailabilityChanged(VoiceAvailability),
    InputAudioQueued {
        frames: usize,
    },
    OutputAudioQueued {
        frames: usize,
    },
    Transcript(TranscriptUpdate),
    VoiceActivity {
        active: bool,
    },
    Interrupted,
    TransportStatus {
        connected: bool,
        detail: Option<String>,
    },
    ToolActivity {
        tool: String,
        active: bool,
    },
    ApprovalRequired {
        request_id: String,
        summary: String,
    },
    Error(VoiceError),
    Closed,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AudioFrame {
    pub sequence: u64,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum VoiceSessionError {
    #[error("invalid realtime voice transition from {from:?} to {to:?}")]
    InvalidTransition {
        from: RealtimeVoiceState,
        to: RealtimeVoiceState,
    },
    #[error("realtime voice session is closed")]
    Closed,
    #[error("audio queue capacity must be greater than zero")]
    InvalidAudioCapacity,
    #[error("voice resource `{resource}` failed to close: {message}")]
    ResourceClose { resource: String, message: String },
}

/// A frontend or transport resource owned for the lifetime of a voice session.
pub trait VoiceResource: Send {
    fn name(&self) -> &str;
    fn close(&mut self) -> Result<(), String>;
}

/// Persists final voice turns into the same ordered conversation used by text.
pub trait VoiceTurnSink: Send {
    fn append_voice_turn(
        &mut self,
        turn_id: &str,
        speaker: TranscriptSpeaker,
        text: &str,
    ) -> Result<(), String>;
}

struct BoundedAudioQueue {
    capacity: usize,
    frames: VecDeque<AudioFrame>,
}

impl BoundedAudioQueue {
    fn new(capacity: usize) -> Result<Self, VoiceSessionError> {
        if capacity == 0 {
            return Err(VoiceSessionError::InvalidAudioCapacity);
        }
        Ok(Self {
            capacity,
            frames: VecDeque::with_capacity(capacity),
        })
    }

    fn push(&mut self, frame: AudioFrame) {
        if self.frames.len() == self.capacity {
            self.frames.pop_front();
        }
        self.frames.push_back(frame);
    }

    fn pop(&mut self) -> Option<AudioFrame> {
        self.frames.pop_front()
    }

    fn clear(&mut self) {
        self.frames.clear();
    }

    fn len(&self) -> usize {
        self.frames.len()
    }
}

/// Shared session state driven by desktop, TUI, and provider transports.
pub struct RealtimeVoiceSession {
    state: RealtimeVoiceState,
    availability: VoiceAvailability,
    input_audio: BoundedAudioQueue,
    output_audio: BoundedAudioQueue,
    transcripts: BTreeMap<(String, TranscriptSpeaker), String>,
    persisted_turns: BTreeSet<(String, TranscriptSpeaker)>,
    events: VecDeque<RealtimeVoiceEvent>,
    resources: Vec<Box<dyn VoiceResource>>,
    turn_sink: Option<Box<dyn VoiceTurnSink>>,
}

impl fmt::Debug for RealtimeVoiceSession {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RealtimeVoiceSession")
            .field("state", &self.state)
            .field("availability", &self.availability)
            .field("input_audio_frames", &self.input_audio.len())
            .field("output_audio_frames", &self.output_audio.len())
            .field("transcript_count", &self.transcripts.len())
            .field("pending_events", &self.events.len())
            .field("resource_count", &self.resources.len())
            .finish()
    }
}

impl RealtimeVoiceSession {
    pub fn new(availability: VoiceAvailability) -> Result<Self, VoiceSessionError> {
        Self::with_audio_capacity(availability, DEFAULT_MAX_AUDIO_FRAMES)
    }

    pub fn with_audio_capacity(
        availability: VoiceAvailability,
        capacity: usize,
    ) -> Result<Self, VoiceSessionError> {
        Ok(Self {
            state: RealtimeVoiceState::Idle,
            availability,
            input_audio: BoundedAudioQueue::new(capacity)?,
            output_audio: BoundedAudioQueue::new(capacity)?,
            transcripts: BTreeMap::new(),
            persisted_turns: BTreeSet::new(),
            events: VecDeque::new(),
            resources: Vec::new(),
            turn_sink: None,
        })
    }

    #[must_use]
    pub fn state(&self) -> RealtimeVoiceState {
        self.state
    }

    #[must_use]
    pub fn availability(&self) -> &VoiceAvailability {
        &self.availability
    }

    pub fn set_availability(&mut self, availability: VoiceAvailability) {
        self.availability = availability.clone();
        self.events
            .push_back(RealtimeVoiceEvent::AvailabilityChanged(availability));
    }

    pub fn set_turn_sink(&mut self, sink: Box<dyn VoiceTurnSink>) {
        self.turn_sink = Some(sink);
    }

    /// Registers a resource, or closes it immediately when setup finishes after shutdown.
    pub fn register_resource(
        &mut self,
        mut resource: Box<dyn VoiceResource>,
    ) -> Result<(), VoiceSessionError> {
        if self.state == RealtimeVoiceState::Closed {
            let name = resource.name().to_owned();
            return match resource.close() {
                Ok(()) => Err(VoiceSessionError::Closed),
                Err(message) => Err(VoiceSessionError::ResourceClose {
                    resource: name,
                    message,
                }),
            };
        }
        self.resources.push(resource);
        Ok(())
    }

    pub fn transition(&mut self, to: RealtimeVoiceState) -> Result<(), VoiceSessionError> {
        self.ensure_open()?;
        if !valid_transition(self.state, to) {
            return Err(VoiceSessionError::InvalidTransition {
                from: self.state,
                to,
            });
        }
        let from = self.state;
        self.state = to;
        self.events
            .push_back(RealtimeVoiceEvent::StateChanged { from, to });
        Ok(())
    }

    pub fn queue_input_audio(&mut self, frame: AudioFrame) -> Result<(), VoiceSessionError> {
        self.ensure_open()?;
        self.input_audio.push(frame);
        self.coalesce_audio_event(true, self.input_audio.len());
        Ok(())
    }

    pub fn queue_output_audio(&mut self, frame: AudioFrame) -> Result<(), VoiceSessionError> {
        self.ensure_open()?;
        self.output_audio.push(frame);
        self.coalesce_audio_event(false, self.output_audio.len());
        Ok(())
    }

    fn coalesce_audio_event(&mut self, input: bool, frames: usize) {
        let replacement = if input {
            RealtimeVoiceEvent::InputAudioQueued { frames }
        } else {
            RealtimeVoiceEvent::OutputAudioQueued { frames }
        };
        if let Some(existing) = self.events.iter_mut().rev().find(|event| {
            matches!(
                (input, &**event),
                (true, RealtimeVoiceEvent::InputAudioQueued { .. })
                    | (false, RealtimeVoiceEvent::OutputAudioQueued { .. })
            )
        }) {
            *existing = replacement;
        } else {
            self.events.push_back(replacement);
        }
    }

    pub fn take_input_audio(&mut self) -> Option<AudioFrame> {
        self.input_audio.pop()
    }

    pub fn take_output_audio(&mut self) -> Option<AudioFrame> {
        self.output_audio.pop()
    }

    #[must_use]
    pub fn input_audio_len(&self) -> usize {
        self.input_audio.len()
    }

    #[must_use]
    pub fn output_audio_len(&self) -> usize {
        self.output_audio.len()
    }

    pub fn update_transcript(&mut self, update: TranscriptUpdate) -> Result<(), VoiceSessionError> {
        self.ensure_open()?;
        let key = (update.turn_id.clone(), update.speaker);
        self.transcripts.insert(key.clone(), update.text.clone());
        if update.is_final && !self.persisted_turns.contains(&key) {
            let persisted = if let Some(sink) = self.turn_sink.as_mut() {
                match sink.append_voice_turn(&update.turn_id, update.speaker, &update.text) {
                    Ok(()) => true,
                    Err(message) => {
                        self.events.push_back(RealtimeVoiceEvent::Error(VoiceError {
                            code: "conversation_append_failed".to_owned(),
                            message,
                            retryable: true,
                        }));
                        false
                    }
                }
            } else {
                false
            };
            if persisted {
                self.persisted_turns.insert(key);
            }
        }
        self.events
            .push_back(RealtimeVoiceEvent::Transcript(update));
        Ok(())
    }

    #[must_use]
    pub fn transcript(&self, turn_id: &str, speaker: TranscriptSpeaker) -> Option<&str> {
        self.transcripts
            .get(&(turn_id.to_owned(), speaker))
            .map(String::as_str)
    }

    pub fn emit(&mut self, event: RealtimeVoiceEvent) -> Result<(), VoiceSessionError> {
        self.ensure_open()?;
        self.events.push_back(event);
        Ok(())
    }

    pub fn next_event(&mut self) -> Option<RealtimeVoiceEvent> {
        self.events.pop_front()
    }

    /// Interrupts current speech or processing without creating a new session.
    pub fn barge_in(&mut self) -> Result<(), VoiceSessionError> {
        self.transition(RealtimeVoiceState::Interrupted)?;
        self.output_audio.clear();
        self.events.push_back(RealtimeVoiceEvent::Interrupted);
        Ok(())
    }

    /// Closes all resources in reverse registration order and clears raw audio.
    pub fn close(&mut self) -> Result<(), VoiceSessionError> {
        if self.state == RealtimeVoiceState::Closed {
            return Ok(());
        }
        let mut first_error = None;
        for resource in self.resources.iter_mut().rev() {
            if let Err(message) = resource.close()
                && first_error.is_none()
            {
                first_error = Some(VoiceSessionError::ResourceClose {
                    resource: resource.name().to_owned(),
                    message,
                });
            }
        }
        self.resources.clear();
        self.input_audio.clear();
        self.output_audio.clear();
        let from = self.state;
        self.state = RealtimeVoiceState::Closed;
        self.events.retain(|event| {
            !matches!(
                event,
                RealtimeVoiceEvent::InputAudioQueued { .. }
                    | RealtimeVoiceEvent::OutputAudioQueued { .. }
            )
        });
        self.events.push_back(RealtimeVoiceEvent::StateChanged {
            from,
            to: RealtimeVoiceState::Closed,
        });
        self.events.push_back(RealtimeVoiceEvent::Closed);
        match first_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    fn ensure_open(&self) -> Result<(), VoiceSessionError> {
        if self.state == RealtimeVoiceState::Closed {
            Err(VoiceSessionError::Closed)
        } else {
            Ok(())
        }
    }
}

impl Drop for RealtimeVoiceSession {
    fn drop(&mut self) {
        let _ = self.close();
    }
}

fn valid_transition(from: RealtimeVoiceState, to: RealtimeVoiceState) -> bool {
    use RealtimeVoiceState as State;
    if to == State::Closed {
        return true;
    }
    matches!(
        (from, to),
        (State::Idle, State::Connecting)
            | (
                State::Connecting,
                State::Listening | State::Reconnecting | State::Failed
            )
            | (
                State::Listening,
                State::UserSpeaking | State::Reconnecting | State::Failed
            )
            | (
                State::UserSpeaking,
                State::Thinking | State::Interrupted | State::Failed
            )
            | (
                State::Thinking,
                State::ToolRunning
                    | State::AwaitingApproval
                    | State::AssistantSpeaking
                    | State::Interrupted
                    | State::Failed
            )
            | (
                State::ToolRunning,
                State::Thinking
                    | State::AwaitingApproval
                    | State::AssistantSpeaking
                    | State::Interrupted
                    | State::Failed
            )
            | (
                State::AwaitingApproval,
                State::Thinking | State::ToolRunning | State::Interrupted | State::Failed
            )
            | (
                State::AssistantSpeaking,
                State::Listening | State::Interrupted | State::Failed
            )
            | (
                State::Interrupted,
                State::Listening | State::Thinking | State::Reconnecting | State::Failed
            )
            | (State::Reconnecting, State::Listening | State::Failed)
            | (State::Failed, State::Reconnecting | State::Idle)
    )
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;

    fn availability() -> VoiceAvailability {
        VoiceAvailability {
            available: true,
            reason: None,
            supports_input_audio: true,
            supports_output_audio: true,
            supports_barge_in: true,
        }
    }

    #[test]
    fn supports_deterministic_lifecycle_and_rejects_invalid_transitions() {
        let mut session = RealtimeVoiceSession::new(availability()).expect("session");
        session
            .transition(RealtimeVoiceState::Connecting)
            .expect("connect");
        session
            .transition(RealtimeVoiceState::Listening)
            .expect("listen");
        assert_eq!(
            session.transition(RealtimeVoiceState::AssistantSpeaking),
            Err(VoiceSessionError::InvalidTransition {
                from: RealtimeVoiceState::Listening,
                to: RealtimeVoiceState::AssistantSpeaking,
            })
        );
    }

    #[test]
    fn audio_queues_and_notifications_are_bounded() {
        let mut session =
            RealtimeVoiceSession::with_audio_capacity(availability(), 2).expect("session");
        for sequence in 1..=100 {
            session
                .queue_input_audio(AudioFrame {
                    sequence,
                    bytes: vec![sequence as u8],
                })
                .expect("queue");
        }
        assert_eq!(session.input_audio_len(), 2);
        let events = std::iter::from_fn(|| session.next_event()).collect::<Vec<_>>();
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, RealtimeVoiceEvent::InputAudioQueued { .. }))
                .count(),
            1
        );
        assert!(matches!(
            events.last(),
            Some(RealtimeVoiceEvent::InputAudioQueued { frames: 2 })
        ));
    }

    struct RecordingSink(Arc<Mutex<Vec<(String, TranscriptSpeaker, String)>>>);

    impl VoiceTurnSink for RecordingSink {
        fn append_voice_turn(
            &mut self,
            turn_id: &str,
            speaker: TranscriptSpeaker,
            text: &str,
        ) -> Result<(), String> {
            self.0.lock().expect("recording sink").push((
                turn_id.to_owned(),
                speaker,
                text.to_owned(),
            ));
            Ok(())
        }
    }

    #[test]
    fn repeated_final_transcripts_are_persisted_once() {
        let recorded = Arc::new(Mutex::new(Vec::new()));
        let mut session = RealtimeVoiceSession::new(availability()).expect("session");
        session.set_turn_sink(Box::new(RecordingSink(Arc::clone(&recorded))));
        for text in ["fix the test", "fix the test"] {
            session
                .update_transcript(TranscriptUpdate {
                    turn_id: "turn-1".to_owned(),
                    speaker: TranscriptSpeaker::User,
                    text: text.to_owned(),
                    is_final: true,
                })
                .expect("final");
        }
        assert_eq!(recorded.lock().expect("recorded").len(), 1);
    }

    struct RecordingResource {
        name: &'static str,
        closed: Arc<Mutex<Vec<&'static str>>>,
        fail: bool,
    }

    impl VoiceResource for RecordingResource {
        fn name(&self) -> &str {
            self.name
        }

        fn close(&mut self) -> Result<(), String> {
            self.closed
                .lock()
                .expect("closed resources")
                .push(self.name);
            if self.fail {
                Err("close failed".to_owned())
            } else {
                Ok(())
            }
        }
    }

    #[test]
    fn close_releases_every_resource_in_reverse_order_and_clears_audio() {
        let closed = Arc::new(Mutex::new(Vec::new()));
        let mut session = RealtimeVoiceSession::new(availability()).expect("session");
        session
            .register_resource(Box::new(RecordingResource {
                name: "microphone",
                closed: Arc::clone(&closed),
                fail: true,
            }))
            .expect("register microphone");
        session
            .register_resource(Box::new(RecordingResource {
                name: "transport",
                closed: Arc::clone(&closed),
                fail: false,
            }))
            .expect("register transport");
        assert!(matches!(
            session.close(),
            Err(VoiceSessionError::ResourceClose { .. })
        ));
        assert_eq!(
            closed.lock().expect("closed order").as_slice(),
            &["transport", "microphone"]
        );
    }

    #[test]
    fn resource_registered_after_close_is_closed_immediately() {
        let closed = Arc::new(Mutex::new(Vec::new()));
        let mut session = RealtimeVoiceSession::new(availability()).expect("session");
        session.close().expect("close session");
        assert_eq!(
            session.register_resource(Box::new(RecordingResource {
                name: "late-stream",
                closed: Arc::clone(&closed),
                fail: false,
            })),
            Err(VoiceSessionError::Closed)
        );
        assert_eq!(closed.lock().expect("closed").as_slice(), &["late-stream"]);
    }
}
