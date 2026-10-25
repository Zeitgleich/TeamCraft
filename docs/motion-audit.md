# Motion audit

Audit of every animation in the UI (issue #429, umbrella #273), against the rules of the
`improve-animations` skill. Scope: `crates/teamcraft-ui/style/*.css`, the Rust motion helpers
(`motion.rs`, `toast_motion.rs`, `attention.rs`, `components/sliding_indicator.rs`,
`components/lightbox_motion.rs`, `lightbox/physics.rs`), `loader.js`, and the iOS and Android shells.

## Verdict

The motion system is in good shape: three durations and four curves as tokens
(`--dur-*`, `--ease-*`), movement only on `transform`/`opacity`/`translate`, no `transition: all`,
no layout properties animated, exits through `motion-ghost` copies, a reduced-motion block for every
entrance, and the end-of-motion snap fixed (#514, `will-change` allow list in `scripts/check-css.py`).
The findings below are corrections to curves and one paint-heavy loop, plus token consolidation.
Nothing is feel-breaking (no HIGH).

## Motion map

| Area                  | Where                                                | Mechanism                                   | Frequency  |
| --------------------- | ---------------------------------------------------- | ------------------------------------------- | ---------- |
| Channel switch        | `.main > *` `motion-view-in`                         | CSS keyframes, opacity, 120 ms              | very high  |
| Popover, emoji picker | `motion-popover-in/out`                              | keyframes + ghost, origin from trigger      | high       |
| Modal, sheet          | `motion-dialog-*`, `sheet-*`                         | keyframes + ghost; sheet drag in Rust       | medium     |
| Toast                 | `motion-toast-*`, `toast_motion.rs`                  | keyframes + FLIP `translate`, swipe         | medium     |
| Tab/segment indicator | `.tab-indicator`, `sliding_indicator.rs`             | transform transition                        | high       |
| Sidebar sections      | `motion-section-open/close`, chevron                 | keyframes                                   | medium     |
| Messages              | `motion-message-in`, `msg-settling`, `msg-highlight` | keyframes                                   | high       |
| Reactions, badges     | `motion-pop`, `motion-dot-in`                        | keyframes with `--ease-pop`                 | medium     |
| Compact navigation    | `pane-push/pop/leave`, edge swipe                    | keyframes + ghost + Rust drag               | high       |
| Lightbox              | `lightbox_motion.rs`, `physics.rs`                   | rAF driver, springs, `will-change` (listed) | low        |
| Crop dialog           | `.crop-stage.is-animated`                            | transform transition                        | rare       |
| Incoming call         | `.incoming-ring`                                     | looping keyframes                           | rare       |
| Loader hand-over      | `loader.js`, `style/loader.css`                      | WAAPI, `.is-gone` before removal            | once       |
| iOS                   | `LoadingMarkView.swift`, `ChatPane.swift`            | Canvas timeline, `withAnimation`            | once, high |
| Android               | none (Compose has no custom motion)                  | -                                           | -          |

## Findings

| # | Sev    | Category      | Location                                                                              | Finding                                                                                                         | Fix                                                           | Status                                      |
| - | ------ | ------------- | ------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------- | ------------------------------------------- |
| 1 | MEDIUM | Easing        | `--ease-exit` (17 uses)                                                               | `cubic-bezier(0.3, 0, 1, 1)` is an ease-in: every exit starts slowly, in the frames the eye is on               | Strong ease-out `cubic-bezier(0.23, 1, 0.32, 1)`              | fixed                                       |
| 2 | MEDIUM | Physicality   | `@keyframes motion-pop`                                                               | `--ease-pop` overshoots per keyframe segment; the 1.12 middle keyframe stacks a second overshoot and undershoot | One segment `0.7 -> 1`, the curve's own overshoot is the pop  | fixed                                       |
| 3 | MEDIUM | Performance   | `.incoming-ring`, `@keyframes ring`                                                   | Infinite `box-shadow` animation repaints every frame while a call rings                                         | `::after` ring animated with `transform` and `opacity`        | fixed                                       |
| 4 | LOW    | Cohesion      | window buttons, `.tile`, `.auth-card .input`, `.lazy-area .spinner`, `.media-toolbar` | Hard-coded `120ms ease`, `0.15s ease`, `0.2s ease-out`, and a bare default easing next to the tokens            | Use `--dur-*` and `--ease-standard`                           | fixed                                       |
| 5 | LOW    | Accessibility | `ChatPane.swift:163,175,390`                                                          | `withAnimation { scrollTo }` ignores Reduce Motion and uses the default curve                                   | `@Environment(\.accessibilityReduceMotion)`, `.easeOut(0.2)`  | open (no Swift toolchain on the Linux host) |
| 6 | LOW    | Purpose       | `.main > *` view fade                                                                 | Runs on every channel switch (very high frequency). 120 ms of opacity only; hides a cached channel's repaint    | Keep; revisit if the channel-switch budget (100 ms p95) slips | accepted                                    |
| 7 | LOW    | Performance   | `.msg-highlight`                                                                      | Animates `background` for 2.5 s on one row                                                                      | Keep: one element, paint only, rare                           | accepted                                    |

## Checked and fine

- No `transition: all`, no animated `width`/`height`/`margin`/`top`/`left`, no `scale(0)`; the smallest
  start scale is 0.4 (`motion-dot-in`, together with opacity).
- Press feedback (`--press-scale`) and hovers are gated (`:active`, `(hover: hover)`), reduced motion sets
  the scale to 1.
- Popovers grow from their trigger edge (`--popover-origin`), modals from the centre.
- Interruptible: toasts, indicators, sheets and the lightbox use transitions or the rAF driver, so a new
  target starts from the current position. Keyframes are used only for one-way entrances and exits.
- Reduced motion: every entrance becomes a fade, indicators jump, pops and loops stop, Rust-measured
  motion checks `prefers_reduced_motion()` (`toast_motion.rs`).
- #514: ghosts get `.is-gone` and leave two frames later (`motion.rs`), the loader does the same
  (`loader.js`); `will-change` stays limited to the list in `scripts/check-css.py`.
- Android: no custom animation code.

## Missed opportunities

None worth adding. The UI already animates every state change that teleports; more motion on the
high-frequency paths (channel switch, list navigation) would cost more than it gives.

## Open

- Finding 5 needs a Swift build to verify; do it with the next iOS change.
