import { describe, expect, it, vi } from "vitest";
import {
  OpenAiRealtimeWebRtcTransport,
  type DesktopRealtimeCapability,
} from "./OpenAiRealtimeWebRtcTransport";
import type { VoicePhase, VoiceTranscript } from "./VoiceControls";

class FakeDataChannel {
  readonly label: string;
  readyState: RTCDataChannelState = "open";
  onmessage: ((event: MessageEvent) => void) | null = null;
  onerror: (() => void) | null = null;
  sent: string[] = [];
  close = vi.fn();

  constructor(label: string) {
    this.label = label;
  }

  send(value: string) {
    this.sent.push(value);
  }
}

class FakePeerConnection {
  connectionState: RTCPeerConnectionState = "new";
  ontrack: ((event: RTCTrackEvent) => void) | null = null;
  onconnectionstatechange: (() => void) | null = null;
  readonly channel = new FakeDataChannel("oai-events");
  readonly addTrack = vi.fn();
  readonly close = vi.fn();
  readonly setLocalDescription = vi.fn().mockResolvedValue(undefined);
  readonly setRemoteDescription = vi.fn().mockResolvedValue(undefined);
  readonly createOffer = vi.fn().mockResolvedValue({
    type: "offer",
    sdp: "v=0\r\na=offer:test\r\n",
  });

  createDataChannel(label: string) {
    this.channel.readyState = "open";
    expect(label).toBe("oai-events");
    return this.channel as unknown as RTCDataChannel;
  }
}

function media() {
  const track = { enabled: true, stop: vi.fn() };
  const stream = {
    getTracks: () => [track],
    getAudioTracks: () => [track],
  } as unknown as MediaStream;
  return { stream, track };
}

function audio() {
  return {
    autoplay: false,
    muted: false,
    srcObject: null,
    play: vi.fn().mockResolvedValue(undefined),
    pause: vi.fn(),
    setSinkId: vi.fn().mockResolvedValue(undefined),
  } as unknown as HTMLAudioElement;
}

function session() {
  return {
    authorizationToken: "short-lived-secret",
    expiresAt: 200,
    model: "gpt-realtime",
    webrtcCallUrl: "https://api.openai.com/v1/realtime/calls",
  };
}

function capability(): DesktopRealtimeCapability {
  return {
    available: true,
    supportsInputAudio: true,
    supportsOutputAudio: true,
    supportsBargeIn: true,
  };
}

void capability;

describe("OpenAiRealtimeWebRtcTransport", () => {
  it("mints the bounded credential before requesting microphone access", async () => {
    const order: string[] = [];
    const peer = new FakePeerConnection();
    const { stream } = media();
    const request = vi.fn().mockResolvedValue(
      new Response("v=0\r\na=answer:test\r\n", {
        status: 201,
        headers: { Location: "/v1/realtime/calls/rtc_test" },
      }),
    );
    const transport = new OpenAiRealtimeWebRtcTransport({}, {
      invoke: vi.fn().mockImplementation(async () => {
        order.push("credential");
        return session();
      }),
      fetch: request,
      createPeerConnection: () => peer as unknown as RTCPeerConnection,
      createAudioElement: audio,
      nowSeconds: () => 100,
    });

    await transport.connect({
      speakerId: "speaker",
      acquireMicrophone: async () => {
        order.push("microphone");
        return stream;
      },
    });

    expect(order).toEqual(["credential", "microphone"]);
    expect(peer.addTrack).toHaveBeenCalled();
    expect(peer.setRemoteDescription).toHaveBeenCalledWith({
      type: "answer",
      sdp: "v=0\r\na=answer:test\r\n",
    });
    const [url, init] = request.mock.calls[0] as [string, RequestInit];
    expect(url).toBe("https://api.openai.com/v1/realtime/calls");
    expect(init.method).toBe("POST");
    expect((init.headers as Record<string, string>).Authorization).toBe(
      "Bearer short-lived-secret",
    );
    const body = init.body as FormData;
    const offer = body.get("sdp") as Blob;
    expect(offer.type).toBe("application/sdp");
    expect(offer.size).toBeGreaterThan(0);
  });

  it("translates data-channel events and sends the WebRTC barge-in contract", async () => {
    const peer = new FakePeerConnection();
    const { stream } = media();
    const phases: VoicePhase[] = [];
    const transcripts: VoiceTranscript[] = [];
    const transport = new OpenAiRealtimeWebRtcTransport(
      {
        onPhaseChange: (phase) => phases.push(phase),
        onTranscript: (transcript) => transcripts.push(transcript),
      },
      {
        invoke: vi.fn().mockResolvedValue(session()),
        fetch: vi
          .fn()
          .mockResolvedValue(new Response("v=0\r\na=answer:test\r\n", { status: 201 })),
        createPeerConnection: () => peer as unknown as RTCPeerConnection,
        createAudioElement: audio,
        nowSeconds: () => 100,
      },
    );
    await transport.connect({ acquireMicrophone: async () => stream });

    peer.channel.onmessage?.({
      data: JSON.stringify({
        type: "conversation.item.input_audio_transcription.delta",
        item_id: "user-1",
        delta: "run ",
      }),
    } as MessageEvent);
    peer.channel.onmessage?.({
      data: JSON.stringify({
        type: "conversation.item.input_audio_transcription.completed",
        item_id: "user-1",
        transcript: "run tests",
      }),
    } as MessageEvent);
    peer.channel.onmessage?.({
      data: JSON.stringify({ type: "response.created", response: { id: "response-1" } }),
    } as MessageEvent);
    peer.channel.onmessage?.({
      data: JSON.stringify({
        type: "output_audio_buffer.started",
        response_id: "response-1",
      }),
    } as MessageEvent);

    expect(transcripts[transcripts.length - 1]).toEqual({
      id: "user:user-1",
      role: "user",
      text: "run tests",
      final: true,
    });
    expect(phases).toContain("thinking");
    expect(phases).toContain("assistant-speaking");

    await transport.interruptPlayback();
    expect(peer.channel.sent.map((value) => JSON.parse(value))).toEqual([
      { type: "response.cancel", response_id: "response-1" },
      { type: "output_audio_buffer.clear" },
    ]);
    expect(phases[phases.length - 1]).toBe("interrupted");
  });

  it("mutes capture and releases every media and peer resource", async () => {
    const peer = new FakePeerConnection();
    const output = audio();
    const { stream, track } = media();
    const transport = new OpenAiRealtimeWebRtcTransport({}, {
      invoke: vi.fn().mockResolvedValue(session()),
      fetch: vi
        .fn()
        .mockResolvedValue(new Response("v=0\r\na=answer:test\r\n", { status: 201 })),
      createPeerConnection: () => peer as unknown as RTCPeerConnection,
      createAudioElement: () => output,
      nowSeconds: () => 100,
    });
    await transport.connect({ acquireMicrophone: async () => stream });

    await transport.setMuted(true);
    await transport.setSpeakerEnabled(false);
    expect(track.enabled).toBe(false);
    expect(output.muted).toBe(true);

    await transport.disconnect();
    expect(track.stop).toHaveBeenCalled();
    expect(peer.channel.close).toHaveBeenCalled();
    expect(peer.close).toHaveBeenCalled();
    expect(output.pause).toHaveBeenCalled();
    expect(output.srcObject).toBeNull();
  });

  it("rejects unsafe or expired frontend session material before microphone access", async () => {
    const acquireMicrophone = vi.fn();
    const transport = new OpenAiRealtimeWebRtcTransport({}, {
      invoke: vi.fn().mockResolvedValue({
        ...session(),
        expiresAt: 105,
        webrtcCallUrl: "http://api.openai.com/v1/realtime/calls",
      }),
      fetch: vi.fn(),
      createPeerConnection: () => new FakePeerConnection() as unknown as RTCPeerConnection,
      createAudioElement: audio,
      nowSeconds: () => 100,
    });

    await expect(transport.connect({ acquireMicrophone })).rejects.toThrow(
      /expired or invalid/,
    );
    expect(acquireMicrophone).not.toHaveBeenCalled();
  });
});
