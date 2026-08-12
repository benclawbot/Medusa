# Multimodal verification

Medusa's required multimodal checks are deterministic and do not use external image URLs, provider credentials, microphones, or paid services.

## Required contract suite

Run the public provider boundary tests with:

```bash
cargo test -p medusa-provider --test multimodal_contract
```

The suite starts a loopback recording server and proves that:

- mixed text and image content reaches the outgoing OpenAI-compatible request as an `image_url` data URL;
- image-only prompts remain valid;
- MIME type and base64 payload are preserved exactly at the provider boundary;
- a route without image capability fails before any TCP connection is accepted;
- validation errors do not include the image payload.

The server returns a deterministic local response, so the test exercises the public `ModelProvider::complete` path without contacting a real provider.

## Frontend and shared attachment suites

Run the shared TUI attachment checks with:

```bash
cargo test -p medusa-tui clipboard::tests
```

These cover selected image files, canonical attachment validation, metadata summaries, removal, and draft revision behavior. The normal TUI application tests cover clipboard image-first handling, text fallback, image-only submission, and persisted draft restoration.

Run the desktop suite with:

```bash
cd apps/medusa-desktop
npm ci
npm test
```

The desktop tests cover picker, clipboard, and drag/drop ingestion, preview and removal, compatibility feedback, and submission blocking for known text-only routes.

The `Multimodal Contract` GitHub Actions workflow runs the provider and TUI contract on Ubuntu, macOS, and Windows, and runs the desktop suite on Ubuntu. Platform clipboard APIs are kept behind frontend adapters; deterministic tests inject or generate image content rather than depending on the host clipboard.

## Optional live-provider smoke test

Live vision smoke coverage must remain opt-in and separate from required CI. Use a generated fixture image, a cost-capped authenticated route, and a deterministic question whose answer is visible in the fixture. Record only provider/model identifiers, status, timing, and redacted request metadata. Never log image bytes, data URLs, base64 payloads, OAuth tokens, or API credentials.
