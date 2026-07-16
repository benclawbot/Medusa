use medusa_tui::app::{AppState, Scrollback};
use std::sync::Arc;

use medusa_tui::clipboard::UnsupportedClipboard;

#[test]
fn scrollback_offset_starts_at_zero() {
    let sb = Scrollback::default();
    assert_eq!(sb.offset, 0);
}

#[test]
fn scrollback_scroll_up_caps_at_max() {
    let mut sb = Scrollback::default();
    sb.scroll_up(10, 50);
    assert_eq!(sb.offset, 10);
    sb.scroll_up(100, 50);
    assert_eq!(sb.offset, 50);
}

#[test]
fn scrollback_scroll_down_clamps_at_zero() {
    let mut sb = Scrollback { offset: 5 };
    sb.scroll_down(10);
    assert_eq!(sb.offset, 0);
}

#[test]
fn wrap_to_width_preserves_full_content() {
    // Sanity check that the wrap helper (now used in place of the
    // old truncate) preserves the full string when narrower than width.
    // Wrapping inserts '\n' at width boundaries, so removing the newlines
    // recovers the original content.
    let big = "x".repeat(8_000);
    let wrapped = medusa_tui::wrap_to_width(&big, 80);
    let stripped = wrapped.replace('\n', "");
    assert_eq!(stripped, big);
}

#[test]
fn app_state_initializes_scrollback_at_zero() {
    let dir = tempfile::tempdir().expect("tempdir");
    let app = AppState::new(
        dir.path().to_path_buf(),
        "scroll-test",
        "",
        Arc::new(UnsupportedClipboard),
    )
    .expect("app");
    assert_eq!(app.scrollback_offset(), 0);
}
