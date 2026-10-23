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
| Mute from the list (needs the per-conversation notification setting)             | Android, iOS | owner: push-quality |
| Typing indicator ("Alice is typing…"), with a setting to not share one's own     | Android, iOS | done   |
| Archive / hide a conversation from the list                                      | Android, iOS | open: core |
| Multi-select (mark several read)                                                 | Android, iOS | open   |

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
| Swipe-to-reply (iOS has reply in the context menu now; a drag in a `ScrollView` needs a device to tune) | iOS | open |
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
| Emoji autocomplete (`:smile:`)                                                            | Android, iOS | open   |
| Voice messages                                                                            | Android, iOS | open: core |
| Image grid for several images in one message                                              | Android      | done   |
| Image grid for several images in one message                                              | iOS          | open   |
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
| Settings are one long scrolling page; group into sub-pages                       | Android      | open   |
| Dynamic color (Material You) as an option next to the TeamCraft theme             | Android      | open   |
| After a server restart clients never got the live gateway back (stale session resumed forever) | all | done (#92) |
| Font scale 200 %: chat list, conversation, composer checked, nothing cut off      | Android      | done   |
| No Android logs from the Rust core (`tracing` goes nowhere on Android)            | Android      | open: made the send bug hard to find |
