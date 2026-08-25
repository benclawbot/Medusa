# Realtime voice verification

Medusa's required realtime-voice validation is deterministic. It uses synthetic PCM frames, recording transports, and browser media mocks; ordinary pull-request CI never requires a microphone, speaker, paid provider call, or live OAuth account.

## Required checks

Run the shared session and resilience contracts:

```bash
cargo test -p medusa-runtime --test realtime_voice_contract
```

Run the OpenAI Realtime protocol and OAuth-safety fixtures:

```bash
cargo test -p medusa-openai-realtime
```

Run TUI voice lifecycle and interaction tests:

```bash
cargo test -p medusa-tui voice
```

The full repository authority remains:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-targets --all-features
```

## Enforced contracts

Required tests prove that:

- shared state transitions reject invalid lifecycle movement;
- partial transcript updates collapse into one final turn;
- audio queues remain bounded under backpressure;
- barge-in clears queued playback within the controlled test budget without cancelling the coding task;
- stop playback, cancel response, cancel task, mute, reconnect, and close remain distinct actions;
- voice, text, tool, approval, and final-result events preserve ordering;
- provider fixtures cover setup, input audio, output audio, transcripts, voice activity, completion, errors, cancellation, truncation, and reconnect;
- OAuth capability failure remains explicit and never falls back to asking for an API key;
- protocol errors do not echo OAuth credentials, raw audio, or transcript payloads;
- repeated connect/disconnect cycles release every registered resource and clear all buffers;
- TUI and Telegram retain text input and authoritative approval behavior while their voice surfaces are active;
- unsupported device, permission, remote, container, WSL, CI, headless, and capability-unavailable paths fail closed while text mode remains usable.

## Latency budgets

The shared deterministic barge-in contract currently allows **20 ms** from invoking interruption to an empty playback queue. This is a software-control budget, not an end-user acoustic latency guarantee. Real hardware, operating-system mixers, network transport, and provider processing are measured only by opt-in tests.

Capture-to-transcript and end-of-turn-to-first-audio measurements require a transport or fixture that supplies timestamps at the relevant boundaries. They must be reported separately from the deterministic local queue budget.

## Platform behavior

Windows, macOS, and Ubuntu run the shared Rust contracts. Browser media tests use synthetic `MediaStream` and `MediaDeviceInfo` fixtures because hosted CI generally has no reliable audio hardware. Platform-specific real-device behavior may be skipped only with an explicit reason in the opt-in report; required synthetic coverage must not be skipped.

## Opt-in live smoke test

A live smoke test is limited to a separately supported TUI or Telegram Realtime route. It is permitted only when an authenticated OAuth Realtime route is already configured and the operator explicitly enables it. It must:

1. probe capability before requesting microphone access;
2. use the existing supported OAuth gateway rather than requesting an API key;
3. avoid logging credentials, raw audio, and sensitive transcripts;
4. exercise connect, one spoken turn, one assistant audio turn, barge-in, reconnect, and close;
5. release all device and transport handles even after failure.

Live-service and real-device checks are never required for ordinary pull requests or releases unless the project explicitly changes that policy.
