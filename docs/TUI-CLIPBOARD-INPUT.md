# Clipboard Text and Screenshot Input

**Status:** Mandatory part of the interactive TUI acceptance contract  
**Applies to:** `feature/interactive-tui` and PR #19  
**Purpose:** Let users paste text and screenshots directly into the active prompt so Medusa can use them as task context.

## 1. Required user experience

The composer must accept both ordinary clipboard text and image clipboard contents.

### Text paste

- Standard terminal paste inserts text at the current cursor position.
- Multiline text is inserted as a single guarded paste operation, never interpreted as immediate commands.
- Large pastes show a compact attachment-style marker in the composer while preserving the exact text in draft state.
- The user can expand, edit, remove, or replace pasted text before submission.
- Pasted text is submitted as part of the current user turn and persisted in the session transcript.
- Line endings are normalized without changing semantic content.
- NUL bytes and invalid UTF-8 are rejected with an actionable message.
- Secret detection and redaction warnings run before model submission, but the original draft is not silently mutated.

### Screenshot/image paste

- Pasting while the clipboard contains an image attaches it to the current draft.
- The composer shows a chip such as `screenshot-1.png · 1440×900 · 312 KB`.
- The user can preview metadata, rename, remove, or replace the attachment before submission.
- Multiple images may be attached to one prompt, subject to configurable count and size limits.
- The prompt may combine text, pasted source snippets, and screenshots in one user turn.
- The submitted transcript shows an image attachment marker and a stable evidence identifier, never raw binary bytes.
- The attachment remains available when the draft is reopened after a dialog, resize, temporary disconnect, or application restart.

## 2. Input methods

Required shortcuts and behavior:

| Input | Behavior |
|---|---|
| Normal terminal paste | Insert clipboard text safely |
| `Ctrl+V` | Paste text, or attach an image when the terminal delivers a paste event and clipboard image access is available |
| `Ctrl+Shift+V` | Explicit clipboard inspection and paste/attach action |
| `/paste` | Inspect clipboard and insert text or attach image |
| `/attach-clipboard` | Explicitly attach clipboard image |
| Drag/drop or path paste | Resolve a pasted local image path as a normal file attachment after confirmation |

Terminal-native bracketed paste must be enabled where supported. Key bindings remain configurable.

## 3. Clipboard abstraction

Introduce a platform-neutral service:

```rust
pub trait ClipboardService: Send + Sync {
    fn read(&self) -> ClipboardResult<ClipboardContent>;
}

pub enum ClipboardContent {
    Empty,
    Text(String),
    Image(ClipboardImage),
    Files(Vec<PathBuf>),
}

pub struct ClipboardImage {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    pub source_format: Option<String>,
}
```

Implementations:

- Windows clipboard API for CMD, PowerShell, Windows Terminal, and WSL interop where available.
- macOS pasteboard.
- Linux Wayland and X11 clipboard paths.
- A no-clipboard implementation that returns an actionable unsupported result rather than panicking.

Clipboard reads must never block the render thread. They run through a bounded background task with cancellation and timeout.

## 4. Draft and attachment model

The composer draft becomes structured data rather than one string:

```rust
pub struct PromptDraft {
    pub text: String,
    pub attachments: Vec<PromptAttachment>,
    pub revision: u64,
}

pub enum PromptAttachment {
    PastedText(TextAttachment),
    Image(ImageAttachment),
    File(FileAttachment),
}
```

Each attachment records:

- generated attachment ID
- display name
- MIME type
- byte length
- checksum
- creation time
- source (`clipboard`, `path`, `drag_drop`, or `generated`)
- optional width/height
- durable staging path
- redaction/classification status

Draft state is atomically persisted below `.medusa/sessions/<session>/drafts/`. Binary attachment data is never embedded directly in JSON or Markdown session records.

## 5. Image processing pipeline

1. Read clipboard image pixels.
2. Validate dimensions, decoded byte count, and format.
3. Reject decompression bombs and oversized images before encoding.
4. Strip nonessential metadata, including EXIF and location data.
5. Normalize to a supported lossless or high-quality format, preferring PNG for screenshots.
6. Compute a checksum and deduplicate within the session.
7. Store atomically in the session attachment directory with restrictive permissions.
8. Create a thumbnail only for local UI preview.
9. Add a provider-neutral image block to the user message.
10. Persist only attachment metadata and stable references in the transcript/evidence chain.

Default limits:

- 10 images per prompt
- 20 MB encoded per image
- 40 million decoded pixels per image
- 50 MB total attachments per prompt

Limits are configurable but always bounded by hard safety ceilings.

## 6. Provider contract and guaranteed usability

Extend the provider-neutral message format with an image content block:

```rust
MessageBlock::Image {
    media_type: String,
    data: ImageData,
    alt_text: Option<String>,
}

pub enum ImageData {
    Base64(String),
    AttachmentRef(String),
}
```

Add explicit provider capabilities:

```rust
pub struct ProviderCapabilities {
    pub image_input: bool,
    pub supported_image_media_types: Vec<String>,
    pub max_image_bytes: Option<u64>,
    pub max_images_per_request: Option<u32>,
}
```

Behavior:

- When the configured provider supports image input, Medusa sends the normalized screenshot as a native image content block.
- When it does not, the UI must not silently omit the screenshot.
- The user is shown that the active provider cannot consume images and is offered a configured local image-understanding adapter or a model switch.
- The prompt cannot be submitted while an image would be ignored.
- Any fallback-generated description is displayed to the user and attached as derived context with provenance; it is never represented as the original image.

This guarantees that a pasted screenshot is actually used or submission is explicitly blocked with a corrective action.

## 7. Prompt assembly

On submission, the active user turn is assembled in this order:

1. user-entered text
2. pasted text attachments in composer order
3. image attachments in composer order
4. file attachments in composer order
5. provenance metadata kept outside user-visible prompt text where the provider supports structured blocks

The model receives clear boundaries between user text, pasted logs/source, screenshots, and files. Clipboard content is treated as untrusted task data, never as system instructions or policy overrides.

## 8. Security and privacy

- Clipboard access occurs only after an explicit paste action; Medusa never polls clipboard contents continuously.
- Clipboard contents are not logged before redaction/classification.
- Image metadata is stripped before persistence or provider upload.
- Attachment paths are repository/session contained and symlink-safe.
- Attachment files use restrictive permissions.
- Clipboard text and image-derived text pass through credential and token detection.
- The UI warns when probable credentials, private keys, recovery codes, or authentication screenshots are detected.
- Warnings require explicit confirmation in manual modes and remain non-bypassable where existing secret policy requires denial.
- MCP servers, hooks, shell tools, and workers do not receive prompt attachments unless the exact tool invocation contract grants them access.
- Exported transcripts exclude image bytes by default and use attachment references.
- Deleting a draft or session follows the configured attachment retention policy and produces auditable cleanup evidence.

## 9. Failure behavior

The composer remains intact when:

- clipboard access is unavailable
- clipboard ownership changes mid-read
- image decoding fails
- image exceeds a limit
- provider rejects the media type
- upload fails
- daemon disconnects
- session persistence fails

Errors identify the affected attachment and offer retry, remove, save locally, or switch provider where applicable. A failed image attachment never causes the text portion of the draft to disappear.

## 10. Testing requirements

### Deterministic unit tests

- single-line and multiline text paste
- paste at cursor and over selection
- bracketed paste cannot trigger submit or shell mode
- CRLF/LF normalization
- invalid UTF-8 and NUL rejection
- image dimension, format, checksum, and deduplication
- metadata stripping
- limit enforcement and decompression-bomb rejection
- atomic draft and attachment persistence
- attachment recovery after restart
- provider capability gating
- attachment ordering in prompt assembly
- secret-warning behavior

### Interactive PTY tests

1. Launch `medusa`, paste a multiline compiler error, add an instruction, and submit one combined prompt.
2. Paste a screenshot into an empty prompt, observe the attachment chip, add explanatory text, submit, and verify that the model request contains the image block.
3. Attach two screenshots, remove one, and verify only the remaining image is submitted.
4. Paste text containing characters that resemble terminal control sequences and verify they remain inert text.
5. Paste while a background job is running and verify the active draft is preserved.
6. Restart the TUI before submission and verify the text and screenshot draft recover.
7. Configure a provider without image support and verify submission is blocked rather than silently dropping the screenshot.
8. Paste an oversized image and verify a clear rejection without terminal corruption or memory exhaustion.

### Live coding scenario

In a disposable repository:

1. Present a screenshot of a failing application or test output.
2. Add the prompt: `Use this screenshot to diagnose and fix the issue, then run the relevant tests.`
3. Verify the screenshot is represented in the provider request or through an explicitly approved provenance-preserving fallback.
4. Verify Medusa changes the repository, runs targeted checks, and records transcript, attachment checksum, diff, and verification evidence.

## 11. Acceptance criteria

The clipboard feature is complete only when:

1. Text and screenshots can be pasted into the active composer on every supported desktop platform.
2. Pasted text never executes merely because it contains newlines or command syntax.
3. Screenshot chips are visible, removable, recoverable, and submitted in deterministic order.
4. A screenshot is never silently ignored.
5. Provider capability mismatch produces an explicit corrective workflow.
6. Drafts survive resize, dialogs, daemon reconnect, TUI restart, and process interruption.
7. Clipboard and attachment data obey redaction, containment, permission, size, and retention controls.
8. Deterministic, PTY, cross-platform, security, and credential-gated live coding tests pass.
9. The feature is included in the final PR #19 acceptance checklist and release documentation.
