// The TUI's reserved `current` key is a fresh runtime session. Explicit resume/continue keys
// remain durable and are restored by the single implementation below.
include!("draft_store_base.rs");
