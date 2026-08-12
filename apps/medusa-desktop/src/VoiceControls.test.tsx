import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import {
  VoiceControls,
  type VoiceConnectInput,
  type VoiceTransport,
} from "./VoiceControls";

function media() {
  const stop = vi.fn();
  const stream = {
    getTracks: () => [{ stop }],
    getAudioTracks: () => [{ stop, enabled: true }],
  } as unknown as MediaStream;
  Object.defineProperty(navigator, "mediaDevices", {
    configurable: true,
    value: {
      getUserMedia: vi.fn().mockResolvedValue(stream),
      enumerateDevices: vi.fn().mockResolvedValue([
        { kind: "audioinput", deviceId: "mic", label: "Desk mic" },
        { kind: "audiooutput", deviceId: "speaker", label: "Desk speaker" },
      ]),
      addEventListener: vi.fn(),
      removeEventListener: vi.fn(),
    },
  });
  return { stop, stream };
}

function transport(): VoiceTransport {
  return {
    connect: vi
      .fn<(input: VoiceConnectInput) => Promise<MediaStream>>()
      .mockImplementation((input) => input.acquireMicrophone()),
    reconnect: vi.fn().mockResolvedValue(undefined),
    disconnect: vi.fn().mockResolvedValue(undefined),
    setMuted: vi.fn().mockResolvedValue(undefined),
    setSpeakerEnabled: vi.fn().mockResolvedValue(undefined),
    interruptPlayback: vi.fn().mockResolvedValue(undefined),
  };
}

afterEach(() => cleanup());

describe("VoiceControls", () => {
  it("does not request microphone permission when capability is unavailable", async () => {
    media();
    render(
      <VoiceControls
        capability={{ available: false, reason: "OAuth route has no Realtime endpoint" }}
      />,
    );
    fireEvent.click(screen.getByRole("button", { name: "Start voice mode" }));
    expect(await screen.findByText(/OAuth route/)).toBeInTheDocument();
    expect(navigator.mediaDevices.getUserMedia).not.toHaveBeenCalled();
  });

  it("lets the authenticated transport decide when microphone capture begins", async () => {
    const { stop } = media();
    const api = transport();
    render(<VoiceControls capability={{ available: true }} transport={api} />);
    expect(navigator.mediaDevices.getUserMedia).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "Start voice mode" }));
    await screen.findByText("Listening");
    expect(api.connect).toHaveBeenCalledWith(
      expect.objectContaining({ acquireMicrophone: expect.any(Function) }),
    );
    fireEvent.click(screen.getByRole("button", { name: "Leave voice mode" }));
    await waitFor(() => expect(stop).toHaveBeenCalled());
    expect(api.disconnect).toHaveBeenCalled();
  });

  it("keeps live transcripts distinct and exposes mute and output controls", async () => {
    media();
    const api = transport();
    render(
      <VoiceControls
        capability={{ available: true }}
        transport={api}
        transcripts={[
          { id: "u", role: "user", text: "run tests", final: false },
          { id: "a", role: "assistant", text: "Starting now", final: true },
        ]}
      />,
    );
    fireEvent.click(screen.getByRole("button", { name: "Start voice mode" }));
    await screen.findByText("Listening");
    expect(screen.getByText("run tests")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Mute microphone" }));
    expect(api.setMuted).toHaveBeenCalledWith(true);
    fireEvent.click(screen.getByRole("button", { name: "Mute speaker output" }));
    expect(api.setSpeakerEnabled).toHaveBeenCalledWith(false);
  });

  it("barge-in interrupts playback without disconnecting the task", () => {
    const api = transport();
    render(
      <VoiceControls
        capability={{ available: true }}
        transport={api}
        phase="assistant-speaking"
      />,
    );
    fireEvent.click(screen.getByRole("button", { name: "Interrupt assistant speech" }));
    expect(api.interruptPlayback).toHaveBeenCalled();
    expect(api.disconnect).not.toHaveBeenCalled();
  });
});
