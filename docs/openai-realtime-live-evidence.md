# OpenAI Realtime live evidence

This developer-only acceptance surface proves the OpenAI Realtime portion of issue #719:

```text
existing ChatGPT/Codex account state
  -> bounded short-lived Realtime credential
  -> explicit microphone activation
  -> final user transcript
  -> assistant remote audio track and playback
```

It does not add an API-key field or a voice-specific credential store. The normal desktop application remains unchanged unless the evidence mode is explicitly enabled.

## Preconditions

- The active Medusa connection is `chatgpt-oauth` and its provider is `openai-oauth`.
- The existing Codex authentication file represents a ChatGPT login that automatically provisioned an OpenAI API credential capable of minting a Realtime client secret.
- A microphone and speaker are available to the desktop WebView.
- The account can use the configured `gpt-realtime` and transcription models.

Unsupported or revoked accounts fail before microphone permission is requested.

## Run the evidence surface

From `apps/medusa-desktop`, explicitly enable evidence mode and start the Tauri development application.

Before launching evidence mode, start the normal desktop application once and select **ChatGPT OAuth**
under Settings, then apply the configuration. The evidence surface reads the shared active provider and
will refuse to start against another route such as MiniMax; it does not silently switch providers or
request microphone permission for an unsupported route. Close the normal desktop application before
launching evidence mode.

Linux or macOS:

```bash
VITE_MEDUSA_OPENAI_REALTIME_EVIDENCE=1 npm run tauri:dev
```

PowerShell:

```powershell
$env:VITE_MEDUSA_OPENAI_REALTIME_EVIDENCE = "1"
npm run tauri:dev
```

The evidence surface is also available to development builds with the query parameter `?openai-realtime-evidence=1`.

1. Confirm that the evidence-only screen is visible.
2. Click **Start 45-second live evidence**. This click is the explicit microphone opt-in.
3. When connected, say: `Medusa live voice evidence. Please answer with a short confirmation.`
4. Pause and allow the server VAD to finalize the turn.
5. Wait for the assistant audio to play.
6. Copy the sanitized evidence JSON only after the screen reports `PASSED`.

If the screen reports that the authenticated account does not expose Realtime, the account's existing
ChatGPT/Codex authentication did not provision a Realtime-capable OpenAI credential. Keep the evidence
open and resolve the account/gateway capability first; do not add an API-key field or paste credentials
into the evidence surface.

The run stops and tears down the microphone, peer connection, data channel, and audio element as soon as it passes or after 45 seconds.

## Evidence contract

A passing report records only:

- ISO start and finish timestamps and the 45-second bound;
- explicit opt-in;
- proof that the short-lived credential completed before `getUserMedia` was called;
- the presence and state of an audio microphone track;
- SHA-256 and character count for the final user transcript, never transcript text;
- optional hashed assistant-transcript metadata;
- the provider assistant-audio start event;
- the presence of a remote audio track;
- requested and successfully started audio playback;
- bounded voice phase transitions;
- explicit assertions that credentials, transcript text, and raw audio were not persisted.

The report never contains the long-lived account credential, short-lived Realtime credential, raw audio, or complete transcript.

## Closure evidence for #719

Do not close #719 from deterministic tests alone. Attach the passing sanitized JSON to the issue or the final pull request and include:

- the commit SHA that produced the desktop build;
- operating system and desktop WebView version;
- confirmation that the active provider was `openai-oauth` and no API-key UI was used;
- confirmation that the spoken prompt was heard as assistant audio;
- the passing evidence JSON.

If the report is `FAILED`, keep #719 open and fix the reported live boundary before rerunning it.
