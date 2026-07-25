# Medusa desktop UX

Medusa Desktop uses an activity-first visual hierarchy inspired by the strongest interaction patterns in modern AI desktop applications while retaining Medusa's coding-agent identity.

## Hierarchy

1. **Conversation and composer** remain the primary surface.
2. **Runtime state** is visible but quiet in the header.
3. **Plan and activity** are compact, scannable execution evidence in the inspector.
4. **Settings, sessions, diffs, and memory** stay available without permanently competing with the active conversation.

## Interaction principles

- Progressive disclosure: default surfaces show status and summaries; dedicated docks retain deeper evidence.
- Clear state: ready, working, completed, and failed states use consistent motion and semantic color.
- Calm density: cards, spacing, and typography separate user intent, assistant output, runtime notices, plans, and tool activity.
- Keyboard-first composition: the composer remains the dominant action target and preserves slash commands, attachments, cancellation, and queued guidance.
- Responsive focus: narrow desktop windows prioritize conversation; secondary inspection surfaces become transient.
- Accessibility: reduced-motion preferences disable decorative animation, focus states remain visible, and semantic runtime colors are never the only signal.

## Visual system

The final override layer is `apps/medusa-desktop/src/desktop-ux-overhaul.css`. It is deliberately isolated from the legacy shared stylesheet so the desktop visual system can evolve without risking TUI/runtime behavior or unrelated Zeus-derived components.

The layer provides:

- a calmer neutral canvas and restrained Medusa accent;
- stronger conversation hierarchy and compact user bubbles;
- a floating, high-affordance composer;
- live working-state motion;
- task-tree connectors for plan steps;
- rich, scannable tool-activity cards;
- responsive inspector behavior;
- reduced-motion support.
