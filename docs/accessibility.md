# Accessibility

TeamCraft has to be usable with a screen reader, with the keyboard alone, with large text and with
reduced motion: people depend on it, and it is a common procurement requirement (WCAG 2.2 AA,
EN 301 549, BITV). This document holds the target, the test matrix, the audit of 2026-11-22
(issue #217) and what is still open.

## Target

| Platform          | Target                                                                                  |
|-------------------|-----------------------------------------------------------------------------------------|
| Web, desktop      | WCAG 2.2 level AA (the desktop app is the same Leptos UI in a webview)                  |
| Android           | WCAG 2.2 AA where it applies, Android accessibility guidelines, TalkBack, Switch Access |
| iOS               | WCAG 2.2 AA where it applies, Apple HIG accessibility, VoiceOver, Full Keyboard Access  |

Severity of a finding:

| Severity | Meaning                                                                                   |
|----------|-------------------------------------------------------------------------------------------|
| Blocker  | A core task is impossible without a mouse or without sight                                |
| High     | A major barrier on a core flow, or a WCAG AA failure there                                |
| Medium   | A barrier with a workaround, or a failure off the core flows                              |
| Low      | Friction, or a best practice beyond AA                                                    |

## Test matrix

| Platform        | Screen reader         | Keyboard / switch                     | Other checks                         |
|-----------------|-----------------------|---------------------------------------|--------------------------------------|
| Windows desktop | NVDA, Narrator        | keyboard only                         | High Contrast, 200 % zoom            |
| macOS desktop   | VoiceOver             | keyboard only                         | Increase Contrast, Reduce Motion     |
| Linux desktop   | Orca (WebKitGTK)      | keyboard only                         | reduced motion                       |
| Web             | NVDA, VoiceOver       | keyboard only                         | 400 % reflow, text spacing           |
| Android         | TalkBack              | Switch Access, external keyboard      | font scale 200 %, colour correction  |
| iOS             | VoiceOver             | Full Keyboard Access, Switch Control  | Dynamic Type, Reduce Motion          |

Core journeys for each row: sign-in and linking, reading and writing, threads, reactions, search,
files, settings, notifications, calls and meetings, admin console, onboarding. The manual walkthrough
is [manual-tests/screen-readers.md](manual-tests/screen-readers.md).

## Audit of 2026-11-22 (#217)

**How:** web and desktop by reading `crates/teamcraft-ui` and with axe-core 4.10 (WCAG 2.0–2.2 A/AA
rules) on the demo build (`scripts/screenshots/web-demo-build.sh`, `?backend=mock`) in light and
dark: home, a channel, threads, activity, settings. Android and iOS by reading the Compose and SwiftUI
code. No real screen reader was run on any platform yet (#2288), and nothing on an iPhone (#2286).

**Verdict:** no blocker: every core task has a keyboard and screen reader route. The groundwork is
strong on all three platforms (below). The High findings were mostly about what a screen reader hears
at the wrong time or not at all: a message list that read out every row scrolled into view, a focused
message that said only its author, call controls that hid themselves from TalkBack and VoiceOver, and
unlabelled switches. axe found 0 violations on home and 4 rules failing in a channel; after the fixes a channel
has none (one more target size, with the composer's suggestions open, is filed).

### Done well

- **Web/desktop:** modals and sheets trap the focus and return it; the conversation menu, the message
  "more" menu and the call leave menu follow the menu pattern (arrows, Home/End, Esc); one roving
  `tab_list.rs` for tabs; quick switcher, people picker, emoji picker and font menu use
  `aria-activedescendant`; the toast region is always mounted; the typing indicator has its own polite
  region; call tiles carry their state in words; `lang` follows the UI language; one global
  `:focus-visible` ring; every `@keyframes` honours reduced motion, and animated images can be paused.
  Design tokens pass AA (muted text 7.6:1 light, 7.7:1 dark; focus ring ≥ 3:1).
- **Android:** messages have custom actions (reply, react, more, select) for the swipe and long-press
  gestures; sliders expose `progressBarRangeInfo` + `setProgress`; form errors use `error()` and focus
  the first invalid field; live regions for call status, lobby, sign-in errors and send failures; no
  icon button without a label; one reduced-motion helper (`rememberMotionReduced`, `LocalMotion`),
  and every infinite animation checks it.
- **iOS:** shared motion tokens take Reduce Motion everywhere but one button; no unlabelled icon
  button; gesture alternatives (swipe to reply, double-tap to react, hold to record, waveform
  adjustable, reorder actions); forms follow forms.md with `AccessibilityFocusState`; codes spelled
  digit by digit; headers and selected traits; the tab bar caps Dynamic Type with the Large Content
  Viewer; floating surfaces respect Reduce Transparency.

### Findings: web and desktop

| Id   | Finding                                                                                                                       | WCAG       | Severity | State   |
|------|-------------------------------------------------------------------------------------------------------------------------------|------------|----------|---------|
| W-1  | The message list and thread were `role="log"` (live): rows mounted by scrolling and older pages were read out; no announcer for real arrivals | 4.1.3 | High | fixed: lists `aria-live="off"`, one polite region says "Name: start of text" for a new message from somebody else |
| W-2  | A focused message read only "author, time" (a named generic `div`)                                                            | 4.1.2      | High     | fixed: `role="article"`, body as `aria-describedby` |
| W-3  | Forward and Save only on the hover toolbar, not in the "more" menu (Shift+F10)                                                 | 2.1.1      | High     | fixed   |
| W-4  | After Delete from the menu the focus fell to the page; Undo (6 s toast) out of reach                                           | 2.2.1, 2.4.3 | High   | focus fixed (returns to the message or its neighbour); undo #2279 |
| W-5  | The app under the full call screen stayed focusable and readable                                                              | 2.4.11     | High     | fixed: `inert` while the call screen is up |
| W-6  | No keyboard way to answer an incoming call in the web app                                                                     | 2.1.1      | High     | fixed: Alt+Shift+A / V / D, named in the announcement |
| W-7  | Composer suggestions: no `aria-activedescendant`, buttons inside options, `aria-expanded` on a textbox (axe: critical)          | 4.1.2      | High     | fixed, with a polite "N suggestions"; `aria-controls` only while the list exists |
| W-8  | Call popovers (share, effects, camera, audio, quality): no focus handling, wrong ARIA                                         | 2.1.1, 4.1.2 | Medium | fixed: focus in, Esc closes and returns, `aria-haspopup` + `aria-expanded` |
| W-9  | Joins, leaves, lobby arrivals and shortcut mutes aren't announced                                                             | 4.1.3      | Medium   | #2281   |
| W-10 | Call status regions mount with their text, so they are often not announced                                                    | 4.1.3      | Medium   | #2281   |
| W-11 | Toggle buttons change their label and set `aria-pressed` ("Unmute, pressed")                                                   | 4.1.2      | Medium   | fixed: a changing label has no `aria-pressed` |
| W-12 | Date and "new messages" dividers were separators with text and a button inside (axe: nested-interactive)                       | 1.3.1      | Medium   | fixed   |
| W-13 | A channel switch focuses the composer, which doesn't name the channel                                                         | 2.4.6      | Medium   | #2283   |
| W-14 | Presence by colour only (away vs online 1.19:1) and hidden from screen readers                                                | 1.4.1, 1.4.11 | Medium | #2282  |
| W-15 | Quick switcher: unread, group names and "no results" not conveyed                                                             | 1.3.1, 4.1.3 | Medium | #2283   |
| W-16 | Device picker radios without arrow keys                                                                                       | 2.1.1      | Low      | fixed: roving tabindex, arrows, Home, End |
| W-17 | Every pending message announces "Sending"                                                                                     | 4.1.3      | Low      | #2283   |
| W-18 | Muted space header at `opacity: 0.6`: 3.58:1                                                                                  | 1.4.3      | Low      | #2284   |
| W-19 | 22 px icon buttons (toast close, chip remove), passing only by spacing                                                        | 2.5.8      | Low      | #2284   |
| W-20 | Canvas focus shown by a 1 px border colour change                                                                             | 2.4.7      | Low      | #2284   |
| W-21 | Six small transform transitions without a reduced-motion rule                                                                 | 2.3.3 (AAA) | Low     | #2284   |
| W-22 | Full reactor list only by mouse or long press                                                                                 | 2.1.1      | Low      | #2283   |
| W-23 | Custom status emoji has only a `title`                                                                                        | 1.1.1      | Low      | #2283   |
| W-24 | "Jump to latest" count: white on the dark theme's accent, 2.71:1 (axe)                                                         | 1.4.3      | High     | fixed: the accent's own text colour |
| W-26 | "Browse channels" link in the sidebar 21 px tall (axe)                                                                        | 2.5.8      | Low      | fixed   |
| W-27 | Send options chevron 22 px wide (axe)                                                                                         | 2.5.8      | Low      | #2284   |
| W-25 | Activity list items contain focusable elements; code blocks in excerpts scroll but take no focus (axe)                        | 4.1.2, 2.1.1 | Low    | #2283   |

### Findings: Android

| Id   | Finding                                                                                                     | Severity | State   |
|------|-------------------------------------------------------------------------------------------------------------|----------|---------|
| A-1  | The chat list's "New" button read only "Button" (Material clears the extended FAB's text semantics)          | High     | fixed   |
| A-2  | Call controls hid after 5 s with TalkBack on, and vanished from the semantics tree                          | High     | fixed: the recommended timeout (unlimited with TalkBack) |
| A-3  | The call stage was one merged node: participants couldn't be stepped through                               | High     | fixed: tap and accessibility action apart, initials hidden |
| A-4  | Switches and radios without a name (notification dialog, Do not disturb, meeting lobby, UnifiedPush, blur faces) | High | fixed: whole rows `toggleable`/`selectable` with a role |
| A-5  | The voice recording timer was a live region: read every second, into the recording                         | High     | fixed; the record button says "Recording" and its disabled state |
| A-6  | Record button: no role, disabled and recording only shown visually                                         | Medium   | fixed with A-5 |
| A-7  | A-33: poll results, media review strip, pickable tiles, mention suggestions, upload progress, targets, font scale, new-message announcements | Medium/Low | #2285, call parts #2281, presence #2282 |

### Findings: iOS

| Id   | Finding                                                                                                     | Severity | State   |
|------|-------------------------------------------------------------------------------------------------------------|----------|---------|
| I-H1 | Edit, delete, copy and reply in thread only in the long-press menu                                          | High     | fixed: in the actions rotor too |
| I-H2 | A message isn't one VoiceOver element: 3–6 stops, grouped messages without a sender                         | High     | #2286 (needs a device to get right) |
| I-H3 | Call controls hide after 5 s under VoiceOver and Switch Control                                             | High     | fixed   |
| I-H4 | No announcements at all: recording, reconnecting, lobby knocks                                              | High     | fixed for these three; the rest #2281 |
| I-M  | 17 Medium and 15 Low: draft chips, clipping at large sizes, links rotor, viewer escape, drag-only crop, contrast at reduced opacity, no Increase Contrast | Medium/Low | #2286, presence #2282, call #2281 |

## Keeping it from regressing

- Automated checks in CI (axe over the demo views, Android ATF, iOS accessibility audit, theme
  contrast including custom themes): #2287.
- Manual walkthrough with real screen readers per platform, and an accessibility statement with a
  contact for reports: #2288.
- New UI follows [forms.md](../.claude/rules/forms.md) and [design.md](../.claude/rules/design.md);
  reviews check: a name for every control, state in words not only colour, a keyboard route to every
  action (gesture and hover actions too), live regions mounted before their text and never on
  something that changes every second, reduced motion, 24 px (web) / 48 dp / 44 pt targets.
