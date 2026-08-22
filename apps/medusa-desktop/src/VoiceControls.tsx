import { Headphones, Mic, MicOff, PhoneOff, RefreshCw, Volume2, VolumeX } from "lucide-react";
import React, { useEffect, useMemo, useRef, useState } from "react";
import "./voice-controls.css";

export type VoicePhase =
  | "inactive"
  | "connecting"
  | "listening"
  | "user-speaking"
  | "thinking"
  | "assistant-speaking"
  | "interrupted"
  | "reconnecting"
  | "approval-required"
  | "unavailable"
  | "error";

export interface VoiceTranscript {
  id: string;
  role: "user" | "assistant";
  text: string;
  final: boolean;
}

export interface VoiceConnectInput {
  microphoneId?: string;
  speakerId?: string;
  acquireMicrophone: () => Promise<MediaStream>;
}

export interface VoiceTransport {
  connect(input: VoiceConnectInput): Promise<MediaStream>;
  reconnect(): Promise<void>;
  disconnect(): Promise<void>;
  setMuted(muted: boolean): Promise<void>;
  setSpeakerEnabled(enabled: boolean): Promise<void>;
  interruptPlayback(): Promise<void>;
}

export interface VoiceControlsProps {
  capability: { available: boolean; reason?: string };
  transport?: VoiceTransport;
  transcripts?: VoiceTranscript[];
  phase?: VoicePhase;
  onPhaseChange?: (phase: VoicePhase) => void;
}

const labels: Record<VoicePhase, string> = {
  inactive: "Voice off",
  connecting: "Connecting",
  listening: "Listening",
  "user-speaking": "You are speaking",
  thinking: "Medusa is working",
  "assistant-speaking": "Medusa is speaking",
  interrupted: "Playback interrupted",
  reconnecting: "Reconnecting",
  "approval-required": "Approval required",
  unavailable: "Voice unavailable",
  error: "Voice failed",
};

export function VoiceControls({
  capability,
  transport,
  transcripts = [],
  phase: controlledPhase,
  onPhaseChange,
}: VoiceControlsProps) {
  const [internalPhase, setInternalPhase] = useState<VoicePhase>("inactive");
  const [muted, setMuted] = useState(false);
  const [speakerEnabled, setSpeakerEnabled] = useState(true);
  const [devices, setDevices] = useState<MediaDeviceInfo[]>([]);
  const [microphoneId, setMicrophoneId] = useState("");
  const [speakerId, setSpeakerId] = useState("");
  const streamRef = useRef<MediaStream | undefined>(undefined);
  const phase = controlledPhase ?? internalPhase;
  const setPhase = (next: VoicePhase) => {
    setInternalPhase(next);
    onPhaseChange?.(next);
  };
  const active = !["inactive", "unavailable", "error"].includes(phase);
  const microphones = useMemo(
    () => devices.filter((device) => device.kind === "audioinput"),
    [devices],
  );
  const speakers = useMemo(
    () => devices.filter((device) => device.kind === "audiooutput"),
    [devices],
  );

  const refreshDevices = async () => {
    if (!navigator.mediaDevices?.enumerateDevices) return;
    setDevices(await navigator.mediaDevices.enumerateDevices());
  };

  const leave = async () => {
    streamRef.current?.getTracks().forEach((track) => track.stop());
    streamRef.current = undefined;
    await transport?.disconnect();
    setMuted(false);
    setSpeakerEnabled(true);
    setPhase("inactive");
  };

  const enter = async () => {
    if (!capability.available) {
      setPhase("unavailable");
      return;
    }
    if (!transport || !navigator.mediaDevices?.getUserMedia) {
      setPhase("error");
      return;
    }
    setPhase("connecting");
    try {
      const stream = await transport.connect({
        microphoneId: microphoneId || undefined,
        speakerId: speakerId || undefined,
        acquireMicrophone: () =>
          navigator.mediaDevices.getUserMedia({
            audio: microphoneId ? { deviceId: { exact: microphoneId } } : true,
          }),
      });
      streamRef.current = stream;
      await refreshDevices();
      setPhase("listening");
    } catch {
      streamRef.current?.getTracks().forEach((track) => track.stop());
      streamRef.current = undefined;
      await transport.disconnect().catch(() => undefined);
      setPhase("error");
    }
  };

  useEffect(() => {
    const mediaDevices = navigator.mediaDevices;
    if (!mediaDevices?.addEventListener) return;
    const changed = () => {
      if (!active || !transport) return;
      setPhase("reconnecting");
      void refreshDevices()
        .then(() => transport.reconnect())
        .then(() => setPhase("listening"))
        .catch(() => setPhase("error"));
    };
    mediaDevices.addEventListener("devicechange", changed);
    return () => mediaDevices.removeEventListener("devicechange", changed);
  }, [active, transport]);

  useEffect(
    () => () => {
      streamRef.current?.getTracks().forEach((track) => track.stop());
      void transport?.disconnect();
    },
    [transport],
  );

  if (phase === "inactive") {
    return (
      <button
        className="voice-entry"
        onClick={() => void enter()}
        aria-label="Start voice mode"
        title="Start voice mode"
      >
        <Mic size={16} /> Voice
      </button>
    );
  }

  return (
    <section
      className={`voice-controls phase-${phase}`}
      aria-label="Voice mode"
      aria-live="polite"
    >
      <div className="voice-status">
        <span className={`voice-live-dot${active && !muted ? " transmitting" : ""}`} />
        <strong>{labels[phase]}</strong>
        {active && !muted && <small>Microphone transmitting</small>}
      </div>
      {phase === "unavailable" && (
        <p>{capability.reason ?? "The configured provider route does not support realtime voice."}</p>
      )}
      {phase === "error" && (
        <p>
          The authenticated Realtime connection, microphone permission, or audio device is
          unavailable. Voice transmission stopped.
        </p>
      )}
      {active && (
        <>
          <div className="voice-actions">
            <button
              onClick={() => {
                const next = !muted;
                setMuted(next);
                void transport?.setMuted(next);
              }}
              aria-pressed={muted}
              aria-label={muted ? "Unmute microphone" : "Mute microphone"}
            >
              {muted ? <MicOff size={16} /> : <Mic size={16} />}
            </button>
            <button
              onClick={() => {
                const next = !speakerEnabled;
                setSpeakerEnabled(next);
                void transport?.setSpeakerEnabled(next);
              }}
              aria-pressed={!speakerEnabled}
              aria-label={speakerEnabled ? "Mute speaker output" : "Enable speaker output"}
            >
              {speakerEnabled ? <Volume2 size={16} /> : <VolumeX size={16} />}
            </button>
            {phase === "assistant-speaking" && (
              <button
                onClick={() => {
                  setPhase("interrupted");
                  void transport?.interruptPlayback();
                }}
                aria-label="Interrupt assistant speech"
              >
                <Headphones size={16} /> Interrupt
              </button>
            )}
            <button onClick={() => void leave()} aria-label="Leave voice mode">
              <PhoneOff size={16} /> Leave
            </button>
          </div>
          <div className="voice-devices">
            <label>
              Microphone
              <select
                value={microphoneId}
                onChange={(event) => setMicrophoneId(event.target.value)}
              >
                {microphones.length ? (
                  microphones.map((device, index) => (
                    <option key={device.deviceId} value={device.deviceId}>
                      {device.label || `Microphone ${index + 1}`}
                    </option>
                  ))
                ) : (
                  <option value="">System default</option>
                )}
              </select>
            </label>
            <label>
              Speaker
              <select value={speakerId} onChange={(event) => setSpeakerId(event.target.value)}>
                {speakers.length ? (
                  speakers.map((device, index) => (
                    <option key={device.deviceId} value={device.deviceId}>
                      {device.label || `Speaker ${index + 1}`}
                    </option>
                  ))
                ) : (
                  <option value="">System default</option>
                )}
              </select>
            </label>
            <button onClick={() => void refreshDevices()} aria-label="Refresh audio devices">
              <RefreshCw size={14} />
            </button>
          </div>
          {!!transcripts.length && (
            <div className="voice-transcripts">
              {(transcripts ?? []).slice(-4).map((item) => (
                <div
                  key={item.id}
                  className={`${item.role}${item.final ? " final" : " partial"}`}
                >
                  <strong>{item.role === "user" ? "You" : "Medusa"}</strong>
                  <span>{item.text}</span>
                  {!item.final && <small>live</small>}
                </div>
              ))}
            </div>
          )}
        </>
      )}
      {!active && <button onClick={() => setPhase("inactive")}>Back to composer</button>}
    </section>
  );
}
