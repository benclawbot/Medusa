import React, { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import {
  OpenAiRealtimeWebRtcTransport,
  loadDesktopRealtimeCapability,
} from "./OpenAiRealtimeWebRtcTransport";
import type { VoicePhase, VoiceTranscript } from "./VoiceControls";
import {
  OPENAI_REALTIME_LIVE_EVIDENCE_TIMEOUT_MS,
  createOpenAiRealtimeLiveEvidenceReport,
  sha256Hex,
  validateOpenAiRealtimeLiveEvidenceReport,
  type OpenAiRealtimeLiveEvidenceObservations,
  type OpenAiRealtimeLiveEvidenceReport,
} from "./OpenAiRealtimeLiveEvidenceReport";

const evidencePhrase = "Medusa live voice evidence. Please answer with a short confirmation.";

type RunState = "idle" | "running" | "passed" | "failed";

export function OpenAiRealtimeLiveEvidence() {
  const [runState, setRunState] = useState<RunState>("idle");
  const [status, setStatus] = useState(
    "No microphone access has been requested. Start only when you are ready to speak.",
  );
  const [report, setReport] = useState<OpenAiRealtimeLiveEvidenceReport>();
  const transportRef = useRef<OpenAiRealtimeWebRtcTransport | undefined>(undefined);

  useEffect(
    () => () => {
      void transportRef.current?.disconnect();
    },
    [],
  );

  async function start(): Promise<void> {
    if (runState === "running") return;
    setRunState("running");
    setReport(undefined);
    setStatus("Checking the authenticated OpenAI Realtime capability…");

    const startedAtMs = Date.now();
    const observations: OpenAiRealtimeLiveEvidenceObservations = {
      startedAtMs,
      finishedAtMs: startedAtMs,
      result: "failed",
      providerPlaybackStarted: false,
      remoteTrackObserved: false,
      playbackRequested: false,
      playbackStarted: false,
      phases: [],
    };
    let finished = false;
    let timeout: number | undefined;
    let transport: OpenAiRealtimeWebRtcTransport | undefined;

    const finish = async (
      requestedResult: "passed" | "failed",
      failureReason?: string,
    ): Promise<void> => {
      if (finished) return;
      finished = true;
      if (timeout !== undefined) window.clearTimeout(timeout);
      observations.finishedAtMs = Date.now();
      observations.result = requestedResult;
      if (failureReason) {
        observations.failureReason = failureReason;
      } else {
        delete observations.failureReason;
      }
      await transport?.disconnect().catch(() => undefined);
      transportRef.current = undefined;

      let next = createOpenAiRealtimeLiveEvidenceReport(observations);
      if (requestedResult === "passed") {
        const validationFailures = validateOpenAiRealtimeLiveEvidenceReport(next);
        if (validationFailures.length > 0) {
          observations.result = "failed";
          observations.failureReason = validationFailures.join("; ");
          next = createOpenAiRealtimeLiveEvidenceReport(observations);
        }
      }
      setReport(next);
      setRunState(next.result);
      setStatus(
        next.result === "passed"
          ? "Live microphone → transcript → assistant-audio evidence passed. Copy the sanitized JSON below."
          : next.failureReason ?? "Live evidence failed.",
      );
    };

    const maybeFinish = (): void => {
      if (
        observations.credentialEstablishedAtMs !== undefined &&
        observations.microphoneRequestedAtMs !== undefined &&
        observations.credentialEstablishedAtMs <= observations.microphoneRequestedAtMs &&
        observations.microphoneTrackKind === "audio" &&
        observations.userTranscript !== undefined &&
        observations.providerPlaybackStarted &&
        observations.remoteTrackObserved &&
        observations.playbackRequested &&
        observations.playbackStarted
      ) {
        void finish("passed");
      }
    };

    const rememberPhase = (phase: VoicePhase): void => {
      if (finished) return;
      const lastPhase = observations.phases[observations.phases.length - 1];
      if (lastPhase !== phase && observations.phases.length < 32) {
        observations.phases.push(phase);
      }
      if (phase === "assistant-speaking") {
        observations.providerPlaybackStarted = true;
      }
      setStatus(phaseStatus(phase));
      maybeFinish();
    };

    const rememberTranscript = (transcript: VoiceTranscript): void => {
      if (finished || !transcript.final || transcript.text.trim().length === 0) return;
      const text = transcript.text;
      void sha256Hex(text).then((sha256) => {
        if (finished) return;
        const evidence = { sha256, characterCount: text.length };
        if (transcript.role === "user") {
          observations.userTranscript = evidence;
        } else {
          observations.assistantTranscript = evidence;
        }
        maybeFinish();
      });
    };

    const invokeWithEvidence = async <T,>(command: string): Promise<T> => {
      const result = await invoke<T>(command);
      if (command === "desktop_establish_realtime_session") {
        observations.credentialEstablishedAtMs = Date.now();
      }
      return result;
    };

    const createObservedAudioElement = (): HTMLAudioElement => {
      const audio = new Audio();
      const play = audio.play.bind(audio);
      audio.play = async () => {
        if (!finished) {
          observations.playbackRequested = true;
          const remoteStream = audio.srcObject;
          observations.remoteTrackObserved =
            remoteStream instanceof MediaStream &&
            remoteStream.getAudioTracks().length > 0;
        }
        await play();
        if (!finished) {
          observations.playbackStarted = true;
          maybeFinish();
        }
      };
      return audio;
    };

    timeout = window.setTimeout(() => {
      void finish(
        "failed",
        "The bounded live evidence window expired before a final user transcript and assistant audio playback were both observed.",
      );
    }, OPENAI_REALTIME_LIVE_EVIDENCE_TIMEOUT_MS);

    try {
      const capability = await loadDesktopRealtimeCapability(invokeWithEvidence);
      if (
        !capability.available ||
        !capability.supportsInputAudio ||
        !capability.supportsOutputAudio
      ) {
        await finish(
          "failed",
          capability.reason ??
            "The active authenticated account does not expose full-duplex OpenAI Realtime audio.",
        );
        return;
      }

      transport = new OpenAiRealtimeWebRtcTransport(
        {
          onPhaseChange: rememberPhase,
          onTranscript: rememberTranscript,
        },
        {
          invoke: invokeWithEvidence,
          createAudioElement: createObservedAudioElement,
        },
      );
      transportRef.current = transport;

      setStatus(`Speak this phrase, then pause: “${evidencePhrase}”`);
      await transport.connect({
        acquireMicrophone: async () => {
          observations.microphoneRequestedAtMs = Date.now();
          const stream = await navigator.mediaDevices.getUserMedia({
            audio: {
              echoCancellation: true,
              noiseSuppression: true,
              autoGainControl: true,
              channelCount: 1,
            },
          });
          const track = stream.getAudioTracks()[0];
          if (!track) {
            stream.getTracks().forEach((item) => item.stop());
            throw new Error("Realtime microphone access returned no audio track");
          }
          observations.microphoneTrackKind = track.kind;
          observations.microphoneTrackReadyState = track.readyState;
          return stream;
        },
      });
    } catch (error) {
      await finish("failed", safeEvidenceError(error));
    }
  }

  async function copyReport(): Promise<void> {
    if (!report) return;
    if (!navigator.clipboard) {
      setStatus("Clipboard access is unavailable; select and copy the JSON manually.");
      return;
    }
    await navigator.clipboard.writeText(JSON.stringify(report, null, 2));
    setStatus("Sanitized evidence JSON copied.");
  }

  return (
    <main style={styles.page}>
      <section style={styles.card} aria-labelledby="live-evidence-title">
        <p style={styles.eyebrow}>Developer-only acceptance evidence</p>
        <h1 id="live-evidence-title">OpenAI Realtime live voice proof</h1>
        <p>
          This mode uses Medusa&apos;s existing ChatGPT/Codex account state. It never
          asks for an API key. Microphone permission is requested only after a
          bounded short-lived Realtime credential has been established.
        </p>
        <p>
          The report stores no credential, raw audio, or transcript text. Final
          transcripts are represented only by SHA-256 and character count.
        </p>
        <div style={styles.callout} role="status">
          {status}
        </div>
        <button
          type="button"
          onClick={() => void start()}
          disabled={runState === "running"}
          style={styles.button}
        >
          {runState === "running" ? "Live evidence running…" : "Start 45-second live evidence"}
        </button>
        {report ? (
          <section aria-labelledby="evidence-json-title" style={styles.report}>
            <h2 id="evidence-json-title">
              Sanitized evidence: {report.result.toUpperCase()}
            </h2>
            <pre style={styles.pre}>{JSON.stringify(report, null, 2)}</pre>
            <button type="button" onClick={() => void copyReport()} style={styles.button}>
              Copy evidence JSON
            </button>
          </section>
        ) : null}
      </section>
    </main>
  );
}

function phaseStatus(phase: VoicePhase): string {
  switch (phase) {
    case "listening":
      return `Connected. Speak this phrase, then pause: “${evidencePhrase}”`;
    case "user-speaking":
      return "Microphone speech detected…";
    case "thinking":
      return "Finalizing the user transcript and waiting for the assistant…";
    case "assistant-speaking":
      return "Assistant audio started; verifying the remote audio track and playback…";
    case "reconnecting":
      return "Realtime connection is renewing…";
    case "interrupted":
      return "Assistant playback was interrupted.";
    case "error":
      return "The Realtime transport reported an error.";
    case "inactive":
      return "Realtime transport is inactive.";
    default:
      return "Realtime evidence is running…";
  }
}

function safeEvidenceError(error: unknown): string {
  const message = error instanceof Error ? error.message : "";
  if (
    message.startsWith("Realtime ") ||
    message.startsWith("The active authenticated account")
  ) {
    return message;
  }
  if (error instanceof DOMException && error.name === "NotAllowedError") {
    return "Microphone permission was not granted. No audio was transmitted.";
  }
  return "The bounded OpenAI Realtime live evidence run failed.";
}

const styles: Record<string, React.CSSProperties> = {
  page: {
    minHeight: "100vh",
    display: "grid",
    placeItems: "center",
    padding: "32px",
    background: "#0d1117",
    color: "#f0f6fc",
    fontFamily: "system-ui, sans-serif",
  },
  card: {
    width: "min(880px, 100%)",
    padding: "32px",
    border: "1px solid #30363d",
    borderRadius: "16px",
    background: "#161b22",
    boxShadow: "0 24px 80px rgba(0, 0, 0, 0.35)",
  },
  eyebrow: {
    textTransform: "uppercase",
    letterSpacing: "0.08em",
    fontSize: "12px",
    color: "#8b949e",
  },
  callout: {
    margin: "24px 0",
    padding: "16px",
    borderRadius: "10px",
    background: "#0d1117",
    border: "1px solid #30363d",
  },
  button: {
    minHeight: "44px",
    padding: "10px 16px",
    border: "1px solid #2f81f7",
    borderRadius: "8px",
    background: "#1f6feb",
    color: "white",
    fontWeight: 700,
    cursor: "pointer",
  },
  report: {
    marginTop: "28px",
  },
  pre: {
    maxHeight: "420px",
    overflow: "auto",
    padding: "16px",
    borderRadius: "10px",
    background: "#010409",
    border: "1px solid #30363d",
    whiteSpace: "pre-wrap",
    overflowWrap: "anywhere",
  },
};
