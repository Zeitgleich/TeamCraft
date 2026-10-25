# Design

Every change that affects how TeamCraft looks, moves or feels — desktop, web, Android and iOS —
follows these rules. They apply to plans, implementation and review alike.

## Apple design is mandatory

- All design work **must use the `apple-design` skill**: layout, typography, spacing, colour,
  materials, components, motion, gestures and interaction feedback. Load it before designing or
  changing UI, and review UI changes against it.
- Motion work additionally uses the `animate` / `animation-performance` skills; the findings and
  decisions live in [docs/motion-audit.md](../../docs/motion-audit.md).
- The goal is polish (umbrella #273): smooth, snappy, restrained, consistent across platforms, in
  light and dark, at phone width.

## No emoji as icons

- The app's own design never uses emoji: no emoji icons, state markers, bullets, placeholders or
  emoji inside UI strings (e.g. no clock emoji for "sending", no coloured circle for presence, no
  "📷 Photo" in notification text).
- Icons come from the app's SVG line icon set (desktop/web), Material Symbols (Android) and SF
  Symbols (iOS).
- Emoji are user content only: messages, reactions and quick reactions, the emoji picker, custom
  status (including the status presets' emoji). Keyboard symbols (⌥ ⌘ ⇧) are not emoji.
- Enforced by a CI check (#575).
