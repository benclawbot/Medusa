# Shared multimodal attachments

Medusa owns prompt attachment validation in `medusa-runtime::prompt`, backed by the dedicated `attachment` module.

Frontends must convert clipboard, file-picker, drag-and-drop, and screenshot input into the shared prompt attachment types before submission. The shared module owns byte, pixel, count, total-size, cursor, and NUL validation so desktop and TUI behavior cannot silently diverge.

Provider-specific serialization remains outside this module. Provider routes must consume the canonical prompt representation and either encode every image block or reject the request before transmission.

The existing public `medusa_runtime::prompt::*` API is preserved by re-exporting the canonical module, so current TUI, session, and runtime callers continue using the same types while ownership is centralized.
