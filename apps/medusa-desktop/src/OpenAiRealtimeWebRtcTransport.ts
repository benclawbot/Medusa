import { invoke } from "@tauri-apps/api/core";
import type {
  VoiceConnectInput,
  VoicePhase,
  VoiceTranscript,
  VoiceTransport,
} from "./VoiceControls";

export interface DesktopRealtimeCapability {
  available: boolean;
  reason?: string;
  supportsInputAudio: boolean;
  supportsOutputAudio: boolean;
  supportsBargeIn: boolean;
}

interface DesktopRealtimeSession {
  authorizationToken: string;
  expiresAt: number;
  model: string;
  webrtcCallUrl: string;
}

interface TransportCallbacks {
  onPhaseChange?: (phase: VoicePhase) => void;
  onTranscript?: (transcript: VoiceTranscript) => void;
}

interface TransportEnvironment {
  invoke: <T>(command: string) => Promise<T>;
  fetch: typeof fetch;
  createPeerConnection: () => RTCPeerConnection;
  createAudioElement: () => HTMLAudioElement;
  nowSeconds: () => number;
}

const defaultEnvironment: TransportEnvironment = {
  invoke,
  fetch: globalThis.fetch.bind(globalThis),
  createPeerConnection: () => new RTCPeerConnection(),
  createAudioElement: () => new Audio(),
  nowSeconds: () => Math.floor(Date.now() / 1000),
};

const MINIMUM_CREDENTIAL_LIFETIME_SECONDS = 10;

type RealtimeEvent = Record<string, unknown> & { type?: string };

export async function loadDesktopRealtimeCapability(
  invokeCommand: TransportEnvironment["invoke"] = invoke,
): Promise<DesktopRealtimeCapability> {
  return invokeCommand<DesktopRealtimeCapability>("desktop_realtime_capability");
}

export class OpenAiRealtimeWebRtcTransport implements VoiceTransport {
  private readonly callbacks: TransportCallbacks;
  private readonly environment: TransportEnvironment;
  private peer?: RTCPeerConnection;
  private channel?: RTCDataChannel;
  private stream?: MediaStream;
  private output?: HTMLAudioElement;
  private lastInput?: VoiceConnectInput;
  private reconnecting?: Promise<void>;
  private generation = 0;
  private responseId?: string;
  private readonly transcriptText = new Map<string, string>();

  constructor(
    callbacks: TransportCallbacks = {},
    environment: Partial<TransportEnvironment> = {},
  ) {
    this.callbacks = callbacks;
    this.environment = { ...defaultEnvironment, ...environment };
  }

  async connect(input: VoiceConnectInput): Promise<MediaStream> {
    this.lastInput = input;
    return this.open(input);
  }

  async reconnect(): Promise<void> {
    if (!this.lastInput) {
      throw new Error("Realtime voice has not been started");
    }
    if (this.reconnecting) {
      return this.reconnecting;
    }
    this.callbacks.onPhaseChange?.("reconnecting");
    this.reconnecting = this.open(this.lastInput)
      .then(() => undefined)
      .finally(() => {
        this.reconnecting = undefined;
      });
    return this.reconnecting;
  }

  async disconnect(): Promise<void> {
    this.lastInput = undefined;
    this.disconnectCurrent();
    this.callbacks.onPhaseChange?.("inactive");
  }

  async setMuted(muted: boolean): Promise<void> {
    this.stream?.getAudioTracks().forEach((track) => {
      track.enabled = !muted;
    });
  }

  async setSpeakerEnabled(enabled: boolean): Promise<void> {
    if (this.output) {
      this.output.muted = !enabled;
      if (enabled) {
        await this.output.play().catch(() => undefined);
      }
    }
  }

  async interruptPlayback(): Promise<void> {
    this.sendEvent({
      type: "response.cancel",
      ...(this.responseId ? { response_id: this.responseId } : {}),
    });
    this.sendEvent({ type: "output_audio_buffer.clear" });
    this.callbacks.onPhaseChange?.("interrupted");
  }

  private async open(input: VoiceConnectInput): Promise<MediaStream> {
    this.disconnectCurrent();
    const generation = this.generation;
    const session = await this.environment.invoke<DesktopRealtimeSession>(
      "desktop_establish_realtime_session",
    );
    validateSession(session, this.environment.nowSeconds());

    // The trusted runtime must mint and validate the short-lived credential
    // before the browser asks for microphone access.
    const stream = await input.acquireMicrophone();
    const peer = this.environment.createPeerConnection();
    const output = this.environment.createAudioElement();
    output.autoplay = true;
    output.muted = false;

    try {
      if (input.speakerId) {
        const sink = output as HTMLAudioElement & {
          setSinkId?: (deviceId: string) => Promise<void>;
        };
        await sink.setSinkId?.(input.speakerId);
      }

      peer.ontrack = (event) => {
        if (generation !== this.generation) return;
        output.srcObject = event.streams[0] ?? new MediaStream([event.track]);
        void output.play().catch(() => undefined);
      };
      peer.onconnectionstatechange = () => {
        if (generation !== this.generation) return;
        if (peer.connectionState === "connected") {
          this.callbacks.onPhaseChange?.("listening");
        } else if (peer.connectionState === "failed") {
          void this.reconnect().catch(() => {
            this.callbacks.onPhaseChange?.("error");
          });
        }
      };

      const channel = peer.createDataChannel("oai-events");
      channel.onmessage = (event) => this.handleServerEvent(event.data);
      channel.onerror = () => this.callbacks.onPhaseChange?.("error");

      stream.getAudioTracks().forEach((track) => peer.addTrack(track, stream));
      const offer = await peer.createOffer();
      if (!offer.sdp) throw new Error("Realtime WebRTC offer omitted SDP");
      await peer.setLocalDescription(offer);

      const form = new FormData();
      form.append(
        "sdp",
        new Blob([offer.sdp], { type: "application/sdp" }),
        "offer.sdp",
      );
      const response = await this.environment.fetch(session.webrtcCallUrl, {
        method: "POST",
        headers: { Authorization: `Bearer ${session.authorizationToken}` },
        body: form,
      });
      if (!response.ok) {
        throw new Error(`Realtime WebRTC handshake returned HTTP ${response.status}`);
      }
      const answer = await response.text();
      if (!answer.trim()) throw new Error("Realtime WebRTC answer omitted SDP");
      await peer.setRemoteDescription({ type: "answer", sdp: answer });

      if (generation !== this.generation) {
        stopStream(stream);
        channel.close();
        peer.close();
        throw new Error("Realtime WebRTC connection was superseded");
      }
      this.peer = peer;
      this.channel = channel;
      this.stream = stream;
      this.output = output;
      return stream;
    } catch (error) {
      stopStream(stream);
      peer.ontrack = null;
      peer.onconnectionstatechange = null;
      peer.close();
      output.pause();
      output.srcObject = null;
      throw safeConnectionError(error);
    }
  }

  private disconnectCurrent(): void {
    this.generation += 1;
    this.channel?.close();
    this.channel = undefined;
    if (this.peer) {
      this.peer.ontrack = null;
      this.peer.onconnectionstatechange = null;
      this.peer.close();
      this.peer = undefined;
    }
    if (this.stream) {
      stopStream(this.stream);
      this.stream = undefined;
    }
    if (this.output) {
      this.output.pause();
      this.output.srcObject = null;
      this.output = undefined;
    }
    this.responseId = undefined;
    this.transcriptText.clear();
  }

  private sendEvent(payload: Record<string, unknown>): void {
    if (!this.channel || this.channel.readyState !== "open") return;
    this.channel.send(JSON.stringify(payload));
  }

  private handleServerEvent(raw: unknown): void {
    let event: RealtimeEvent;
    try {
      event = JSON.parse(String(raw)) as RealtimeEvent;
    } catch {
      this.callbacks.onPhaseChange?.("error");
      return;
    }
    switch (event.type) {
      case "session.created":
      case "session.updated":
        this.callbacks.onPhaseChange?.("listening");
        break;
      case "input_audio_buffer.speech_started":
        this.callbacks.onPhaseChange?.("user-speaking");
        break;
      case "input_audio_buffer.speech_stopped":
      case "response.created":
        this.responseId = readNestedString(event, "response", "id");
        this.callbacks.onPhaseChange?.("thinking");
        break;
      case "output_audio_buffer.started":
        this.responseId = readString(event, "response_id") ?? this.responseId;
        this.callbacks.onPhaseChange?.("assistant-speaking");
        break;
      case "output_audio_buffer.stopped":
        this.responseId = undefined;
        this.callbacks.onPhaseChange?.("listening");
        break;
      case "output_audio_buffer.cleared":
        this.responseId = undefined;
        this.callbacks.onPhaseChange?.("interrupted");
        break;
      case "conversation.item.input_audio_transcription.delta":
        this.updateTranscript(event, "user", false, "delta");
        break;
      case "conversation.item.input_audio_transcription.completed":
        this.updateTranscript(event, "user", true, "transcript");
        break;
      case "response.output_audio_transcript.delta":
      case "response.audio_transcript.delta":
        this.updateTranscript(event, "assistant", false, "delta");
        break;
      case "response.output_audio_transcript.done":
      case "response.audio_transcript.done":
        this.updateTranscript(event, "assistant", true, "transcript");
        break;
      case "error":
        this.callbacks.onPhaseChange?.("error");
        break;
      default:
        break;
    }
  }

  private updateTranscript(
    event: RealtimeEvent,
    role: VoiceTranscript["role"],
    final: boolean,
    textField: "delta" | "transcript",
  ): void {
    const id = readString(event, "item_id");
    const value = readString(event, textField);
    if (!id || value === undefined) return;
    const key = `${role}:${id}`;
    const text = final ? value : `${this.transcriptText.get(key) ?? ""}${value}`;
    this.transcriptText.set(key, text);
    this.callbacks.onTranscript?.({ id: key, role, text, final });
  }
}

function validateSession(session: DesktopRealtimeSession, nowSeconds: number): void {
  if (
    !session.authorizationToken ||
    session.expiresAt <= nowSeconds + MINIMUM_CREDENTIAL_LIFETIME_SECONDS
  ) {
    throw new Error("Realtime session credential is expired or invalid");
  }
  let endpoint: URL;
  try {
    endpoint = new URL(session.webrtcCallUrl);
  } catch {
    throw new Error("Realtime WebRTC endpoint is invalid");
  }
  if (endpoint.protocol !== "https:") {
    throw new Error("Realtime WebRTC endpoint must use HTTPS");
  }
}

function stopStream(stream: MediaStream): void {
  stream.getTracks().forEach((track) => track.stop());
}

function safeConnectionError(error: unknown): Error {
  const message = error instanceof Error ? error.message : "Realtime WebRTC connection failed";
  if (message.startsWith("Realtime ")) return new Error(message);
  return new Error("Realtime WebRTC connection failed");
}

function readString(event: RealtimeEvent, field: string): string | undefined {
  const value = event[field];
  return typeof value === "string" ? value : undefined;
}

function readNestedString(
  event: RealtimeEvent,
  objectField: string,
  field: string,
): string | undefined {
  const object = event[objectField];
  if (!object || typeof object !== "object") return undefined;
  const value = (object as Record<string, unknown>)[field];
  return typeof value === "string" ? value : undefined;
}
