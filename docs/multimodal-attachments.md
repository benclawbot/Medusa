# Shared multimodal attachments

Medusa owns prompt attachment validation in `medusa-runtime::prompt`, backed by the dedicated `attachment` module.

Frontends must convert clipboard, file-picker, drag-and-drop, and screenshot input into the shared prompt attachment types before submission. The shared module owns byte, pixel, count, total-size, cursor, and NUL validation so desktop and TUI behavior cannot silently diverge.

Provider-specific serialization remains outside this module. Provider routes must consume the canonical prompt representation and either encode every image block or reject the request before transmission.

The existing public `medusa_runtime::prompt::*` API is preserved by re-exporting the canonical module, so current TUI, session, and runtime callers continue using the same types while ownership is centralized.

## Desktop file-picker behavior

The desktop composer `+` picker accepts any regular file type. PNG, JPEG, WebP, and GIF selections continue through the validated image path. Other files are staged as generic file attachments.

For prompt context, UTF-8 files are included as text. Non-UTF-8 files are included losslessly as base64 inside an `attached_binary_file` block so arbitrary binary formats do not fail before the provider turn starts. This is a transport representation, not format-specific parsing; the model may need tools or domain knowledge to interpret formats such as PDF, ZIP, XLSX, or executables.

Generic non-image files are limited to 2 MiB each, matching the runtime prompt-context boundary. Images retain the 20 MiB per-image limit, the 10-image count limit, and the shared 50 MiB total attachment limit. The desktop rejects an oversized generic file before staging it.

In-memory image and generic-upload data URLs are intentionally not persisted in desktop draft storage. If the app is restarted before submission, those selected files must be attached again.

