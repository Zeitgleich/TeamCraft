# Motion audit

Audit of every animation in the UI (issue #429, umbrella #273), against the rules of the
`improve-animations` skill. Scope: `crates/teamcraft-ui/style/*.css`, the Rust motion helpers
(`motion.rs`, `toast_motion.rs`, `attention.rs`, `components/sliding_indicator.rs`,
`components/lightbox_motion.rs`, `lightbox/physics.rs`), `loader.js`, and the iOS and Android shells.

## Verdict

The motion system is in good shape: three durations and four curves as tokens
(`--dur-*`, `--ease-*`), movement only on `transform`/`opacity`/`translate`, no `transition: all`,
no layout properties animated, exits through `motion-ghost` copies (popovers since #639), a reduced-motion block for every
entrance, and the end-of-motion snap fixed (#514, `will-change` allow list in `scripts/check-css.py`).
The findings below are corrections to curves and one paint-heavy loop, plus token consolidation.
Nothing in the first pass was feel-breaking; #639 later found two HIGH faults in the popovers (8, 9).

## Motion map

| Area                  | Where                                                | Mechanism                                   | Frequency  |
| --------------------- | ---------------------------------------------------- | ------------------------------------------- | ---------- |
| Channel switch        | `.main > *` `motion-view-in`                         | CSS keyframes, opacity, 120 ms              | very high  |
| Popover, emoji picker | `motion-popover-in/out`, `PopoverBackdrop`           | keyframes + ghost, origin from trigger      | high       |
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

| #  | Sev    | Category      | Location                                                                              | Finding                                                                                                         | Fix                                                           | Status                                                     |
| -- | ------ | ------------- | ------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------- | ---------------------------------------------------------- |
| 1  | MEDIUM | Easing        | `--ease-exit` (17 uses)                                                               | `cubic-bezier(0.3, 0, 1, 1)` is an ease-in: every exit starts slowly, in the frames the eye is on               | Strong ease-out `cubic-bezier(0.23, 1, 0.32, 1)`              | fixed                                                      |
| 2  | MEDIUM | Physicality   | `@keyframes motion-pop`                                                               | `--ease-pop` overshoots per keyframe segment; the 1.12 middle keyframe stacks a second overshoot and undershoot | One segment `0.7 -> 1`, the curve's own overshoot is the pop  | fixed                                                      |
| 3  | MEDIUM | Performance   | `.incoming-ring`, `@keyframes ring`                                                   | Infinite `box-shadow` animation repaints every frame while a call rings                                         | `::after` ring animated with `transform` and `opacity`        | fixed                                                      |
| 4  | LOW    | Cohesion      | window buttons, `.tile`, `.auth-card .input`, `.lazy-area .spinner`, `.media-toolbar` | Hard-coded `120ms ease`, `0.15s ease`, `0.2s ease-out`, and a bare default easing next to the tokens            | Use `--dur-*` and `--ease-standard`                           | fixed                                                      |
| 5  | LOW    | Accessibility | `ChatPane.swift:163,175,390`                                                          | `withAnimation { scrollTo }` ignores Reduce Motion and uses the default curve                                   | `@Environment(\.accessibilityReduceMotion)`, `.easeOut(0.2)`  | fixed (not compiled: no Swift toolchain on the Linux host) |
| 6  | LOW    | Purpose       | `.main > *` view fade                                                                 | Runs on every channel switch (very high frequency). 120 ms of opacity only; hides a cached channel's repaint    | Keep; revisit if the channel-switch budget (100 ms p95) slips | accepted                                                   |
| 7  | LOW    | Performance   | `.msg-highlight`                                                                      | Animated `background` for 2.5 s on one row                                                                      | Overlay `::before`, fades `opacity`                           | fixed                                                      |
| 8  | HIGH   | Interaction   | every `.popover-backdrop`                                                             | Closed on `mousedown`; WebKit then sent the press's `click` to the trigger under it: close and reopen (#639)    | `PopoverBackdrop` closes on `click`, after the whole press    | fixed                                                      |
| 9  | HIGH   | Exit          | `MenuSurface` popovers, lightbox, file, link, date and message menus, emoji picker    | No ghost: the popover vanished in one frame (only the status menu had one)                                      | `PopoverBackdrop` leaves the popover's ghost                  | fixed                                                      |
| 10 | MEDIUM | Interaction   | `.modal-backdrop`                                                                     | Same as 8: closed on `mousedown`, the `click` reached what was under it                                         | Closes on a press that starts and ends on the backdrop        | fixed                                                      |
| 11 | MEDIUM | Focus         | popover backdrops                                                                     | The press moved the focus to the page before the menu closed: the composer's focus ring blinked off and on      | `mousedown` default prevented; the owner gives the focus back | fixed                                                      |

## Popovers on WebKit (#639)

A recording from the Linux desktop app (WebKitGTK) showed the composer's send options opening and
closing several times while the pointer rested on their button, each time with one faint frame and
no exit, the composer's focus ring blinking along, and rows of the menu drawn over each other.

- **One toggle per press.** The cause of the flicker was finding 8: the trigger toggled the menu on
  `click`, the backdrop closed it on `mousedown`. A press on the open menu's button closed it on the
  way down, the backdrop left, and WebKit delivered the `click` of the same press to the button,
  which opened the menu again (Chromium sends it to the common ancestor, so it hid there). Each try
  to close restarted the entrance: the "faint frame, then snap". All popovers now sit on
  `PopoverBackdrop` (`components/sheet.rs`): it takes the whole press and closes on `click` (or a
  right click), so the trigger only sees presses that start while its popover is closed. Open and
  close are two states driven by one signal per popover; nothing opens on hover or focus.
- **Exit before removal.** `PopoverBackdrop` also leaves the popover's ghost (`use_leave_ghost`), so
  every popover and the emoji picker play `motion-popover-out` instead of vanishing. The real
  element goes at once, so a reopen during the 100 ms exit starts a fresh entrance over the fading
  copy (interruptible; nothing waits for the animation).
- **No measured or layout motion.** Popovers animate `opacity` and `transform` only; no height,
  FLIP or position animation runs on them, so nothing depends on rows being measured. The
  overlapping rows were not reproduced in Chromium or in the code: menu rows are normal flow
  (`.menu-item` grows with its two-line label). They appeared only in the frames of the flicker, so
  the likeliest cause is WebKitGTK presenting a stale layer while the entrance restarted every
  press; with one toggle per press it no longer restarts. Recheck on the next desktop build.
- **Reduced motion.** Unchanged: popovers fade in, and their ghosts (now including the emoji
  picker's) fade out.
- **Test:** `tests/e2e/tests/suite/web.rs`
  `a_popover_opened_once_stays_open_while_the_pointer_rests` counts every time the menu is added and
  removed while the pointer rests on its button and after a second press.

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

- Finding 5 is not compiled here (no Swift toolchain on the Linux host); check it with the next iOS build.
- Finding 6 stays as is on purpose: the fade is the cheapest cover for a channel's first paint.
- #639: the overlapping menu rows were not reproduced here (no WebKitGTK capture); check the send
  options on the next Linux desktop build.
