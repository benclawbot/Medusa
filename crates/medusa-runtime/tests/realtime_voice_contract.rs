use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use std::time::{Duration, Instant};

use medusa_runtime::voice::{
    AudioFrame, RealtimeVoiceEvent, RealtimeVoiceSession, RealtimeVoiceState, TranscriptSpeaker,
    TranscriptUpdate, VoiceAvailability, VoiceResource,
};

const BARGE_IN_BUDGET: Duration = Duration::from_millis(20);
const LIFECYCLE_CYCLES: usize = 250;

fn available() -> VoiceAvailability {
    VoiceAvailability {
        available: true,
        reason: None,
        supports_input_audio: true,
        supports_output_audio: true,
        supports_barge_in: true,
    }
}

fn speaking_session(capacity: usize) -> RealtimeVoiceSession {
    let mut session =
        RealtimeVoiceSession::with_audio_capacity(available(), capacity).expect("voice session");
    for state in [
        RealtimeVoiceState::Connecting,
        RealtimeVoiceState::Listening,
        RealtimeVoiceState::UserSpeaking,
        RealtimeVoiceState::Thinking,
        RealtimeVoiceState::AssistantSpeaking,
    ] {
        session
            .transition(state)
            .expect("valid lifecycle transition");
    }
    session
}

#[test]
fn barge_in_clears_playback_within_the_controlled_latency_budget() {
    let mut session = speaking_session(64);
    for sequence in 0..64 {
        session
            .queue_output_audio(AudioFrame {
                sequence,
                bytes: vec![0; 960],
            })
            .expect("queue output audio");
    }

    let started = Instant::now();
    session.barge_in().expect("barge in");
    let elapsed = started.elapsed();

    assert!(
        elapsed <= BARGE_IN_BUDGET,
        "barge-in took {elapsed:?}, budget is {BARGE_IN_BUDGET:?}"
    );
    assert_eq!(session.state(), RealtimeVoiceState::Interrupted);
    assert_eq!(session.output_audio_len(), 0);
}

#[test]
fn stopping_playback_does_not_cancel_the_coding_task() {
    let task_running = Arc::new(AtomicBool::new(true));
    let task_probe = Arc::clone(&task_running);
    let mut session = speaking_session(8);
    session
        .queue_output_audio(AudioFrame {
            sequence: 1,
            bytes: vec![1; 960],
        })
        .expect("queue output audio");

    session.barge_in().expect("stop playback");

    assert!(task_probe.load(Ordering::SeqCst));
    assert_eq!(session.output_audio_len(), 0);
    assert_eq!(session.state(), RealtimeVoiceState::Interrupted);
}

#[test]
fn ordered_voice_text_tool_approval_and_result_events_are_preserved() {
    let mut session = RealtimeVoiceSession::new(available()).expect("voice session");
    session
        .transition(RealtimeVoiceState::Connecting)
        .expect("connecting");
    session
        .transition(RealtimeVoiceState::Listening)
        .expect("listening");
    session
        .update_transcript(TranscriptUpdate {
            turn_id: "turn-user".to_owned(),
            speaker: TranscriptSpeaker::User,
            text: "run the verification".to_owned(),
            is_final: true,
        })
        .expect("user transcript");
    session
        .transition(RealtimeVoiceState::UserSpeaking)
        .expect("user speaking");
    session
        .transition(RealtimeVoiceState::Thinking)
        .expect("thinking");
    session
        .emit(RealtimeVoiceEvent::ToolActivity {
            tool: "cargo test".to_owned(),
            active: true,
        })
        .expect("tool event");
    session
        .transition(RealtimeVoiceState::AwaitingApproval)
        .expect("approval state");
    session
        .emit(RealtimeVoiceEvent::ApprovalRequired {
            request_id: "approval-1".to_owned(),
            summary: "Allow verification command".to_owned(),
        })
        .expect("approval event");
    session
        .transition(RealtimeVoiceState::Thinking)
        .expect("resume thinking");
    session
        .transition(RealtimeVoiceState::AssistantSpeaking)
        .expect("assistant speaking");
    session
        .update_transcript(TranscriptUpdate {
            turn_id: "turn-assistant".to_owned(),
            speaker: TranscriptSpeaker::Assistant,
            text: "Verification passed".to_owned(),
            is_final: true,
        })
        .expect("assistant transcript");

    let events = std::iter::from_fn(|| session.next_event()).collect::<Vec<_>>();
    let user_index = events
        .iter()
        .position(|event| matches!(event, RealtimeVoiceEvent::Transcript(update) if update.turn_id == "turn-user"))
        .expect("user transcript event");
    let tool_index = events
        .iter()
        .position(|event| matches!(event, RealtimeVoiceEvent::ToolActivity { active: true, .. }))
        .expect("tool event");
    let approval_index = events
        .iter()
        .position(|event| matches!(event, RealtimeVoiceEvent::ApprovalRequired { request_id, .. } if request_id == "approval-1"))
        .expect("approval event");
    let result_index = events
        .iter()
        .position(|event| matches!(event, RealtimeVoiceEvent::Transcript(update) if update.turn_id == "turn-assistant"))
        .expect("assistant result event");

    assert!(user_index < tool_index);
    assert!(tool_index < approval_index);
    assert!(approval_index < result_index);
}

struct CountedResource(Arc<AtomicUsize>);

impl VoiceResource for CountedResource {
    fn name(&self) -> &str {
        "synthetic-audio-handle"
    }

    fn close(&mut self) -> Result<(), String> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

#[test]
fn rapid_connect_disconnect_cycles_release_every_handle_and_buffer() {
    let closed = Arc::new(AtomicUsize::new(0));

    for cycle in 0..LIFECYCLE_CYCLES {
        let mut session =
            RealtimeVoiceSession::with_audio_capacity(available(), 4).expect("voice session");
        session
            .register_resource(Box::new(CountedResource(Arc::clone(&closed))))
            .expect("register resource");
        session
            .queue_input_audio(AudioFrame {
                sequence: cycle as u64,
                bytes: vec![2; 960],
            })
            .expect("queue input");
        session
            .queue_output_audio(AudioFrame {
                sequence: cycle as u64,
                bytes: vec![3; 960],
            })
            .expect("queue output");
        session.close().expect("close");
        assert_eq!(session.input_audio_len(), 0);
        assert_eq!(session.output_audio_len(), 0);
    }

    assert_eq!(closed.load(Ordering::SeqCst), LIFECYCLE_CYCLES);
}

#[test]
fn reconnect_after_provider_failure_preserves_the_session() {
    let mut session = RealtimeVoiceSession::new(available()).expect("voice session");
    session
        .transition(RealtimeVoiceState::Connecting)
        .expect("connecting");
    session
        .transition(RealtimeVoiceState::Failed)
        .expect("provider failure");
    session
        .transition(RealtimeVoiceState::Reconnecting)
        .expect("reconnecting");
    session
        .transition(RealtimeVoiceState::Listening)
        .expect("recovered");

    assert_eq!(session.state(), RealtimeVoiceState::Listening);
}

#[test]
fn unavailable_oauth_capability_stays_explicit_and_api_key_free() {
    let reason = "configured OAuth route does not expose realtime voice";
    let session = RealtimeVoiceSession::new(VoiceAvailability::unavailable(reason))
        .expect("unavailable session");

    assert!(!session.availability().available);
    assert_eq!(session.availability().reason.as_deref(), Some(reason));
    assert!(!reason.to_ascii_lowercase().contains("api key"));
}
