# TUI keyboard shortcuts

This reference covers the interactive `medusa tui` interface.

## Composer

| Shortcut | Action |
| --- | --- |
| `Enter` | Submit the current prompt. While a run is active, queue a follow-up for the next turn. |
| `Ctrl+V` | Paste text, an image, or file attachments from the clipboard. |
| `Tab` | Complete or move through slash-command suggestions. |
| `Up` / `Down` | Move through command suggestions when the slash-command menu is open. |

## Transcript and panels

| Shortcut | Action |
| --- | --- |
| `Page Up` / `Page Down` | Scroll the transcript by ten rows. |
| `Ctrl+Home` | Jump to the oldest available transcript content. |
| `Ctrl+End` | Return to the newest transcript content. |
| Mouse wheel | Scroll the transcript by three rows. |
| `Ctrl+T` | Show or hide the task list panel. |
| `Ctrl+L` | Force a terminal redraw. |

## Runs and sessions

| Shortcut | Action |
| --- | --- |
| `Ctrl+C` | Interrupt the active run. Press twice within one second to quit the TUI. |
| `Esc` | Interrupt an active run when no modal is open. Otherwise close or leave the current interaction. |
| `Ctrl+D` | Exit when the composer is empty. |

## Modal controls

Model configuration and question modals show context-specific help in the footer. Common controls include `Tab`, arrow keys, `Enter`, `Space`, `Shift+Tab`, and `Esc`.

The footer inside the TUI remains the source of truth for controls that depend on the current mode.
