# TUI troubleshooting

Use these checks when the interactive terminal looks stuck, fails to accept input, or is not showing the expected transcript state.

## The screen looks stale or partially drawn

Press `Ctrl+L` to force a terminal redraw. This is useful after terminal resizing, reconnecting to a remote shell, or display corruption.

## A run is still active

Press `Ctrl+C` once to request cancellation. Pressing it twice within one second exits the TUI, so avoid a rapid second press unless you intend to quit.

When no modal is open, `Esc` also interrupts the active run.

## The transcript is not at the newest output

- Press `Ctrl+End` to return to the newest transcript content.
- Use `Page Down` or the mouse wheel to move toward the latest rows.
- Press `Ctrl+Home` to jump to the oldest available transcript content.

## The task list is missing

Press `Ctrl+T` to show or hide the task list panel.

## Clipboard paste does not behave as expected

`Ctrl+V` can paste text, attach an image, or add file attachments depending on the clipboard contents and platform support. If clipboard integration is unavailable, type the prompt directly or provide the file path in the prompt.

## A prompt was rejected

Rejected submissions restore the previous draft and attachments. Review the status message, correct the prompt or environment problem, and submit again.

## Exiting safely

Press `Ctrl+D` while the composer is empty to exit. During an active run, cancel first if you need the runtime to stop its current work.

See [TUI keyboard shortcuts](tui-keyboard-shortcuts.md) for the complete control reference.
