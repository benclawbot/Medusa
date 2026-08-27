import type { VoicePhase } from "./VoiceControls";

export const OPENAI_REALTIME_LIVE_EVIDENCE_TIMEOUT_MS = 45_000;
const MAX_EVIDENCE_TIMEOUT_MS = 60_000;
const SHA256_HEX_LENGTH = 64;

export interface TranscriptEvidence {
  observed: boolean;
  sha256?: string;
  characterCount?: number;
}

export interface OpenAiRealtimeLiveEvidenceReport {
  schemaVersion: 1;
  result: "passed" | "failed";
  provider: "openai-oauth";
  startedAt: string;
  finishedAt: string;
  timeoutMs: number;
  optInActivated: true;
  credentialEstablishedBeforeMicrophoneRequest: boolean;
  microphone: {
    observed: boolean;
    trackKind?: string;
    trackReadyState?: MediaStreamTrackState;
  };
  userTranscript: TranscriptEvidence;
  assistantTranscript: TranscriptEvidence;
  assistantAudio: {
    providerPlaybackStarted: boolean;
    remoteTrackObserved: boolean;
    playbackRequested: boolean;
    playbackStarted: boolean;
  };
  phases: VoicePhase[];
  privacy: {
    rawAudioPersisted: false;
    transcriptTextPersisted: false;
    credentialPersisted: false;
  };
  failureReason?: string;
}

export interface OpenAiRealtimeLiveEvidenceObservations {
  startedAtMs: number;
  finishedAtMs: number;
  result: "passed" | "failed";
  credentialEstablishedAtMs?: number;
  microphoneRequestedAtMs?: number;
  microphoneTrackKind?: string;
  microphoneTrackReadyState?: MediaStreamTrackState;
  userTranscript?: Omit<TranscriptEvidence, "observed">;
  assistantTranscript?: Omit<TranscriptEvidence, "observed">;
  providerPlaybackStarted: boolean;
  remoteTrackObserved: boolean;
  playbackRequested: boolean;
  playbackStarted: boolean;
  phases: VoicePhase[];
  failureReason?: string;
}

export function createOpenAiRealtimeLiveEvidenceReport(
  observations: OpenAiRealtimeLiveEvidenceObservations,
): OpenAiRealtimeLiveEvidenceReport {
  return {
    schemaVersion: 1,
    result: observations.result,
    provider: "openai-oauth",
    startedAt: new Date(observations.startedAtMs).toISOString(),
    finishedAt: new Date(observations.finishedAtMs).toISOString(),
    timeoutMs: OPENAI_REALTIME_LIVE_EVIDENCE_TIMEOUT_MS,
    optInActivated: true,
    credentialEstablishedBeforeMicrophoneRequest:
      observations.credentialEstablishedAtMs !== undefined &&
      observations.microphoneRequestedAtMs !== undefined &&
      observations.credentialEstablishedAtMs <= observations.microphoneRequestedAtMs,
    microphone: {
      observed: observations.microphoneRequestedAtMs !== undefined,
      ...(observations.microphoneTrackKind
        ? { trackKind: observations.microphoneTrackKind }
        : {}),
      ...(observations.microphoneTrackReadyState
        ? { trackReadyState: observations.microphoneTrackReadyState }
        : {}),
    },
    userTranscript: transcriptEvidence(observations.userTranscript),
    assistantTranscript: transcriptEvidence(observations.assistantTranscript),
    assistantAudio: {
      providerPlaybackStarted: observations.providerPlaybackStarted,
      remoteTrackObserved: observations.remoteTrackObserved,
      playbackRequested: observations.playbackRequested,
      playbackStarted: observations.playbackStarted,
    },
    phases: [...observations.phases],
    privacy: {
      rawAudioPersisted: false,
      transcriptTextPersisted: false,
      credentialPersisted: false,
    },
    ...(observations.failureReason
      ? { failureReason: observations.failureReason }
      : {}),
  };
}

export function validateOpenAiRealtimeLiveEvidenceReport(
  report: OpenAiRealtimeLiveEvidenceReport,
): string[] {
  const failures: string[] = [];
  if (report.schemaVersion !== 1) failures.push("unsupported evidence schema");
  if (report.provider !== "openai-oauth") failures.push("unexpected provider");
  if (!report.optInActivated) failures.push("explicit opt-in was not recorded");
  if (report.timeoutMs <= 0 || report.timeoutMs > MAX_EVIDENCE_TIMEOUT_MS) {
    failures.push("evidence run was not bounded to sixty seconds or less");
  }
  if (!report.credentialEstablishedBeforeMicrophoneRequest) {
    failures.push("credential establishment did not precede microphone access");
  }
  if (!report.microphone.observed || report.microphone.trackKind !== "audio") {
    failures.push("an audio microphone track was not observed");
  }
  if (!validTranscriptEvidence(report.userTranscript)) {
    failures.push("a sanitized final user transcript was not observed");
  }
  if (!report.assistantAudio.providerPlaybackStarted) {
    failures.push("OpenAI did not report assistant audio playback start");
  }
  if (!report.assistantAudio.remoteTrackObserved) {
    failures.push("a remote assistant audio track was not observed");
  }
  if (!report.assistantAudio.playbackRequested || !report.assistantAudio.playbackStarted) {
    failures.push("assistant audio playback was not started");
  }
  if (
    report.privacy.rawAudioPersisted ||
    report.privacy.transcriptTextPersisted ||
    report.privacy.credentialPersisted
  ) {
    failures.push("evidence report records forbidden sensitive persistence");
  }
  const startedAt = Date.parse(report.startedAt);
  const finishedAt = Date.parse(report.finishedAt);
  if (
    !Number.isFinite(startedAt) ||
    !Number.isFinite(finishedAt) ||
    finishedAt < startedAt ||
    finishedAt - startedAt > report.timeoutMs + 5_000
  ) {
    failures.push("evidence timestamps exceed the bounded run window");
  }
  if (report.result !== "passed") failures.push("live evidence result is not passed");
  return failures;
}

export async function sha256Hex(text: string): Promise<string> {
  const bytes = new TextEncoder().encode(text);
  const digest = await crypto.subtle.digest("SHA-256", bytes);
  return Array.from(new Uint8Array(digest), (byte) =>
    byte.toString(16).padStart(2, "0"),
  ).join("");
}

function transcriptEvidence(
  evidence?: Omit<TranscriptEvidence, "observed">,
): TranscriptEvidence {
  return evidence
    ? { observed: true, ...evidence }
    : { observed: false };
}

function validTranscriptEvidence(evidence: TranscriptEvidence): boolean {
  return (
    evidence.observed &&
    typeof evidence.sha256 === "string" &&
    evidence.sha256.length === SHA256_HEX_LENGTH &&
    /^[a-f0-9]+$/.test(evidence.sha256) &&
    typeof evidence.characterCount === "number" &&
    evidence.characterCount > 0
  );
}
