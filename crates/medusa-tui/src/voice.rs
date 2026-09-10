//! Terminal-facing realtime voice interaction state.
//!
//! Audio capture, playback, and provider transport stay outside the renderer.
//! This controller owns TUI interaction semantics and drives the shared
//! provider-neutral realtime voice session.

use std::{collections::BTreeMap, env, path::Path};

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use medusa_runtime::voice::{
    RealtimeVoiceEvent, RealtimeVoiceSession, RealtimeVoiceState, TranscriptSpeaker,
    TranscriptUpdate, VoiceAvailability, VoiceSessionError,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PushToTalkMode {
    HoldSpaceFocus,
    TapSpaceFocus,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VoiceControl {
    None,
    Enter,
    Leave,
    Mute,
    Unmute,
    FocusCaptureStart,
    FocusCaptureStop,
    StopSpeech,
    CancelResponse,
    CancelTask,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AudioEnvironment {
    Local,
    Ssh,
    Container,
    Wsl,
    Ci,
    Headless,
}

impl AudioEnvironment {
    #[must_use]
    pub fn detect() -> Self {
        Self::detect_from(
            &env::vars_os().collect(),
            cfg!(target_os = "windows"),
            Path::new("/.dockerenv").exists(),
        )
    }

    /// Testable core of [`AudioEnvironment::detect`]: `vars` holds the
    /// observed environment, `is_windows` selects the platform branch, and
    /// `container_file` stands in for `/.dockerenv`.
    ///
    /// Both platform branches are compiled on every target (selected with a
    /// runtime flag, never `cfg`) so Windows headless sessions are detected
    /// instead of falling through to `Local`.
    pub(crate) fn detect_from(
        vars: &std::collections::HashMap<std::ffi::OsString, std::ffi::OsString>,
        is_windows: bool,
        container_file: bool,
    ) -> Self {
        let present = |name: &str| vars.contains_key(std::ffi::OsStr::new(name));
        let non_empty = |name: &str| {
            vars.get(std::ffi::OsStr::new(name))
                .is_some_and(|value| !value.is_empty())
        };
        if present("CI") {
            return Self::Ci;
        }
        if present("SSH_CONNECTION") || present("SSH_TTY") {
            return Self::Ssh;
        }
        if present("WSL_DISTRO_NAME") || present("WSL_INTEROP") {
            return Self::Wsl;
        }
        if present("container") || present("KUBERNETES_SERVICE_HOST") || container_file {
            return Self::Container;
        }
        if is_windows {
            // Windows has no DISPLAY/WAYLAND audio-server variables. An
            // interactive session exposes SESSIONNAME (Console/RDP) or runs
            // inside Windows Terminal (WT_SESSION); services and other
            // headless sessions expose neither.
            if non_empty("SESSIONNAME") || present("WT_SESSION") {
                return Self::Local;
            }
            return Self::Headless;
        }
        if !present("DISPLAY")
            && !present("WAYLAND_DISPLAY")
            && !present("PULSE_SERVER")
            && !present("PIPEWIRE_REMOTE")
        {
            return Self::Headless;
        }
        Self::Local
    }

    #[must_use]
    pub fn availability(self) -> VoiceAvailability {
        let reason = match self {
            Self::Local => {
                return VoiceAvailability {
                    available: true,
                    reason: None,
                    supports_input_audio: true,
                    supports_output_audio: true,
                    supports_barge_in: true,
                };
            }
            Self::Ssh => {
                "voice is unavailable over SSH without explicitly forwarded local audio devices"
            }
            Self::Container => {
                "voice is unavailable in this container because no local audio device is exposed"
            }
            Self::Wsl => {
                "voice is unavailable in this WSL session until a working Windows audio bridge is detected"
            }
            Self::Ci => "voice is disabled in CI; text mode remains fully available",
            Self::Headless => {
                "voice is unavailable in this headless session because no local audio server is detected"
            }
        };
        VoiceAvailability::unavailable(reason)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VoiceTranscriptLine {
    pub turn_id: String,
    pub speaker: TranscriptSpeaker,
    pub text: String,
    pub is_final: bool,
}

#[derive(Debug)]
pub struct TuiVoiceController {
    session: RealtimeVoiceSession,
    enabled: bool,
    muted: bool,
    focus_capture: bool,
    ptt_mode: PushToTalkMode,
    transcripts: BTreeMap<(String, TranscriptSpeaker), VoiceTranscriptLine>,
    last_error: Option<String>,
}

impl TuiVoiceController {
    pub fn for_environment(environment: AudioEnvironment) -> Result<Self, VoiceSessionError> {
        Self::new(environment.availability(), PushToTalkMode::HoldSpaceFocus)
    }

    pub fn new(
        availability: VoiceAvailability,
        ptt_mode: PushToTalkMode,
    ) -> Result<Self, VoiceSessionError> {
        Ok(Self {
            session: RealtimeVoiceSession::new(availability)?,
            enabled: false,
            muted: false,
            focus_capture: false,
            ptt_mode,
            transcripts: BTreeMap::new(),
            last_error: None,
        })
    }

    #[must_use]
    pub fn enabled(&self) -> bool {
        self.enabled
    }

    #[must_use]
    pub fn state(&self) -> RealtimeVoiceState {
        self.session.state()
    }

    #[must_use]
    pub fn status_line(&self) -> String {
        if !self.enabled {
            return "voice ready · speak naturally · hold Space to focus capture".to_owned();
        }
        if !self.session.availability().available {
            let reason = self
                .session
                .availability()
                .reason
                .as_deref()
                .unwrap_or("unsupported audio environment");
            return format!("voice unavailable · {reason}");
        }
        if self.muted {
            return "voice muted · text remains active · /unmute to resume".to_owned();
        }
        if self.focus_capture {
            return match self.ptt_mode {
                PushToTalkMode::HoldSpaceFocus => {
                    "voice focused · release Space to return to full duplex".to_owned()
                }
                PushToTalkMode::TapSpaceFocus => {
                    "voice focused · tap Space to return to full duplex".to_owned()
                }
            };
        }
        let state = match self.session.state() {
            RealtimeVoiceState::Idle => "ready",
            RealtimeVoiceState::Connecting => "connecting",
            RealtimeVoiceState::Listening => "listening",
            RealtimeVoiceState::UserSpeaking => "you are speaking",
            RealtimeVoiceState::Thinking => "thinking",
            RealtimeVoiceState::ToolRunning => "tool running",
            RealtimeVoiceState::AwaitingApproval => "approval required",
            RealtimeVoiceState::AssistantSpeaking => "assistant speaking · speak to interrupt",
            RealtimeVoiceState::Interrupted => "interrupted",
            RealtimeVoiceState::Reconnecting => "reconnecting",
            RealtimeVoiceState::Failed => "failed",
            RealtimeVoiceState::Closed => "closed",
        };
        format!("voice {state} · full duplex · hold Space to focus")
    }

    pub fn enter(&mut self) -> Result<(), VoiceSessionError> {
        if self.enabled {
            return Ok(());
        }
        if !self.session.availability().available {
            self.last_error = self.session.availability().reason.clone();
            return Ok(());
        }
        self.session.transition(RealtimeVoiceState::Connecting)?;
        self.session.transition(RealtimeVoiceState::Listening)?;
        self.enabled = true;
        Ok(())
    }

    pub fn leave(&mut self) -> Result<(), VoiceSessionError> {
        self.enabled = false;
        self.focus_capture = false;
        self.muted = true;
        self.session.close()
    }

    pub fn apply_control(&mut self, control: VoiceControl) -> Result<(), VoiceSessionError> {
        match control {
            VoiceControl::None => {}
            VoiceControl::Enter => self.enter()?,
            VoiceControl::Leave => self.leave()?,
            VoiceControl::Mute => {
                self.muted = true;
                self.focus_capture = false;
            }
            VoiceControl::Unmute => self.muted = false,
            VoiceControl::FocusCaptureStart if self.enabled && !self.muted => {
                self.focus_capture = true;
            }
            VoiceControl::FocusCaptureStart | VoiceControl::FocusCaptureStop => {
                self.focus_capture = false;
            }
            VoiceControl::StopSpeech | VoiceControl::CancelResponse => {
                if self.enabled && self.session.state() == RealtimeVoiceState::AssistantSpeaking {
                    self.session.barge_in()?;
                }
            }
            VoiceControl::CancelTask => {}
        }
        Ok(())
    }

    #[must_use]
    pub fn control_for_key(&self, key: KeyEvent, composer_empty: bool) -> VoiceControl {
        if !self.enabled
            || !composer_empty
            || key.code != KeyCode::Char(' ')
            || key.modifiers != KeyModifiers::NONE
        {
            return VoiceControl::None;
        }
        match self.ptt_mode {
            PushToTalkMode::HoldSpaceFocus => match key.kind {
                KeyEventKind::Press => VoiceControl::FocusCaptureStart,
                KeyEventKind::Release => VoiceControl::FocusCaptureStop,
                KeyEventKind::Repeat => VoiceControl::None,
            },
            PushToTalkMode::TapSpaceFocus if key.kind == KeyEventKind::Press => {
                if self.focus_capture {
                    VoiceControl::FocusCaptureStop
                } else {
                    VoiceControl::FocusCaptureStart
                }
            }
            PushToTalkMode::TapSpaceFocus => VoiceControl::None,
        }
    }

    pub fn update_transcript(&mut self, update: TranscriptUpdate) -> Result<(), VoiceSessionError> {
        let key = (update.turn_id.clone(), update.speaker);
        self.transcripts.insert(
            key,
            VoiceTranscriptLine {
                turn_id: update.turn_id.clone(),
                speaker: update.speaker,
                text: update.text.clone(),
                is_final: update.is_final,
            },
        );
        self.session.update_transcript(update)
    }

    #[must_use]
    pub fn transcript_lines(&self) -> Vec<&VoiceTranscriptLine> {
        self.transcripts.values().collect()
    }

    pub fn drain_events(&mut self) -> Vec<RealtimeVoiceEvent> {
        std::iter::from_fn(|| self.session.next_event()).collect()
    }

    #[must_use]
    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }
}

impl Drop for TuiVoiceController {
    fn drop(&mut self) {
        if self.session.state() != RealtimeVoiceState::Closed {
            let _ = self.session.close();
        }
    }
}

#[must_use]
pub fn parse_voice_command(value: &str) -> Option<VoiceControl> {
    match value.trim() {
        "/voice" | "/voice on" => Some(VoiceControl::Enter),
        "/voice off" | "/leave-voice" => Some(VoiceControl::Leave),
        "/mute" => Some(VoiceControl::Mute),
        "/unmute" => Some(VoiceControl::Unmute),
        "/stop-speech" => Some(VoiceControl::StopSpeech),
        "/cancel-response" => Some(VoiceControl::CancelResponse),
        "/cancel-task" => Some(VoiceControl::CancelTask),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn available() -> VoiceAvailability {
        VoiceAvailability {
            available: true,
            reason: None,
            supports_input_audio: true,
            supports_output_audio: true,
            supports_barge_in: true,
        }
    }

    #[test]
    fn full_duplex_is_default_and_space_is_only_a_focus_override() {
        let mut voice =
            TuiVoiceController::new(available(), PushToTalkMode::HoldSpaceFocus).expect("voice");
        voice.enter().expect("enter");
        assert_eq!(voice.state(), RealtimeVoiceState::Listening);
        assert!(voice.status_line().contains("full duplex"));
        let press =
            KeyEvent::new_with_kind(KeyCode::Char(' '), KeyModifiers::NONE, KeyEventKind::Press);
        assert_eq!(
            voice.control_for_key(press, true),
            VoiceControl::FocusCaptureStart
        );
        assert_eq!(voice.control_for_key(press, false), VoiceControl::None);
    }

    #[test]
    fn release_ends_focus_without_ending_voice_mode() {
        let mut voice =
            TuiVoiceController::new(available(), PushToTalkMode::HoldSpaceFocus).expect("voice");
        voice.enter().expect("enter");
        voice
            .apply_control(VoiceControl::FocusCaptureStart)
            .expect("focus");
        voice
            .apply_control(VoiceControl::FocusCaptureStop)
            .expect("release");
        assert!(voice.enabled());
        assert_eq!(voice.state(), RealtimeVoiceState::Listening);
    }

    #[test]
    fn tap_fallback_toggles_focus_for_release_less_terminals() {
        let mut voice =
            TuiVoiceController::new(available(), PushToTalkMode::TapSpaceFocus).expect("voice");
        voice.enter().expect("enter");
        let press = KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE);
        let first = voice.control_for_key(press, true);
        voice.apply_control(first).expect("start");
        let second = voice.control_for_key(press, true);
        assert_eq!(second, VoiceControl::FocusCaptureStop);
    }

    #[test]
    fn partial_transcript_collapses_into_one_final_turn() {
        let mut voice =
            TuiVoiceController::new(available(), PushToTalkMode::HoldSpaceFocus).expect("voice");
        for (text, is_final) in [("fix the", false), ("fix the tests", true)] {
            voice
                .update_transcript(TranscriptUpdate {
                    turn_id: "turn-1".to_owned(),
                    speaker: TranscriptSpeaker::User,
                    text: text.to_owned(),
                    is_final,
                })
                .expect("transcript");
        }
        let lines = voice.transcript_lines();
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text, "fix the tests");
        assert!(lines[0].is_final);
    }

    #[test]
    fn unavailable_environment_never_activates_microphone() {
        let mut voice = TuiVoiceController::for_environment(AudioEnvironment::Ci).expect("voice");
        voice.enter().expect("enter");
        assert!(!voice.enabled());
        assert_eq!(voice.state(), RealtimeVoiceState::Idle);
        assert!(voice.status_line().contains("ready"));
        assert!(voice.last_error().is_some());
    }

    #[test]
    fn controls_keep_cancellation_levels_distinct() {
        assert_eq!(
            parse_voice_command("/stop-speech"),
            Some(VoiceControl::StopSpeech)
        );
        assert_eq!(
            parse_voice_command("/cancel-response"),
            Some(VoiceControl::CancelResponse)
        );
        assert_eq!(
            parse_voice_command("/cancel-task"),
            Some(VoiceControl::CancelTask)
        );
    }

    #[test]
    fn leave_closes_deterministically() {
        let mut voice =
            TuiVoiceController::new(available(), PushToTalkMode::HoldSpaceFocus).expect("voice");
        voice.enter().expect("enter");
        voice.leave().expect("leave");
        assert_eq!(voice.state(), RealtimeVoiceState::Closed);
    }

    fn vars(
        pairs: &[(&str, &str)],
    ) -> std::collections::HashMap<std::ffi::OsString, std::ffi::OsString> {
        pairs
            .iter()
            .map(|(key, value)| {
                (
                    std::ffi::OsString::from(key),
                    std::ffi::OsString::from(value),
                )
            })
            .collect()
    }

    #[test]
    fn windows_headless_sessions_are_detected_at_runtime() {
        // No session indicators: service / headless session.
        assert_eq!(
            AudioEnvironment::detect_from(&vars(&[]), true, false),
            AudioEnvironment::Headless
        );
        assert_eq!(
            AudioEnvironment::detect_from(&vars(&[("SESSIONNAME", "")]), true, false),
            AudioEnvironment::Headless
        );
        // Interactive Windows sessions stay local.
        assert_eq!(
            AudioEnvironment::detect_from(&vars(&[("SESSIONNAME", "Console")]), true, false),
            AudioEnvironment::Local
        );
        assert_eq!(
            AudioEnvironment::detect_from(&vars(&[("WT_SESSION", "abc")]), true, false),
            AudioEnvironment::Local
        );
    }

    #[test]
    fn unix_headless_detection_still_uses_audio_server_variables() {
        assert_eq!(
            AudioEnvironment::detect_from(&vars(&[]), false, false),
            AudioEnvironment::Headless
        );
        assert_eq!(
            AudioEnvironment::detect_from(&vars(&[("DISPLAY", ":0")]), false, false),
            AudioEnvironment::Local
        );
        assert_eq!(
            AudioEnvironment::detect_from(&vars(&[("WAYLAND_DISPLAY", "wayland-0")]), false, false),
            AudioEnvironment::Local
        );
        assert_eq!(
            AudioEnvironment::detect_from(&vars(&[("CI", "true")]), true, false),
            AudioEnvironment::Ci
        );
    }
}
