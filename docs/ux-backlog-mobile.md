# Mobile UX backlog

Friction found by using the Android app on an emulator against the dev stack (sign in, chat list,
reading and replying, threads, files and photos, sharing, search, settings), measured against what
people expect from WhatsApp, Slack and Teams on a phone. The iOS app was reviewed in code; it builds
only in CI, so every iOS item below is untested on a device.

Status: **done** (fixed), **open** (worth doing, not done yet), **owner: …** (belongs to another
work stream).

## Blockers found on the way

| Problem                                                                                              | Platform     | Status |
|------------------------------------------------------------------------------------------------------|--------------|--------|
| After the first app switch every send failed ("Something went wrong") and the text was lost: `Engine::start` dropped the running client when called again | Android, iOS | done (PR #70) |
| Sign-in against the dev stack always failed: AppAuth rejects ID tokens of a plain-HTTP issuer         | Android      | done (PR #70) |
| Downloads finished but the message kept its progress bar: transfers end without a change event       | Android      | done (polled like iOS) |
| 16 KB page size: all native libraries are 16 KB-aligned since the Gradle 9 / NDK 28 / JNA 5.19 upgrade (`llvm-readelf`) | Android | done |

## Getting in

| Friction                                                                              | Platform | Status |
|---------------------------------------------------------------------------------------|----------|--------|
| "Sign-in failed. Please try again." gives no reason (network, cancelled, wrong account) | Android  | done   |
| Notification permission asked on first start, before sign-in and without context      | Android  | owner: push-quality |
| "Create keys" runs for several seconds; the spinner sat under the keyboard              | Android  | done   |

## Chat list

| Friction                                                                         | Platform     | Status |
|----------------------------------------------------------------------------------|--------------|--------|
| No preview of the newest message, no sender, no time                             | Android, iOS | done   |
| Unread conversations only bold, no unread count (mentions only)                  | Android      | done   |
| No swipe actions (read/unread, favorite)                                         | Android, iOS | done   |
| No long-press menu on a conversation                                             | Android, iOS | done   |
| Direct messages sorted by name instead of recency                                | Android, iOS | done   |
| Empty list: one line of text, no way forward                                     | Android, iOS | done   |
| No offline indicator; nothing explains why new messages don't arrive             | Android      | done   |
| Offline banner                                                                   | iOS          | done   |
| Mute from the list (long-press → Notifications), shown right away (#99)            | Android      | done   |
| Typing indicator ("Alice is typing…"), with a setting to not share one's own     | Android, iOS | done   |
| Archive / hide a conversation from the list                                      | Android, iOS | open: core |
| Multi-select: long-press selects, then read/unread, favorite, notifications       | Android      | done   |
| Multi-select in the chat list                                                    | iOS          | open   |

## Conversation

| Friction                                                                                  | Platform     | Status |
|-------------------------------------------------------------------------------------------|--------------|--------|
| After sending, the list jumped back to the "new messages" line; own message off screen     | Android      | done   |
| Opening the keyboard hid the newest messages (list not anchored at the bottom)             | Android      | done   |
| New messages pulled the list down even while reading older ones                           | Android, iOS | done   |
| No "jump to newest" button with the count of newer messages                               | Android, iOS | done   |
| No date separators                                                                        | Android, iOS | done   |
| Name and time repeated on every message, divider lines; follow-ups not grouped             | Android, iOS | done   |
| Joins and renames shown as posts by "You"                                                 | Android      | done   |
| Links in messages not tappable                                                            | Android, iOS | done   |
| Link previews not tappable                                                                | Android      | done   |
| No swipe-to-reply                                                                         | Android      | done   |
| Swipe-to-reply (drag right; vertical drags keep scrolling)                          | iOS          | done   |
| No reply at all                                                                           | iOS          | done   |
| No double-tap quick reaction                                                              | Android, iOS | done   |
| Own reactions not highlighted                                                             | Android      | done   |
| Delete for everyone without confirmation                                                  | Android, iOS | done   |
| Failed send cleared the composer                                                          | iOS          | done   |
| Images open in an external app; no full-screen viewer with zoom and share                 | Android      | done   |
| Thumbnails decoded again on every recomposition (jank while scrolling)                    | Android      | done   |
| "📎 name" label repeated above the file it describes                                      | Android      | done   |
| No camera or photo picker, only the generic file picker                                   | Android      | done   |
| Send options (priority, silent, …) as a ⋮ button right next to Send: easy to mis-tap       | Android      | done   |
| Reply did not focus the composer                                                          | Android, iOS | done   |
| Composer started sentences in lower case                                                  | Android      | done   |
| "Send with Enter" for hardware keyboards and tablets                                      | Android      | done   |
| Quick reactions in the long-press sheet wrap onto two lines on narrow phones              | Android      | done   |
| Edit history, forward, pin, save, copy link, mark unread; reply quotes shown            | iOS          | done   |
| Images as large previews; a tap downloads and then opens Quick Look (zoom, share, save)    | iOS          | done   |
| Mention highlighting in message text                                                      | Android      | done   |
| Mention highlighting in message text                                                      | iOS          | done   |
| Emoji autocomplete (`:tad` → 🎉) and `:)`/`:tada:` on send, catalog shared with desktop | Android, iOS | done   |
| Voice messages                                                                            | Android, iOS | open: core |
| Image grid for several images in one message                                              | Android      | done   |
| Image grid for several images in one message                                              | iOS          | done   |
| Save an image to the gallery (Pictures/TeamCraft, Android 10+)                            | Android      | done   |
| Picked attachments were lost when looking at another conversation before sending          | Android      | done   |
| A message the server rejected showed "Sending…" forever; now "Not sent" with Retry/Delete | Android, iOS | done   |

## Sharing and navigation

| Friction                                                                           | Platform     | Status |
|------------------------------------------------------------------------------------|--------------|--------|
| Can't share text, links, photos or files from other apps into a conversation       | Android      | done   |
| Share extension                                                                    | iOS          | open: needs an app extension target |
| App shortcuts for recent conversations                                             | Android      | owner: push-quality (MessagingStyle conversation shortcuts) |
| Message links in messages open the conversation at the message (in the app)        | Android      | done   |
| Message links in messages open the conversation or thread at the message           | iOS          | done   |
| Message links from other apps (`teamcraft:` deep links; decide with the launch-token rule) | Android, iOS | open |
| Tablet / foldable / landscape: list next to the open conversation                  | Android      | done   |
| Open conversation after process death (checked: the navigation state restores it) | Android      | done   |

## Security and settings

| Friction                                                                         | Platform     | Status |
|----------------------------------------------------------------------------------|--------------|--------|
| No app lock (biometrics or device screen lock, delay configurable)               | Android, iOS | done   |
| App switcher shows chats                                                         | iOS          | done (option) |
| Settings were one long page: now a hub with Notifications, Chats, Privacy, Devices; whole rows toggle | Android | done |
| Dynamic color (Material You) as an option next to the TeamCraft theme             | Android      | done   |
| After a server restart clients never got the live gateway back (stale session resumed forever) | all | done (#92) |
| Font scale 200 %: chat list, conversation, composer checked, nothing cut off      | Android      | done   |
| No Android logs from the Rust core (`tracing` goes nowhere on Android)            | Android      | open: made the send bug hard to find |

## First run (2026-10-23)

Install → sign-in → passphrase or linking → notifications → first chat, walked on the emulator
against the dev stack with a test brand.

| Friction                                                                                                                                   | Platform     | Status                               |
|--------------------------------------------------------------------------------------------------------------------------------------------|--------------|--------------------------------------|
| Notification permission asked at the very first start, before sign-in, without a reason                                                    | Android      | done                                 |
| Notification permission asked right after setup with the bare system prompt                                                                | iOS          | done                                 |
| Sign-in showed no brand and not which server; now logo, title, text, links and host                                                        | Android, iOS | done                                 |
| Passphrase fields: no show/hide; keyboard Done did nothing                                                                                 | Android, iOS | done                                 |
| Second device: linking (quickest, history comes along) was the third option; now first                                                     | Android, iOS | done                                 |
| Link screen said only "enter this code"; now steps for a phone (scan) and a computer                                                       | Android, iOS | done                                 |
| Empty chat list: a chip only; now Browse channels / Message someone and a New chat button                                                  | Android, iOS | done                                 |
| Tapping a person in Browse created the DM but didn't open it                                                                               | Android      | done                                 |
| No Browse at all (channels, people, join, new channel): a new user couldn't start a chat                                                   | iOS          | done                                 |
| Browse lists were blank with no explanation when empty                                                                                     | Android, iOS | done                                 |
| New member finds nobody to message: people come only from shared conversations; the server's `GET /users` search is unused by every client | all          | open: core                           |
| Linking a phone from a computer means typing a 105-character code (desktop can't scan)                                                     | desktop      | open: webcam scan or approval prompt |
| Approving a new device on iOS: code typed by hand, no QR scanner                                                                           | iOS          | open                                 |
| Server URL is fixed per build: no server-URL screen (needs a public OIDC discovery endpoint)                                               | Android, iOS | open: server + design                |
| SVG brand logos don't show on mobile (name shown instead)                                                                                  | Android, iOS | open                                 |

## Accessibility (2026-10-23)

Checked with an audit script over uiautomator dumps (labels, 48 dp targets), WCAG contrast ratios
of both palettes, and font scale 200 % on the emulator.

| Friction                                                                                                        | Platform     | Status |
|-----------------------------------------------------------------------------------------------------------------|--------------|--------|
| Unread badge: white on #E4572E was 3.7:1; now #C8441F, 4.9:1                                                    | Android, iOS | done   |
| Field and control borders 1.3:1; now 3:1 (`outline`, `Theme.controlBorder`), dividers stay light                | Android, iOS | done   |
| Selected chips in Material's default purple; sheets and menus lavender-tinted; now the accent and neutral grays | Android      | done   |
| Screen titles, chat list sections and settings headings weren't headings for TalkBack                           | Android      | done   |
| Chat list section headers in capitals were read as written in capitals                                          | Android      | done   |
| Quick reactions in the long-press sheet read only the emoji; now "React with 👍"                                 | Android      | done   |
| Reaction chips: own reaction now announced as selected                                                          | iOS          | done   |
| Jump-to-newest button 40 pt; now 44 pt                                                                          | iOS          | done   |
| Link and call short codes in fixed font sizes; now follow Dynamic Type                                          | iOS          | done   |
| Font scale 200 %: settings hub and long-press sheet checked, nothing cut off                                    | Android      | done   |
| Audit: every clickable node on chat list, conversation, settings and sheet has a label and is ≥ 48 dp           | Android      | done   |
| VoiceOver and Dynamic Type not checked on a device (CI compile only)                                            | iOS          | open   |
| Desktop uses the same badge color (3.7:1)                                                                       | desktop      | open   |
