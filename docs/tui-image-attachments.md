# TUI image attachments

The terminal UI uses the shared `medusa-runtime` prompt attachment model for clipboard and selected image files.

- Clipboard images retain image-first `Ctrl+V` behavior.
- Selected files are decoded to RGBA8 and passed through the same shared validation limits.
- Attachment summaries expose count, dimensions, and payload size without logging image data.
- Removal updates the durable draft revision so restored drafts remain consistent.

Provider compatibility is checked before prompt submission; unsupported image-bearing prompts must remain in the composer with an actionable error rather than becoming text-only requests.
