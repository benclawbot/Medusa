import React, { useEffect, useRef, useState } from "react";
import {
  OpenAiRealtimeWebRtcTransport,
  loadDesktopRealtimeCapability,
  type DesktopRealtimeCapability,
} from "./OpenAiRealtimeWebRtcTransport";
import {
  VoiceControls,
  type VoicePhase,
  type VoiceTranscript,
} from "./VoiceControls";

const checkingCapability: DesktopRealtimeCapability = {
  available: false,
  reason: "Checking the authenticated OpenAI Realtime route…",
  supportsInputAudio: false,
  supportsOutputAudio: false,
  supportsBargeIn: false,
};

export function DesktopVoiceDock() {
  const [capability, setCapability] = useState(checkingCapability);
  const [phase, setPhase] = useState<VoicePhase>("inactive");
  const [transcripts, setTranscripts] = useState<VoiceTranscript[]>([]);
  const transportRef = useRef<OpenAiRealtimeWebRtcTransport | undefined>(undefined);

  if (!transportRef.current) {
    transportRef.current = new OpenAiRealtimeWebRtcTransport({
      onPhaseChange: setPhase,
      onTranscript: (transcript) => {
        setTranscripts((current) => {
          const index = current.findIndex((item) => item.id === transcript.id);
          if (index < 0) return [...current, transcript];
          const next = [...current];
          next[index] = transcript;
          return next;
        });
      },
    });
  }

  useEffect(() => {
    let active = true;
    void loadDesktopRealtimeCapability()
      .then((next) => {
        if (active) setCapability(next);
      })
      .catch(() => {
        if (!active) return;
        setCapability({
          available: false,
          reason:
            "The desktop could not inspect the authenticated Realtime route. No microphone access was requested.",
          supportsInputAudio: false,
          supportsOutputAudio: false,
          supportsBargeIn: false,
        });
      });
    return () => {
      active = false;
      void transportRef.current?.disconnect();
    };
  }, []);

  return (
    <div className="desktop-voice-dock">
      <VoiceControls
        capability={capability}
        transport={transportRef.current}
        transcripts={transcripts}
        phase={phase}
        onPhaseChange={setPhase}
      />
    </div>
  );
}
