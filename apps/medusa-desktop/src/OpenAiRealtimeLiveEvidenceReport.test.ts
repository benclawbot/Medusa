import { describe, expect, it } from "vitest";
import {
  createOpenAiRealtimeLiveEvidenceReport,
  validateOpenAiRealtimeLiveEvidenceReport,
} from "./OpenAiRealtimeLiveEvidenceReport";

const hash = "a".repeat(64);

function passingReport() {
  return createOpenAiRealtimeLiveEvidenceReport({
    startedAtMs: 1_000,
    finishedAtMs: 20_000,
    result: "passed",
    credentialEstablishedAtMs: 2_000,
    microphoneRequestedAtMs: 2_001,
    microphoneTrackKind: "audio",
    microphoneTrackReadyState: "live",
    userTranscript: { sha256: hash, characterCount: 26 },
    assistantTranscript: { sha256: "b".repeat(64), characterCount: 12 },
    providerPlaybackStarted: true,
    remoteTrackObserved: true,
    playbackRequested: true,
    playbackStarted: true,
    phases: [
      "listening",
      "user-speaking",
      "thinking",
      "assistant-speaking",
    ],
  });
}

describe("OpenAI Realtime live evidence report", () => {
  it("accepts a bounded sanitized microphone-to-transcript-to-audio proof", () => {
    const report = passingReport();

    expect(validateOpenAiRealtimeLiveEvidenceReport(report)).toEqual([]);
    expect(JSON.stringify(report)).not.toContain("Medusa live voice evidence");
    expect(report.privacy).toEqual({
      rawAudioPersisted: false,
      transcriptTextPersisted: false,
      credentialPersisted: false,
    });
  });

  it("rejects microphone access that precedes credential establishment", () => {
    const report = passingReport();
    report.credentialEstablishedBeforeMicrophoneRequest = false;

    expect(validateOpenAiRealtimeLiveEvidenceReport(report)).toContain(
      "credential establishment did not precede microphone access",
    );
  });

  it("rejects synthetic success without transcript or assistant audio proof", () => {
    const report = passingReport();
    report.userTranscript = { observed: false };
    report.assistantAudio.remoteTrackObserved = false;
    report.assistantAudio.playbackStarted = false;

    expect(validateOpenAiRealtimeLiveEvidenceReport(report)).toEqual(
      expect.arrayContaining([
        "a sanitized final user transcript was not observed",
        "a remote assistant audio track was not observed",
        "assistant audio playback was not started",
      ]),
    );
  });

  it("rejects reports that exceed the bounded window", () => {
    const report = passingReport();
    report.finishedAt = new Date(70_000).toISOString();

    expect(validateOpenAiRealtimeLiveEvidenceReport(report)).toContain(
      "evidence timestamps exceed the bounded run window",
    );
  });
});
