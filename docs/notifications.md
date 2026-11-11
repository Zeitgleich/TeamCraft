# Threads, unread counts and notifications (server state)

Server side of PLAN.md §3 and §5 (milestone M5). The server sees routing metadata only: which thread
a message belongs to (`thread_id`), who is mentioned, the urgency bit and read positions. Message
content stays encrypted; keyword notifications and the activity feed are built on the client.

## Threads

- A reply follows the thread for the sender (again after an unfollow, since replying is explicit),
  the author of the root message and mentioned members (unless they turned off
  `auto_follow_mentioned_threads`; somebody who never chose follows the workspace default, signed policy
  `threads.auto_follows_mentioned_threads`). Somebody who unfollowed by hand is not brought back by other
  people's replies.
- `PUT` / `DELETE /conversations/{id}/threads/{thread_id}/follow` follow and unfollow manually
  (following starts "from now": existing replies count as read).
- `PUT /conversations/{id}/threads/{thread_id}/read` moves the thread read position forward.
- The signed policy can switch threads off (`threads.is_threaded_view_enabled`): the server then refuses
  messages with a `thread_id` (403); existing threads stay readable.
- `GET /threads` is the threads inbox: followed threads, newest activity first, with reply, unread and
  mention counts and the newest reply (ciphertext) for the preview; `unread_only=true` filters.
- `thread_update` (gateway) tells the user's other devices that a follow or read position changed.

## Unread and mention counts

`GET /unread` lists, per conversation with something unread, the number of unread top-level messages
and how many mention the user, plus the number of followed threads with unread replies and the unread
thread mentions. Messages of the user themselves, thread replies (they count in the threads) and
messages from before the user joined are not counted. The read position is
`PUT /conversations/{id}/read-position`; the server keeps the larger of the stored and the new
position, so a late or repeated write never moves it back. Clients queue read positions,
acknowledgements, pin flags and the manual presence in their store while offline and send them once
back (issue #159), also after an app restart. Clients move the read position when the user opens a
conversation, sends a top-level message in it, and whenever something in the open conversation is
unread while its newest message is in view of the focused window (a message that arrives while the
user reads along, coming back to the window, scrolling down to the end), once per newest message.
"Newest" is the newest message the server stored, reactions, edits and deletions included: they
raise the server's count too, so a reaction to a message on screen is read at once (issue #641).
Moving the read position also sends the DM read receipt (subject to the read-receipt setting), so a
DM shows as read as soon as the recipient is at its bottom in a focused, visible window.

Clients keep the counts current from gateway events instead of polling `GET /unread` (issue #183):

- A new top-level `message_create` from someone else raises the conversation's unread count by one, and
  its mention count when `mentioned_user_ids` contains the user (counted like the server counts). The
  client stops at the cap `GET /unread` reports (`max_counted`, `limits.max_unread_counted`), and a
  message it already stored (a duplicate delivery) is not counted again. `GET /unread` also names an
  ID taken after the counts were read (`counted_through`): events of messages up to it queued while
  the answer was on its way and are already in the counts, so only higher IDs are counted.
- Moving the read position, on any device, sends `unread_update` with the conversation's new counts to
  the user's devices.
- `thread_update` makes the client read `GET /threads` and `GET /unread` again.
- After every gateway connection (resumed or new) the client reads the counts once, so a count that
  drifted is repaired on reconnect, not by a timer. After a resume it reads them only once the replay
  (`resumed.replayed` events) has been applied: `GET /unread` already counts the replayed messages,
  which would otherwise be counted twice.
- Presence announced by `presence_update` reaches the UI through a bounded log (the newest 5,000
  people by default, one record per person) with a revision per entry; the UI is sent only the
  entries newer than the revision it has (issue #259). After every connection the core reads the
  presence of everyone it knows in batches of 200 (`GET /presence`) and announces what changed, so
  the UI never reads presence itself.
- Read receipts arrive as `read_receipt` events. The UI reads `GET /conversations/{id}/read-receipts`
  only for the DM it shows, once per connection (issue #252), never one request per DM.
- Account data (saved items, drafts, backgrounds) is read once per connection and on `key_bag_update`;
  nothing is read on a timer (issue #252).

The client core keeps all of this in its store and hands it to the UI with its snapshot; the UI asks
only for what changed since its last read (`core_snapshot_changes`, issue #246), so a counter or
presence change sends the live state and nothing of the workspace. Only a UI without a core (no
gateway) still polls, every 5 minutes while the page is visible.

### Where the counts show

DMs and group DMs live outside spaces (their `space_id` is null), so no space counts them:

| Place                                | Counts                                                                                     |
|--------------------------------------|--------------------------------------------------------------------------------------------|
| Space rail entry of a space          | Unread messages of the space's channels, red, up to "99+"; muted ones count only mentions  |
| Chats (top of the rail)              | Unread messages of all DMs and group DMs plus mentions; muted ones count only mentions     |
| "Unreads" group of a space's sidebar | That space's channels only                                                                 |
| App badge (dock, taskbar, tray)      | Mentions everywhere plus unread DMs and group DMs (`badge_count`); on macOS also as a number next to the menu bar icon, up to "99+" |
| Mobile chat list tabs                | "Spaces": mentions (a dot for other unread messages); "Direct messages": as the rail entry |
| Other accounts (desktop, #214)       | Account menu and the account button: the other accounts' mentions, a dot for other unread  |

The desktop app's accounts that are not active run no core: the shell reads their `GET /unread` every
2 minutes with each account's own session. The server counts muted conversations as well (mutes are
in the encrypted account data), so these badges show mentions as a number and anything else as a dot;
they bring no notifications of their own yet.

**Chats** (desktop and web; the first rail entry, a phone's first tab) lists every DM and
group DM, newest message first (or by name), favorites on top, with avatar and presence, a bot marker,
filter chips (Unread, People, Groups), a text filter and "New message" for one person (a DM) or several
(a group DM, `POST /group-dms`). Its "…" menu holds the sort order and the number of conversations shown
before "show more"; conversations with unread messages always stay visible. Android and iOS show the
same list as a "Chats" tab (still labelled "Home" in the mobile shells until they follow) before "Spaces".

## Preferences

| What                              | Where                                                           |
|-----------------------------------|-----------------------------------------------------------------|
| Level (all / mentions / none)     | `GET`/`PUT /conversations/{id}/notification-settings`           |
| Mute until a time                 | same; also shown as `muted_until` in `my_membership`            |
| `@channel` notifies here          | same (`channel_mentions`); also in `my_membership`              |
| Quiet hours of a conversation     | same (`quiet_hours`); also in `my_membership`                   |
| Do-not-disturb schedule           | `GET`/`PUT /me/notification-settings` (`dnd_schedule`)          |
| Push mode (`away_only`, `always`) | same                                                            |
| Auto-follow mentioned threads     | same                                                            |
| Meeting reminders                 | same (`meeting_reminder_minutes`, `notifies_meeting_start`)     |
| Send read receipts                | same (default: `[notifications] read_receipts_default`)         |
| Send delivery receipts            | same (`send_delivery_receipts`; default as for read receipts)   |
| Level of everything else          | same (`level`; default: `[notifications] default_level`, `all`) |

### Default level

Which messages notify where the user never chose (issue #1011): the conversation's own level, else
the user's level for its space, else the user's own level (`level` in `/me/notification-settings`,
"Phone push notifications" in the settings), else the instance default `[notifications]
default_level` (admin console, `all` or `mentions`; built-in `all`). `GET` answers `level` only when
the user chose one, and `default_level` always; a `PUT` without `level` keeps the stored choice, so
clients that don't know the field change nothing. The desktop and web clients keep their desktop level
on the device (default `all` as well) and send the phone level only once it differs from the
instance default or the user chose before, so a user who never chose follows a later change of the
default. Integration channels notify only for mentions at any level, on the server and in the desktop
notifier.

Upgrading from before the default was `all`: memberships and spaces nobody configured follow the new
default, so their phones get channel messages too. A conversation set to "mentions" explicitly keeps
it. On the desktop and web, preferences stored with the old default read "mentions" for the desktop
and phone level; they can't tell the default from a deliberate choice, so "mentions" becomes "all"
once (`level_defaults_revision`); "nothing" stays. Users who want mentions only pick it again.
Choosing "nothing" for the phone silences its pushes and call rings (calls still ring in open apps).

### Muting a whole space

`GET`/`PUT /spaces/{id}/notification-settings` hold the member's level (optional) and mute time for a
space. A channel follows them (`my_membership.is_notification_inherited`) until the member sets a level
or mute time for the channel itself; `PUT .../notification-settings` with `"is_inherited": true` drops
that choice again. A channel with its own setting ignores the space entirely. The server resolves this
in the push routing and the e-mail digest, clients with `effective_notifications` in `teamcraft-proto`.
The desktop and web notifier keep the same two layers in their preferences (`spaces` next to
`conversations`) and send changes to these endpoints.

The DND schedule is typed (weekdays, start/end minute, the user's IANA time zone with a fixed UTC offset
as fallback, an allow list of senders, whether urgent messages break through) and stored as JSON text
the database does not interpret. The server evaluates it in the push routing with its time zone
database, so a daylight saving change needs no client.

### Per-conversation filters

Issue #599. Next to level and mute time a member sets two filters per conversation, in the
conversation's bell menu (desktop and web), its notification dialog (Android) or its "Notification
filters" sheet (iOS):

- **`@channel` and `@here`** (`channel_mentions`): `default` follows the general setting
  "@channel notifies me" (Settings → Notifications, synced in the account's encrypted data), `notify`
  lets `@channel`, `@here` and `@all` count as a mention here, `ignore` never. At the level
  "mentions" this is the "only announcements" filter: chatter stays quiet, `@channel` gets through
  (`notify`), or a channel whose members overuse it stops interrupting (`ignore`). A direct
  `@username` mention always counts.
- **Quiet hours** (`quiet_hours`): weekly ranges like the DND schedule's (weekdays, start and end
  minute, a range past midnight belongs to the day it starts on), in the member's IANA time zone with
  the UTC offset as fallback. While one is on, the conversation acts muted: no notification and no
  push, also not for mentions, urgent messages and calls (as a mute; DND overrides don't apply).
  Unread and mention counts are not affected, like a mute's.

Both are the conversation's own: they don't follow the space and `"is_inherited": true` (back to the
space's level) leaves them. A `PUT` without the fields keeps them, so clients from before them
change nothing; quiet hours without ranges remove them; a value from a newer version reads as
`default` (`ChannelMentions::Unknown`), and clients leave it out when they write. The server
validates ranges and zone like the DND schedule's (at most 14 ranges).

**What the server learns.** The server can't read message text, so the sending device marks a
message that addresses the whole conversation (`is_channel_mention`, from `addresses_everyone` in
`teamcraft-proto`, shared by every client). It counts only when the sender may use channel mentions
(`use_channel_mentions`); the server never stores it with the message, a held message keeps it until
it is released. The routing then treats the message as a mention for members whose
`channel_mentions` is `notify`. It can't see the general device setting, so `default` doesn't push
for `@channel` (as before the filter); members who want such pushes choose `notify`. Clients from
before the bit send none: their `@channel` notifies on the desktops of members who let it, not on
phones.

**Where it is decided.** The push routing (`push::routing`) checks quiet hours right after the mute
and evaluates them with its time zone database, so daylight saving time needs no client. The desktop
and web notifier (`notifier::decide`) checks them again with the device's current offset, like the
DND schedule, and applies `channel_mentions` to its own mention rules. Android and iOS show what the push
routing let through. Changes reach the user's other devices like level and mute: the server holds
them, desktop and web read them at start, Android and iOS with the dialog or sheet.

## E-mail digest of missed mentions

The only e-mail notification, **content-free** like everything the server can do here: it knows who is
mentioned in which message and who has read what, so the mail says how many mentions wait ("3 new
mentions") and links to the app, never where, from whom or what. Off by default.

- Server: `[notifications.email_digest]` (`is_enabled`, default and minimum interval, grace period,
  scheduler interval, texts) and `[mail.smtp]` with `[mail] public_base_url` (the mail goes through
  lettre like the activation and invitation mail).
- User: `GET`/`PUT /me/email-digest` (`is_enabled`, optional `interval_minutes` ≥ the instance minimum;
  without it the instance default). Turning it on starts "from now"; it needs an e-mail address on the
  account.
- The scheduler looks at each user once per interval and counts mentions since the last look that are
  at least `grace_secs` old and still unread (thread replies against the thread's read position),
  in conversations that aren't muted, and not written by the user. A mention that was read in time is
  never mailed; several mentions are one mail. A mail that can't be sent is tried again after five
  minutes without losing the count.
- Only the digest's count and the link leave the server; pushes for urgent messages and mentions are
  separate (below).

## Mention rules on the device

What counts as a mention beyond `@username` is decided on the device, on the decrypted text, because the
server cannot read it: `@channel` / `@here` / `@all`, the user's first name and their own keywords
(`teamcraft_client_api::notify`, shared by desktop and mobile; desktop: Settings → Notifications). The
mobile core keeps the preferences in the sealed local store (`teamcraft_core::notify_rules`, set with
`TeamCraft.set_mention_settings`), so the app and the iOS push extension, which opens the same store,
apply the same ones: `notification_for_push` sets `is_mention` when the decrypted text matches. The text
is only available while the signed push-content policy lets it show on the lock screen. The rules rank
and mark notifications (iOS: relevance score); they do not stop the server from pushing, and an extension
without the filtering entitlement cannot suppress a notification.

## Group mentions

`@accounting` of a user group the sender may mention in a conversation (`use_group_mentions`, admin.md
§3.2.4) notifies the group's members **who are in that conversation** (never anyone else; the sender's own
device expands the handle, e2ee.md §4.4, and the handle itself travels in the ciphertext). For the people
listed it counts as a mention everywhere a mention counts: push, badge, unread mention counts and the
activity feed, and the same mute, do-not-disturb and urgent rules. A message's list is what the sender
attached at sending: who is in the group later doesn't change who was mentioned.

- **Composer:** groups are suggested after `@` next to people, only where the permission holds. A mention
  of a group whose members aren't all in the conversation shows a hint that those members won't be notified.
- **Large groups:** the `@channel` confirmation setting ("Ask before using @channel ... with more than",
  `confirm_channel_over`) also asks before group mentions that notify more than that many people.
- **Limit:** a message names at most `[limits] max_mentions_per_message` people (default 200). Beyond it a
  device notifies the first ones and says so in the confirmation; the rest see the message without a
  mention. Older clients render `@accounting` as text; the people listed are still mentioned.
- **`@admins`:** a built-in group of the instance's active admins and owners, mentioned like any group
  (same permission, only those in the conversation, never the sender). It is no stored group: the
  server adds it to the rosters (`admin_ids`) while the admin console's notification setting
  `is_admins_mention_enabled` (default on) allows it, so it follows role changes without anyone keeping
  a list. Off, the rosters carry no `admin_ids` and devices neither suggest nor expand `@admins`; the
  handle stays reserved, so no stored group can take it.
- **Hourly budget for large mentions:** a message that names more people than
  `max_mentions_without_group_mentions` counts against its sender's hourly budget,
  `limits.max_large_mentions_per_user_per_hour` (admin console, default 30; `0` switches it off).
  Once it is used up the send is refused with `429 rate_limited` and a `Retry-After`; the message stays
  in the device's outbox as pending and goes out when the budget refills, so nothing is lost. Only a
  stored new message spends the budget: a retry the server already has (same idempotency key) is free.
  The budget is per server process and in memory (like the meeting mail budget): a restart refills it.
- **Phones:** Android and iOS suggest groups after `@` from the same core list as desktop and web
  (name, how many it notifies, and the hint about members outside the conversation), ask before large
  group mentions with the same `confirm_channel_over` setting, and render `@accounting` as a group
  mention, highlighted like a mention of the user when the message's list names them (`mentions_me`).
  Push needs nothing new: the server routes by the list as for any mention.
- **Identity provider:** user groups are managed in the app. The existing mirror of the provider's
  top-level groups (`[oidc.sync] sync_groups`, off by default, admin.md §3.2.3) stays as it is; nested
  groups and further sync options are not planned.

### Group mentions under real-world conditions

| Condition                   | What happens                                                                                                                                                                          |
|-----------------------------|---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| Offline sender              | The core expands from its stored copy of the rosters; the message waits in the outbox with its list.                                                                                  |
| Stale rosters               | The copy is refreshed on every connect and on `user_group_update`; `admin_ids` too, so a role change shows from the next connect or group change. A mention uses the copy at sending. |
| Server restart              | The hourly budget refills (in memory, bounded by the number of senders); rosters are read again on reconnect.                                                                         |
| Retry after a lost response | The idempotency key finds the stored message; the budget isn't spent twice.                                                                                                           |
| Over the budget             | `429` with `Retry-After`: the outbox backs off and resends; the message shows as pending meanwhile.                                                                                   |
| Large groups                | Confirmation above `confirm_channel_over`; at most `max_mentions_per_message` people per message; the roster read is one request per connect, not one per group.                      |
| Multi-device                | Each device expands from its own copy; receivers only use the list on the message, so devices never disagree about who was mentioned.                                                 |
| Older clients and servers   | An older client shows the handle as text and still counts the mention; a newer core against an older server finds no `admin_ids` and offers no `@admins`.                             |
| Guests                      | Read no rosters, so they never suggest or expand groups, `@admins` included.                                                                                                          |

## Push routing

For each recipient without a live connection the server decides from metadata it has anyway
(`push::routing`). A phone whose app reported going to the background counts as without one: it
keeps its gateway connection until it stops syncing, about half a minute later, but raises no
notification for what arrives over it. In order:

1. **Muted** (level `none`, or muted until a later time, own or inherited from the space): never pushes.
   The conversation's quiet hours count as a mute while they are on ("Per-conversation filters").
2. **Do not disturb** (manual, timed, or the weekly schedule): suppresses the push unless an override
   applies: the sender is on the user's allow list, or the message is urgent and the user lets urgent
   messages through. Overrides apply to manual and scheduled DND alike. It is the only status that
   holds notifications back: *busy* (by hand or during a call or meeting), *be right back* and *away*
   notify as usual (#1578).
3. **Attention**: no push while a phone of the user is in the foreground (it gets the event live). While
   the user is *at a desktop or web session* (attentive, or recently active on it; see below), pushes to
   phones wait for the grace delay and go out only if the message is still unread then. Push mode
   `always` skips both.
4. **Level** (see "Default level"): mentions, DMs and replies in followed threads push; other
   messages only in `all` conversations; integration channels only for mentions. An `@channel`
   counts as a mention where the member set `channel_mentions` to `notify`.

Calls ring every device of the callee (desktop, web and phones alike) unless the conversation is muted (quiet hours included) or
DND is on without an override.

## Mobile push delivery

The push design is in e2ee.md §7: the server seals a wake-up for the device, the app opens it and
decrypts the message itself, from the end-to-end encrypted ciphertext the wake-up carries when it fits
(e2ee.md §7.3), else fetched. This section is about *when* a push goes out and what the
apps do with it. Bad push ruins a chat product, so v1 ships only when every row of the target table holds
on real devices.

### Audit (2026-10-23, before `feat/push-quality`)

| Area                   | Finding                                                                                                                                                                   |
|------------------------|---------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| Desktop activity       | The UI reported `active` until 10 minutes without input, whether the window had focus or the screen was locked; no OS idle time; the server never expired an active report. |
| Grace delay            | Phones waited `mobile_push_delay_secs` (120 s) while a desktop was active, then pushed if unread. Correct idea, wrong input (see above).                                    |
| Read elsewhere         | Nothing withdrew a notification that was read on another device: not on Android, iOS, the desktop or the web service worker.                                              |
| Calls                  | The server never sent a call push (`PushKind::Call` was never created): phones rang only while the app was open. iOS never registered its PushKit token.                     |
| DND                    | Schedule with a fixed UTC offset (wrong after a DST change until a client corrected it), allow list only for the schedule, urgent never broke through, no mobile API or UI.  |
| Priority and TTL       | FCM `HIGH` only for urgent messages: ordinary DMs and mentions waited for Doze windows. One TTL for every kind (a ring could arrive hours late).                            |
| Retries and duplicates | A failed send was dropped; nothing kept a notification from being shown twice when several wake-ups for one message arrived.                                              |
| Android                | MessagingStyle per conversation with reply and mark-read, but no "calls" or "other" channel, no full-screen intent, no battery-optimisation guidance, distributor picked silently, no badge. |
| iOS                    | NSE with communication notifications and thread identifiers, but no badge, no handling of silent pushes, calls not deliverable in the background.                          |
| Diagnostics            | None: no test push, no "last push received", no delivery counts for admins.                                                                                              |

### Target behaviour

| Topic                | Target                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                     |
|----------------------|------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| Attention            | A desktop or web session is attentive only while its window is focused and visible, the user gave input within `attention_input_secs` (default 120 s), the OS idle time is below that, and the screen is not locked. It reports `attention` on change only ("away" once it lasted a minute); its gateway heartbeats renew the report, which the server forgets after `notifications.attention_lease_secs` (default 75 s), so a crashed or sleeping desktop stops holding pushes back. Users and admins can turn it off (then an active desktop holds pushes as presence does).                                                                                                                             |
| Grace delay          | While a desktop is attentive, phone pushes wait `notifications.mobile_push_delay_secs` (default 120 s) and go out only if the message is still unread. Reading on the desktop meanwhile drops them.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                        |
| Read elsewhere       | Reading a conversation (or a thread) on any device sends a `dismiss` wake-up to the user's other phones that were pushed for it: Android cancels the notification, iOS removes delivered notifications of that thread and updates the badge (silent background push). The web service worker closes its notification for the conversation.                                                                                                                                                                                                                                                                                                                                                                 |
| Calls                | A ring sends a `call` wake-up (high priority, TTL = ring timeout) to every device without a live connection; iOS as a mutable alert on the app's topic (the NSE hands it to CallKit once the filtering entitlement is granted, until then an "incoming call" alert), Android as a full-screen incoming-call notification on the "calls" channel (Android only takes the screen of a locked phone with it; with the app in front, the app opens its incoming-call screen itself). Answering on the system's call UI, a watch or in the car goes through Telecom, whose answer callback only starts the join: Telecom waits 5 s for it. A failed join says why on the call screen, or in a notification when the screen isn't up (#1272). The end of the ring sends `dismiss` with the call ID.                                                                                                                                                                                                                                                                                                     |
| DND                  | Weekly schedule with several ranges, evaluated in the user's IANA time zone (`time_zone`; fixed offset as fallback); quick DND (1 h, until tomorrow morning, custom) through presence `until`; per-conversation mute until; overrides for selected people and (configurable) urgent messages. Stored on the server, so every device and the push routing see the same; the apps check it again before showing anything.                                                                                                                                                                                                                                                                                    |
| Priority             | FCM `HIGH` and APNs priority 10 for everything a user should see (messages, mentions, calls, test); `dismiss` goes as a normal-priority data message (FCM) or a background push (APNs `content-available`).                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                |
| TTL per kind         | `[push.ttl]`: messages 1 day, calls the ring timeout, dismiss 1 day, test 5 minutes.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                       |
| Retries              | A send that failed for a temporary reason (5xx, 429, timeout, connection) goes into a durable queue in the database and is tried again with exponential backoff and jitter, at least as long as the push service asked (`Retry-After`), until its TTL ends (at most `push.max_retries` times). While a provider is failing, new wake-ups are queued before the first attempt. Workers claim queued wake-ups with `FOR UPDATE SKIP LOCKED` and hold the row lock while sending: a restart or a killed server loses none and no two workers send one twice. A message read meanwhile, a revoked device or a refused or gone registration ends the retries; a withdrawal drops the queued wake-ups it covers; deleting a channel, a space or a direct conversation drops its queued wake-ups in the same transaction (a retry is never claimed once its conversation is marked deleted). |
| Duplicates and order | The apps remember the message IDs they showed (per conversation) and ignore a second wake-up for the same message; MessagingStyle and iOS threads sort by the message time, not by arrival.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                |
| Badge                | The wake-up carries the user's unread count (conversations with unread mentions or DMs plus unread thread mentions); iOS sets it on the notification, Android on the notification (`setNumber`) and launcher.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                              |
| Android channels     | `direct_messages` (high), `mentions` (high), `messages` (default: channel messages), `incoming_calls` (high, full-screen intent, ringtone; importance can't be raised after creation, so the older `calls` stays the ongoing-call channel), `meetings` (high: reminders), `join_requests` (default: an admin asks to join a channel the user owns, posted by the app while it runs, admin.md §2.4.1), `other` (low: test pushes). Users change sound and importance in the system settings; the app links there.                                                                                                                                                                                           |
| Android extras       | MessagingStyle per conversation with long-lived conversation shortcuts and a group summary, inline reply, mark read and mute (8 hours, the server's per-conversation setting), battery state in the diagnostics (no exemption request; with UnifiedPush the distributor's), UnifiedPush distributor detection and choice. Senders show their profile picture (circle, fetched in the background through the core, cached; never from the push) or generated initials; bubbles follow in v1.0.5.                                                                                                                                                                                                            |
| iOS extras           | Mutable-content alerts through the NSE as communication notifications, thread identifier per conversation, relevance score, time-sensitive for urgent, badge.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                              |
| Diagnostics          | Mobile screen: test push (the server sends a sealed `test` wake-up end to end), provider, last push received, notification permission, battery optimisation, channel settings, DND state. Admins see per user and device how many pushes were sent, failed and suppressed (counts only).                                                                                                                                                                                                                                                                                                                                                                                                                   |

### Server implementation

`push::routing` decides, `push::delivery` sends (priority and TTL by kind), `push::retry` works off
the durable retry queue (table `push_retries`), `push::tracker`
remembers per process which devices show what (for withdrawals and rings) and the delivery counts.

| Setting                                           | Default     | Meaning                                                                         |
|---------------------------------------------------|-------------|---------------------------------------------------------------------------------|
| `notifications.mobile_push_delay_secs`            | 120         | Grace delay while a desktop is attentive; `0` sends nothing meanwhile           |
| `notifications.attention_lease_secs`              | 75          | How long an `attention` report counts without being repeated                    |
| `notifications.desktop_activity_secs`             | 120         | Activity on a desktop holds pushes like attention this long; `0`: off           |
| `notifications.is_desktop_attention_enabled`      | true        | Off: sessions report no attention; an active desktop holds pushes like presence |
| `notifications.max_held_pushes_per_user`          | 100         | Held pushes per user; beyond that the oldest goes out at once                   |
| `push.ttl_secs`                                   | 86400       | TTL of message and mention wake-ups                                             |
| `push.call_ttl_secs`                              | 0           | TTL of rings; `0` uses `[calls] ring_timeout_secs`                              |
| `push.dismiss_ttl_secs` / `push.test_ttl_secs`    | 86400 / 300 | TTL of withdrawals and test pushes                                              |
| `push.meeting_ttl_secs`                           | 600         | TTL of a meeting's reminder and start notice                                    |
| `push.max_retries`, `push.retry_initial_delay_ms` | 200, 2000   | Retries after temporary failures, doubling the wait (jitter), within the TTL    |
| `push.retry_max_delay_secs`                       | 600         | Longest wait between retries, unless the service asks for more (`Retry-After`)  |
| `push.retry_workers`                              | 2           | Retry workers per pool (APNs/FCM; Web Push/UnifiedPush), one DB connection each |
| `push.retry_max_queued_per_device`                | 200         | Wake-ups one device may have waiting; more are dropped                          |
| `push.retry_poll_interval_ms`                     | 1000        | How often idle retry workers look for due wake-ups                              |
| `push.test_interval_secs`                         | 10          | Shortest time between two test pushes to one device                             |
| `push.max_rings_per_minute`                       | 10          | Rings one caller may push per minute (further rings reach open apps only)       |
| `push.dismiss_coalesce_ms`                        | 3000        | Withdrawals for one conversation within this time go out as one                 |
| `push.sync_wake_coalesce_ms`                      | 30000       | New messages within this time make one sync wake-up                             |
| `push.sync_wake_min_interval_secs`                | 300         | At most one sync wake-up per user within this time; `0`: none                   |

Held pushes sit in one queue per user (at most `max_held_pushes_per_user`), worked off by one task per
user that the hub wakes only for that user. A held push goes out early when the desktop stops being
attentive (report `false`, connection closed).
A withdrawal goes only to devices that were pushed for the conversation (or thread) and whose newest
pushed message is now read; after a server restart nothing is withdrawn for earlier pushes. The tracker
is per process like the gateway hub: with several instances, a read on one instance withdraws only
what that instance pushed.

The device checks again (`teamcraft_core::push_guard`): a message shown before is not shown again
(urgent repeats are), a conversation muted or DND active on the device keeps the push silent, and the
diagnostics record when the last push arrived and how long it took.

### Sync wake-ups: the messages are there before the user looks

A phone gets a visible push only for what should alert (rules above), and none while the user is at a
desktop: held pushes are dropped once the desktop read them. That is right for the user's attention
and wrong for the phone's store: everything it wasn't pushed for, it fetches when the app opens, while
the user waits (docs/performance.md, "Coming back to the phone with the desktop open"). Signal and
Threema wake their apps for every message and fetch in the background; this does the same without
alerting.

- **When.** A message whose recipient gets no visible push now (muted, DND, mentions only, at the
  desktop, held for the grace delay) queues a `sync` wake-up for that user. New messages within
  `push.sync_wake_coalesce_ms` make one, and there is at most one per user within
  `push.sync_wake_min_interval_secs`; a message in that time waits for its end instead of being lost.
  `0` turns them off.
- **To whom.** The user's devices whose push service delivers silent pushes (APNs, FCM, UnifiedPush;
  never Web Push, where every push must show something) and that have no live connection then.
- **What.** A sealed wake-up of kind `sync` naming no conversation, message or sender. Silent: FCM
  normal priority (Doze delivers it in its next maintenance window, never wakes a dozing phone for it),
  APNs background push (iOS may defer or drop it), the TTL of a withdrawal. Through a push gateway only
  if its `/v1/info` lists the kind (push-gateway.md, "The contract").
- **On the device.** The app runs its sync loop until the connection is up and caught up, the
  backlog of a busy conversation included (#1448), at most a few minutes (Android: WorkManager, needs
  a network; iOS: the background push's time), then stops it unless the app is open
  (`TeamCraft::catch_up_in_background`). It shows nothing.
- **Cost.** At most 12 wake-ups an hour per user while messages arrive and the phone is idle, each a
  catch-up of what came since the last (a few conversations, round trips in parallel). The visible
  push for the same message is unchanged.

### Clients

**Attention (desktop and web).** `teamcraft_ui::attention` decides: window focused, page visible,
in-app input within the device setting `notifications.attention_input_secs` (default 120 s), OS idle
below it, screen not locked. Sessions report changes only, "away" once it lasted
`ATTENTION_AWAY_DELAY_SECS` (60 s), send `false` at once on page hide (best effort; the lease covers a
lost frame) and when the user turns the setting off, and repeat their last report after a reconnect.
The server renews an attentive report on every gateway heartbeat, keeps it in memory only and never
logs it.

| Platform | OS idle                              | Screen lock                                    |
|----------|--------------------------------------|------------------------------------------------|
| Linux    | GNOME idle monitor, else ScreenSaver | logind `LockedHint`, ScreenSaver `GetActive`   |
| macOS    | Core Graphics event source           | not read; locking takes focus from the window  |
| Windows  | `GetLastInputInfo`                   | not read; locking takes focus from the window  |
| Web      | none (in-app input only)             | none                                           |

**At the desktop (issue #756).** Attention alone missed users who sat at the computer: with the app's
window behind another one, or a webview that reported focus wrongly (#720), the phone was pushed at
once while the user chatted at the desktop, and when the user turned to the window a moment later the
next messages were held, so the phone showed only the first message of a burst. So the server also
counts the device's own activity (gateway.md, "Presence"): a desktop or web session whose device
renewed its activity after its last "away" report holds phone pushes like an attentive one for
`notifications.desktop_activity_secs` (120 s). The device renews it with every `active` report (input
in the window, and on the desktop input anywhere on the computer, repeated at most every 30 s), with
`typing`, and, with #732, with every message sent from it. An `idle` report (screen locked, "Away
after") ends it at once and releases the held pushes; an "away" report after the last activity (tab
hidden or closed) ends it too. Every message is routed on its own: nothing collapses or throttles
pushes per conversation, and the apps add each message to the conversation's notification.

**Urgent and DND.** With "urgent messages break through" on, an urgent message reaches the user during
DND only in DMs and group DMs by default (`urgent_scope: direct`); `everywhere` includes channels.

**DND editing.** The allow list and the urgent setting are stored even with the schedule off (a
schedule without windows), so they apply to manual DND. Quick DND (1 h, tomorrow 08:00, custom) is
presence `until`. Every client checks DND again at the device's own UTC offset.

**Android.** Every high-priority wake-up shows or updates something, or FCM lowers the app's
priority: a duplicate re-posts the conversation's notification silently, a ring the device suppresses
leaves a silent "missed call" note, a hidden change updates what shows or leaves a short quiet note, and
only a message the device's own mute or DND check stops posts nothing (counted in the diagnostics). A wake-up that takes longer than 8 s continues as expedited work.
The app never asks to be exempted from battery optimisation (FCM high priority is delivered in Doze);
with UnifiedPush it is the distributor that needs the exemption, and the diagnostics point there. A
thread `dismiss` removes that thread's lines up to the message; a conversation `dismiss` removes the
non-thread lines.

**Android message content (issue #314).** The push stays content-free; the app opens the sealed
wake-up with the core (`notification_for_push`) without starting the UI and shows the sender (a
`Person` with the same generated avatar as in the app), the conversation name for channels and group
DMs, and the text (cut at 240 characters, `@mentions` in bold) or a label for files (photo, video,
audio, file name). How the core gets there, within `PUSH_PREVIEW_BUDGET` (7 s; FCM gives about 10 s):

1. From the store if the app already has the message: no access token, no connection (issue #1662).
   Then from the message's ciphertext in the wake-up, if it carries one (a message of up to about
   1 KB, e2ee.md §7.3), checked like a fetched message: again no token and no connection
   (issue #1670). A message this device can't read yet (a key or a sender's device it doesn't know)
   goes on as if the wake-up carried none.
2. Otherwise, while the sync loop runs (the app in the foreground), nothing more: the sync brings the
   message, and the app fills the placeholder in from the store (`notification_from_store`). A push
   then holds the engine for milliseconds instead of a fetch on a connection that may be cold.
3. Otherwise it fetches and decrypts the one message, reading nothing else. The wait for the app's
   access token and the fetch share 4 s: a slow token refresh leaves less time for the fetch rather
   than adding to it. Without a token the store's preview stays and the app tries again later. The
   app's token provider says why there is none (issue #1671): a refresh that failed for now (offline,
   the identity provider down) is a network problem everywhere in the core and is retried; only an
   ended session (signed out, refresh token refused) is a sign-out, which the app handles itself.
4. If that fails or names no sender (a first DM from somebody new, keys that changed while the app was
   closed), the sync loop isn't running and the token wasn't missing, the app's own client catches that one conversation up
   (`Client::catch_up_for_push`: record and members, welcomes, MLS backlog, new messages) and reads the
   message from the store. Only the cancel-safe parts are cut at the deadline: following the groups
   (welcomes, MLS) starts only with 2 s left and then runs to completion, since a welcome spends the
   key package before the join is stored. The iOS notification extension runs next to the app and
   must not advance its state: it catches the conversation up on a probe instead, an overlay of the
   store in memory and a read-only API that are thrown away afterwards, within 14 s and 4 MB of
   writes (`notification_probe_for_push`, issue #839; e2ee.md §7.2). With less than 12 MB of memory
   to spare it stops after step 3 (`notification_preview_for_push`).

Whatever is known by then is shown: "New message" under the sender's name when the text could not be
read, never nothing. `PushNotification.maybe_gap` says why less was shown (no session, not decrypted,
timed out, not showable, hidden, workspace policy, device setting); the diagnostics show the last
reason and the app logs it, never the content.

**Not every wake-up is a message (issue #1249).** Reactions, edits, deletes, pins, bookmarks, system
notes, search index segments, edits of a channel's notes page and other changes to a conversation are encrypted messages like any
other, so the server can't tell them apart and wakes the device for them too (routing only knows the
recipient's settings). Once the device has read one, the core names it (`maybe_gap` hidden) and Android
announces nothing: it updates the conversation's notification silently, or takes away a placeholder
that stood for it. Being added to a conversation is the one change that is announced, to the person
added only, as "<name>: Added you to the conversation" (`maybe_gap` added-you; the text is the app's,
the event carries none), never "New message", as far as routing wakes them at all (a channel on the default "mentions" level
doesn't, as before; the channel shows on the next sync). Requests to join a channel don't go through
this path: they come over the gateway (`JoinRequestWatcher`), not as messages. Pictures and files without a caption are a text message with an empty body: they
show their label like a single file. A kind only a newer client knows may be a message, so it
keeps saying "New message" on the phone. Android only: iOS can't drop a notification without the
filtering entitlement and doesn't read the gap yet, and a browser must show something for every push.

| Android ends on                             | Says                     | Bridges, Auto | Later                                   |
|---------------------------------------------|--------------------------|---------------|-----------------------------------------|
| Text, or a file with or without a caption   | The message              | yes           | -                                       |
| Sender-only setting, app lock, policy       | "New message from Alice" | yes           | - (chosen)                              |
| Nothing setting or policy                   | "New message"            | yes           | - (chosen)                              |
| Read before it was looked at (`is_read`)    | Nothing, its line goes   | no            | -                                       |
| Reaction, edit, delete, other change        | Nothing new              | no            | -                                       |
| A kind this version doesn't know            | "New message"            | no            | -                                       |
| Not read in time: placeholder               | "New message"            | no            | Filled in (below)                       |
| Didn't open at all (store, no account open) | "New message", no names  | no            | Replaced by the real one, up to 4 tries |

`PushStats` counts these paths, and FCM messages delivered below the priority they were sent with
(`FCM_DEPRIORITIZED`), and the app logs and exports them with the diagnostics ("Notification
paths: SHOWN=12 HIDDEN=3"); each opened wake-up is also a line in the core's log (`push opened`: the
gap, how long it ran, whether the client was idle, and why a fetch failed by class), so a phone's
rates can be read off an export. Never content.

**Filling a placeholder in.** When the text could not be read in time (timed out, not decrypted, no
session), the app first waits for it before posting anything (`notification_for_push_retry`, not
counted as a received push; see "Watches, bands and Android Auto" for the bound and pauses). Only if
that wait ends without the text does a placeholder go out, and the app opens the same wake-up again
after 30 s, 2 min, 10 min and 1 h once a network is there. Only being signed out ends that chain: a
failed attempt, a timeout or a closed core lead to the next one. Those are ordinary WorkManager jobs,
so a dozing phone may run them only in its next window, which on a phone left alone is many minutes
away (seen: a placeholder still unfilled after 3 min of Doze, #1249); that is why the wait comes
before the post. A placeholder that went meanwhile costs no fetch. At most
32 chains wait at a time (managed configuration `max_push_retry_chains`), so a broken store can't pile
up jobs. A wake-up that didn't open, showed nothing (it may be a withdrawal) and finds the 32 taken
gets the generic "New message" once instead (one fixed notification, however many are refused), so
it isn't unknown until the sync (#1256). Independently, while a placeholder shows, every change of the store and every start of the
process look in the store (`notification_from_store`, no network): whatever the sync brought fills it
in, however late. The sealed wake-up stays with the placeholder's line for this, so it survives a
restart, and it goes with the line. A notification read or swiped away is not brought back, also not
by the expedited job of a receiver that comes after its run: that one opens the wake-up as a later
look (not counted again), and a repeat with no line left shows nothing. A placeholder that turned out
to be a reaction or an edit goes. Android drops updates of an app that posts more than about 5
notifications a second, without saying so (seen on the emulator, "Shedding" in the system log). Every
message notification carries a rising revision: 1.5 s after a conversation's last post the system's
copy is looked at, and what didn't get through is posted once more, alerting only if the post that
alerted is what got lost. One gap stays: an alert dropped right before a silent post that gets through
(a picture arriving) can't be told apart from one that went out, and isn't repeated.

**A late look checks first.** Each later look, not the tries before the post with their tight bound (a text read
must not be lost to the wait), asks the server how far the user has read, while the client is idle (the sync doesn't run in the background and a `dismiss` can be late), and `is_read` then
takes the line away instead of filling it in. Thread messages rely on the `dismiss`: a thread's read
position isn't in the conversation's record. A wake-up that opens only on a retry never rings: the
ring is over, so it leaves a missed-call note and never reaches Telecom; no clock is involved, since
the first retry is 30 s later and a ring lasts 30 s by default (`ring_timeout_secs`). With a longer
configured ring, one still going is told as missed. The expedited job reopening a wake-up compares the
server's send time with the device's clock over 45 s; a first delivery always rings, because the push
service's time to live is the ring's own and a wrong clock must not silence a real call. Since the
wake-up doesn't say how the call ended, and a `dismiss` comes for a ring that timed out as much as for
one answered, a late look at a ring asks the server's call history (`is_ring_answered`, issue #1257):
answered on another device, it leaves no note and stops a ring of it still going here. Without an
answer within the fetch budget, or with the call not among the newest 50, it stays a missed call.

**Android on watches (issue #739).** Wear OS and vendor bridges (Garmin Connect, Galaxy Wearable)
mirror the phone's notifications. A message is posted once, after the wake-up was opened, and every
later change (more messages, a late decryption, an avatar, a setting) replaces it under the same tag
and ID with `setOnlyAlertOnce`, so a watch alerts once per message. The notification carries
MessagingStyle (a `Person` per sender with name and avatar, conversation title, group conversation
for channels and chats with several senders) and also plain title and text for bridges that read
only those; it is not local-only once it has its text, has the conversation's shortcut and `LocusId`, and offers reply
(remote input), mark as read and mute. The group summary is posted only while two or more
conversations show (bridges mirror a summary as a second notification), never alerts
(`GROUP_ALERT_CHILDREN`), names the conversations ("Project A, Bob") or counts the messages when names
are hidden, and is posted again only when it changed. With "sender only" or the app lock, the watch
shows "New message from Alice"; with "nothing", "New message". Bridges that follow the lock screen's
privacy show the public version while the phone is locked and Android hides sensitive content: the
sender ("New message from Alice", "Project A: 2 new messages from Alice, Bob"), never the text.

**iOS message content (issue #331).** The notification service extension opens the wake-up with the
same core call and shows the same texts: sender and conversation, the text, a label for photos, videos
and audio ("📷 Photo", "🎬 Video", "🎤 Audio", otherwise "📎 file name"), and "New message from <name>"
when only the sender is known. The setting is Settings → Notifications → Notification content. Still
open: avatars as a communication notification, the extension's catch-up (design: e2ee.md §7.2) and the
last gap in the diagnostics.

**Notification content.** Each device chooses "sender and message" (default), "sender only" or
"nothing" (Settings → Notifications); the workspace policy `push_content` limits it, and the stricter
of the two applies (the core applies both when it opens the wake-up; the setting is kept in the store
so a notification extension reads it too). The app lock caps it at "sender only" and removes reply,
mark read and the conversation shortcut. Changing either re-posts what is in the shade. "Sender only"
reads "New message from Alice" or "3 new messages from Alice, Bob". The lock screen's public version
(shown when the device hides sensitive content on the lock screen, and by watch bridges that follow
it) names who wrote but never the text: "New message from Alice" under the sender's or conversation's
name, and the summary's names the conversations. With "nothing" it stays "New message".

**Opening from a notification (issue #1065).** The core keeps what the newest notification of each
conversation showed (`teamcraft_core::notified`: its name, sender and the message as far as it was
read) in the local store, from the app's push handling and from the iOS extension alike. The preview
never stores the message itself; only the sync does, in order. Opening that conversation reads the
record (`notified_conversation`) with no round trip: the header shows the name, the screen the
message and a loading line, until the store has the conversation and has caught up past the message
(the record then goes). A conversation the store doesn't know yet never shows "?": without a record
the header reads "Loading the conversation…". On iOS a tap opens such a conversation only when a
record exists. On the network the focused conversation's members come before its content (#1015).

**Watches, bands and Android Auto (Android).** Watch and band bridges (Wear OS, Garmin, Mi Band apps)
and Android Auto read what the phone posts. They must not get "New message" for a message that has
more (#739, #1093), and Android Auto must get the text with an alert to announce it (#1250: MessagingStyle
with a `Person`, reply with one `RemoteInput`, mark as read, both with their semantic actions and showing
no UI; `NotificationsOnDeviceTest` pins these). Bridges and Auto take a message's first post and
ignore what an update changes, so the text has to be in that post (#1249). The rule, in order:

| When                                     | Phone                  | Bridges and Android Auto              |
|------------------------------------------|------------------------|---------------------------------------|
| The text is read on the first try        | Alerts                 | Show it; Auto announces               |
| Not read: nothing is posted yet          | Nothing                | Nothing                               |
| Read while waiting (up to 2 min)         | Alerts once, with text | Show it; Auto announces               |
| Wait over without the text               | Alerts, "New message"  | Nothing (placeholder, local-only)     |
| Read after that                          | Silent update          | Show it as new; Auto may only list it |
| Read within 5 min, projecting to a car   | Alerts again           | Show it; Auto announces               |
| Never read                               | Stays "New message"    | Nothing                               |

While waiting, the app tries again after a short pause (0.5, 1, 2, 3, then every 4 s), each time
once the device has a validated network the app may use, up to 2 min: the usual reason a first try fails is a
radio still waking up after Doze or a network change, which fails at once rather than slowly (seen on
a phone: the second try came 16 ms after the first and failed the same way, #1249). Threema, which
also gets only a wake-up, likewise waits up to 2 min and posts nothing until it has the message.
The receiver's 8 s and then the expedited job (150 s) keep the process for the wait; no wakelock. A retry job is queued before the wait
and taken back after it, so a process that dies meanwhile still shows the message later. Holds of
earlier versions (a silent placeholder whose alert waited) still end as before:

| At the end of a hold                                    | What happens                |
|---------------------------------------------------------|-----------------------------|
| Read meanwhile                                          | The line goes               |
| Muted or DND meanwhile                                  | Silent; a text fills in too |
| Older than 5 min (Doze stretched the job, or a restart) | Silent                      |
| The user is in the app                                  | Silent                      |
| Otherwise                                               | The phone alerts            |

Times on the device are on `elapsedRealtime`, so a wrong clock doesn't matter; a phone that restarted
counts as too old. Past 5 min a text doesn't alert either, in a car neither: it is new to bridges and
silent on the phone. A message the sender sent silently never alerts, and a keyword mention found in
the text moves the notification to the mentions channel.

Trade-offs: a message reaches the phone up to 2 min later when the first try fails, in exchange for one post with the text everywhere; a message read only after the wait reaches a band as a new notification without the band
buzzing out of step, but off the road Android Auto isn't told; a message nobody can read (a device
that must re-link) is never announced in the car and shows only on the phone; an older line that stays
a placeholder under a newer message with text does travel with the conversation's MessagingStyle. A
filled-in notification is posted without `setOnlyAlertOnce`, because bridges skip updates that carry it.
Android Auto logs "No semantic reply action" for the group summary, which has no conversation; it
skips it and shows the conversations' notifications (checked with its head unit).

**FCM's rule for what shows nothing.** [FCM](https://firebase.google.com/docs/cloud-messaging/android-message-priority)
may deprioritise an app's high-priority messages "if FCM detects a pattern" of them not leading to
user-facing notifications, judged over 7 days per app instance; it publishes no threshold and doesn't
ask for one notification per message. The server can't send hidden changes with normal priority,
because it can't tell them apart, so a high-priority wake-up that was a hidden change, or was read
already, with no notification to update shows a silent, local-only note ("Checking for new messages…")
for 3 s on the low-importance channel, at most one a minute, so a burst shows one. UnifiedPush has no
such rule and shows nothing.

**iOS.** `call` arrives as a mutable alert through the notification service extension; without the
filtering entitlement it shows as a time-sensitive "incoming call" alert with answer and decline, with
it the extension hands it to CallKit (`NSE_NOTIFICATION_FILTERING` in `Base.xcconfig`, together with
`NSE_ENTITLEMENTS`). Without the entitlement iOS can't drop a notification, so muted, DND and duplicate
pushes are shown passively (no sound, no banner). While the app is in the foreground it holds the store
and opens pushes itself (`willPresent`). A `dismiss` without a thread removes only top-level
notifications; one naming a meeting removes that meeting's notices (thread `meeting-<id>`). The PushKit path is ready for when a VoIP token is registered.

### Desktop notifications

Desktop and web decide on the device what to show (`teamcraft_ui::notifier`, `call_alerts`,
`meeting_reminders`; wiring in `os_alerts`); the desktop shell (`apps/desktop/src/notifications`) or
the browser only displays, replaces, withdraws and reports clicks and buttons back as a
`NotificationActionEvent` (`teamcraft_client_api::os_notify`). Every notification has a tag: the
conversation ID for messages (as the service worker's push notifications), `call:`, `missed-call:`,
`meeting:` and `reminder:` prefixes otherwise. A newer notification with the same tag replaces the
older one; a conversation that becomes read anywhere, or is opened, withdraws its message and
missed-call notifications.

On Linux a message following one of the same conversation with the same title (same sender) is
added below the earlier ones (up to five lines), as messaging apps do, until the user dismisses the
notification. Except for a ringing call, a newer notification closes its predecessor and is shown
as a new one instead of replacing it: KDE Plasma doesn't report a popup that timed out into its
history, and replacing that one only updates the history entry, without a popup (#531).

| Platform | Shown through                          | Replace, withdraw | Buttons | Reply field                     | Ringtone, attention                       | Picture²                              |
|----------|----------------------------------------|-------------------|---------|---------------------------------|-------------------------------------------|---------------------------------------|
| Linux    | freedesktop notification service       | yes               | yes     | servers with `inline-reply`     | yes; quiet in GNOME/KDE do-not-disturb    | `image-data` hint (pixels, no file)   |
| Windows  | WinRT toasts (tag, group `teamcraft`)  | yes               | yes     | yes                             | yes; taskbar flash                        | `appLogoOverride`, circle for people  |
| macOS    | `UNUserNotificationCenter` (bundled)   | yes               | yes     | yes                             | yes; dock bounce                          | attachment                            |
| Web      | service worker registration, else page | yes               | yes     | no (browsers have none)         | yes, in the tab that runs the calls¹      | `icon` (`data:` URL)                  |

¹ Browsers play sound only in a tab the user has interacted with; a tab untouched since it loaded
shows the incoming call card and the notification without the ringtone.

² See "Pictures" below. The notification plugin (the fallback) shows none.

macOS buttons and the reply field need a notification category (registered as notifications arrive,
since the labels are translated) and a notification center delegate, an Objective-C class that
`teamcraft-desktop-native` defines (the one crate with `unsafe`, code-style.md "Native FFI"); the
delegate also lets banners show while the app is in front. An unbundled macOS build and a Linux session without a notification service fall back to the
notification plugin (title and text only, no withdrawal).

**Turning them on (#895).** The desktop app asks after sign-in as the web app does (#532): a banner
"Turn on" / "Not now" (a week's snooze), and Settings > Notifications > "This device" with the
system's real state (allowed, blocked in the system's settings with where to change them and the
app's name there, off) and a test notification. The shell reads the state (`notification_state`:
macOS notification settings, the Windows toast notifier's setting, whether a Linux notification
service answers) and the UI reads it again whenever the window comes to the front. "Turn on" does
the platform's step from the click: macOS asks for permission (`notification_allow`; no longer at
start-up), Windows and Linux, which have no prompt, show a test notification so the user sees how
and where they appear. The banner asks on macOS while the system has not asked, on Windows and
Linux until the test went out here; never where notifications are blocked or missing (an unbundled
macOS build, Linux without a notification service). Without notifications new messages show on the
badge and in the tray only, which Settings says. Logic and tests: `components/desktop_notify.rs`.

**Messages.** Mentions, DMs and followed thread replies by the rules above, the text only as far as
"show message preview" and the workspace's lock-screen policy (`push_content_mode`) allow. Buttons:
Mark as read; for top-level messages in conversations the user may post in also a reply field (a
thread reply's answer belongs in the thread, so it has none). A reply is sent like a composer message
(end-to-end encrypted, through the outbox) and marks the conversation read; `@channel` in a large
conversation opens the conversation with the text as draft instead, for the usual confirmation.
In the browser, a push notification shown by the service worker (no tab open) has Mark as read for a
top-level message: the worker sets the read position with the stored push session token, in the
language the page last left next to the database name; with an expired session the click opens the
conversation instead. Join on a meeting's push joins through an open tab; with no tab open the click opens the meeting's chat.

**Pictures (#543).** As in messaging apps, a notification shows who wrote or where: the sender's
avatar for DMs and group DMs (thread replies included), the channel's picture (else its space's) for
channels, the caller's avatar for ringing and missed calls, and the admin's avatar for a join
request; the settings' test notification shows the user's own. Meeting and `/remind` reminders have
none. The UI picks the picture (`teamcraft_ui::notifier::PictureSubject`, `notification_pictures`)
and names it by its key in the persistent media cache (e2ee.md §9.6), where the app keeps every
avatar and space or channel picture it showed; the shell (`apps/desktop/src/notifications/pictures.rs`)
or the page reads it from there and never fetches a picture for a notification. Without a cached copy
the notification shows the tile the app draws instead (initials on the person's or space's colour,
drawn by the UI into a canvas); a channel outside any space without a picture has none. Pictures are
cut to the app's shapes (people round, spaces and channels with rounded corners) at 128 px; the shell
keeps the last 32 prepared, so a burst of messages decodes a picture once.

- **Privacy:** a picture names a person or place as a name does, so it follows the names, not the
  text: the workspace policy `push_content` "generic" (or a mode this version doesn't know) removes
  it with the names, "sender" keeps it; "show message preview" hides only the text. Web Push follows
  the same rule after the device's own setting ("nothing" removes the picture). Desktop and web have
  no app lock; the Android app lock caps content at "sender only", which keeps names and so would
  keep pictures. On the lock screen the system's own preview setting applies (macOS "show previews:
  when unlocked" hides the attachment too).
- **Linux:** the pixels go along as the `image-data` hint (spec 1.2), which GNOME Shell and KDE
  Plasma show in place of the app icon; the app icon and `desktop-entry` still name the app.
- **Windows:** the toast's `appLogoOverride` with `hint-crop="circle"` for people, from a PNG file in
  the app's cache directory (one per picture, deleted when it is no longer kept, at sign-out and at
  start).
- **macOS:** a `UNNotificationAttachment`, which Notification Center shows beside the text; the
  system moves the file, so every notification writes its own copy. A communication notification
  (the avatar in place of the app icon) would need the Communication Notifications entitlement and a
  donated `INSendMessageIntent`; not built.
- **Web:** the page sets the notification's `icon` to a `data:` URL of the picture drawn in its shape
  (the page's CSP allows `data:` images). The service worker's Web Push notifications (no tab open)
  read the account's media cache with its wrapped key and use the cached picture as it is (PNG, JPEG
  or WebP up to 1 MiB, as the browser shows it: square, no initials tile), with the same order: the
  sender for DMs and calls, the channel's picture, then the space's.

**Message sounds (#800).** "Sound for new messages" (`message_sound`) is the system's notification
sound (the default, as before), none, one of the app's own sounds or the user's own file. The app's
sounds (Chime, Ping, Pop, Marimba, Knock, Keys and a playful "Uh-oh!") are synthesized like the
ringtone (`teamcraft_media::message_sounds`; the "uh-oh" is two sung vowels by formant synthesis), so
there are no sound files and nothing to license; no sound is copied from another product. In a
browser the synthesis is a chunk of its own that loads with the first sound. A sound
plays for the message notifications the rules above let through, so a muted conversation, the level
and do-not-disturb keep it quiet, as does the desktop's own do-not-disturb where the OS tells (GNOME,
KDE; macOS Focus and Windows' do-not-disturb aren't readable, as for the ringtone); not during a
call. A burst of messages plays one sound (at most one every 2 s). With the app's sound or none, the
notification itself is silent (macOS without a sound, Windows `<audio silent>`, freedesktop
`suppress-sound`, the browser's `silent`), except in a browser tab nobody has touched yet: it can't
play sound¹, so its notifications keep the system's. The settings play a sound when it is chosen and
with Play.

The own file is picked in the shell's file dialog (desktop) or the browser's file picker and refused
unless it decodes (desktop: Symphonia, for MP3, Ogg Vorbis, WAV and FLAC; browser:
`decodeAudioData`). Limits are settings (`own_sound_limits`): at most 1 MiB, and at most 5 s are
played (2, 5 or 10 s; longer sounds fade out at the cut). The file stays on that device: in the app's
data directory (`sounds/`) or in IndexedDB (`teamcraft-sounds`); it is never uploaded or synced, so
the server never sees it and other devices keep their own choice. Each sound is made or decoded once
per run and kept in memory, and nothing runs between messages (the desktop plays on a thread that
ends with the sound, the page closes its `AudioContext`). A missing or broken file never keeps a sound
from playing: the default sound (Chime) plays instead and the settings say why. The choice is a
device setting like the desktop level and the ringtone; one value for every kind of message for now,
and a field of the same type per kind (direct messages, mentions, channels) can follow. On the phones
the system settings choose the sound (Android channels).

**Calls (desktop).** A ringing call follows the rules of a direct message from the caller: a muted
conversation, the level "nothing" and do-not-disturb (except people on the allow list) keep it to the
in-app card. Otherwise the ringtone plays (setting `plays_ringtone`, default on; a synthesized
electric-piano arpeggio from `teamcraft_media::ringtone` on the default output; not during another
call; not while the desktop's own do-not-disturb is on, where the OS tells: GNOME, KDE). With the
window not in front (minimised, hidden to the tray, behind other windows), also when it leaves the
front while the call rings, the call shows outside it and the window asks for attention: as the
incoming-call window (setting `shows_call_window`, default on), a small borderless window above all
others in the corner where the system's notifications appear (top right; bottom right on Windows)
with the in-app card's caller, Decline and Accept; or, with the setting off or where the window can't
open, as a notification with Accept and Decline (Accept with video only where the in-app card offers
it; Linux: urgency critical, category `call.incoming`; Windows: toast scenario `incomingCall`; macOS:
time-sensitive). The window counts as open only once its page reported it on screen (within 3 s);
in a Wayland session it is never used, as the compositor alone places windows there and keeps an
unfocused new one behind the active window (KWin), so the notification shows (#962). Should no
notification show either, the main window comes up with the in-app card. Coming back to the window takes both away again (the in-app card is on screen).
Answering or declining from the window, the notification or the card is one path: the window's and
the notification's buttons reach the page as the same notification action. All of it stops the moment the call stops
ringing on this device: answered here or on another device (the user is in the roster), declined
here, answered by someone else (group calls), cancelled or timed out. A call nobody answered and the
user did not decline here leaves a "missed call" notification with Call back, under the direct-message
rules again. Names appear only where the lock-screen policy allows sender names.

**Meeting reminders.** Phones, and browsers without an open tab, get the server's push at the
scheduled time (meetings.md, "Starting"), which also carries the settings below to every device.
An open desktop or web app reminds by itself, as described here: for meetings the user is invited
to or organizes, planned (not "meet now"), not cancelled and not declined, one notification
`maybe_meeting_reminder_minutes` before (default 5, off possible) and one at the start
(`notifies_meeting_start`, default on), each with Join. Times are
the server's: `GET /meetings` carries `server_now`, and the client applies the measured offset, so a
wrong device clock does not shift reminders. One timer sleeps until the next reminder, at most 15
minutes (the list is refreshed then; a meeting created meanwhile is seen within that time); right
before a reminder the list is fetched again, so a cancelled or moved meeting does not remind. A device
that slept through the "before" time gets only the start reminder, up to 10 minutes after the start.
Shown reminders are remembered per device by meeting, start time and kind (local storage), so a
restart or a second tab never repeats one and a moved meeting reminds again for its new time; each of
the user's devices reminds once. Later occurrences of a repeating meeting are not reminded yet (the
list returns a series once).

**Meeting notices on several devices (#1473).** A reminder or start notice that was dealt with on
one phone goes away on the user's others. What counts as dealt with, per platform:

| Platform | Reports                                                                              | Removes what it shows on a `dismiss` naming the meeting                                                                                     |
|----------|--------------------------------------------------------------------------------------|---------------------------------------------------------------------------------------------------------------------------------------------|
| Android  | Join, and swiping the notice away (`setDeleteIntent`, no permission)                 | the notification with tag `meeting:<id>`                                                                                                    |
| iOS      | Join, and clearing the notice (`customDismissAction` on the category, no permission) | delivered notifications of thread `meeting-<id>`                                                                                            |
| Web      | Join and closing a push notice (`notificationclose`), with the stored push session   | nothing: no silent push in browsers, so no `dismiss` goes to Web Push (the notice stays until closed or replaced by the start notice's tag) |
| Desktop  | nothing: no push, its own reminders                                                  | nothing                                                                                                                                     |

The report is `POST /meetings/{id}/notices/dismiss` with the occurrence's start. It names only an
occurrence a notice went to the caller for (else 404), is scoped to the caller's account, is
idempotent (the first report of the notice's rows sends the withdrawal, repeats and parallel reports
answer 204 and send nothing) and falls under the general per-user write limit. The server then sends
a `dismiss` wake-up with the meeting and the occurrence's start to the user's phones that can take a
silent withdrawal (APNs, FCM, UnifiedPush), also the one that reported and phones with the app in
front (they show notices with the app open). It names no conversation (`conversation_id` is `0`) and
no text, so an app from before it removes nothing; a gateway sees an ordinary withdrawal. The same
withdrawal drops the notices of that occurrence still waiting in the user's retry queue.

**Several tabs.** In a browser only the tab that owns the account's store (e2ee.md §9.4) shows
notifications. Each tab records in local storage which conversation it shows while in front, so the
owner stays quiet about a conversation the user reads in another tab. Buttons of page notifications
reach the service worker, which hands them to an open tab as a `notification_action` message (Mark as
read without bringing the window up).

### Diagnostics API

- `POST /api/v1/devices/{id}/push/test`: sends a `test` wake-up to one of the caller's own devices and
  answers how the push service took it (`sent`, `not_registered`, `unavailable`, `rejected`, `gone`).
  At most one test per device every `push.test_interval_secs`.
- `GET /api/v1/admin/users/{id}/push-health`: per device the provider and counts since the server
  started (sent, failed, suppressed by DND, held for an attentive desktop, dismissals) with the time of
  the last success and failure. Counts only: no message, conversation or token.
- `GET /api/v1/me/push-health`: the same for the caller's own devices. Settings > Devices shows per
  device the push service and when the last push went out, and sends a test push to any of them
  (issue #409). The counts live in the server process, so after a restart a device shows no push until
  the next one.

### Android conversation bubbles (issue #591)

A direct message (one-to-one or group) can float as a bubble on Android 11+. It is a device setting,
off by default: "Show as bubble" in the conversation's notification settings (direct messages only;
it does not follow the account). A notification gets `BubbleMetadata` only when all hold: the user
opted this conversation in, the system allows bubbles (the app's bubble preference in the system
settings is "All" or "Selected conversations", `getBubblePreference()`; `areBubblesAllowed()` on
Android 11), the notification shows message content (the
"sender only" and "nothing" settings and the app lock suppress the bubble, as they suppress the
text), and it has its conversation shortcut. The expanded bubble is `BubbleActivity`, a separate
entry that shows the conversation screen; tapping the notification, shares and links still open the
main activity. With the app lock on, or in any state that needs sign-in, the bubble sends the user to
the main activity. A conversation deleted or left while its bubble is up closes it and clears the
notification. Turning the toggle off re-posts the notification silently without the bubble.

Bubbles are off for an app until the user allows them, so the toggle says so while the system
preference is "None" and offers "Open settings" (the app's bubble settings); the hint follows the
setting when the user comes back. With "Selected conversations" the system shows a bubble button on
the conversation's notification, and the user picks the conversation there. Channels are not
checked: `NotificationChannel.canBubble()` is false for any channel the user never changed (found on
a device: it kept bubbles off everywhere), and a direct message with a mention posts on the
mentions channel.

### What only real devices can show

Emulators and simulators do not reproduce Doze, App Standby buckets, OEM battery managers, PushKit's
"report a call or be killed" rule or NSE memory limits. Before a release, check on hardware: a DM to a
phone in Doze rings within seconds; a ring wakes a locked phone (Android full-screen, iOS CallKit);
reading on the desktop removes the phone's notification; DND schedule across a DST change; badge after
reading elsewhere; the test push on each provider (gateway FCM, gateway APNs, UnifiedPush).
The full checklist: [manual-tests/push.md](manual-tests/push.md).

## Read receipts

Moving the read position in a DM or group DM sends `read_receipt` to the other members and
`GET /conversations/{id}/read-receipts` lists the positions. Receipts are reciprocal: whoever turned
them off neither sends nor sees them. Channels have none.

### Delivered

After a device stored new messages of a DM or group DM (a live `message_create`, the end of a
conversation's catch-up), it sends the gateway frame `delivered` with the newest server message ID it
has, once per conversation and only when that moved forward; offline it waits, and the catch-up after
a reconnect reports again. The server keeps one position per membership, moved forward only, and for
each other member works out "every other member has the messages up to here on a device". When that
moves it sends that member `delivery_update` (`conversation_id`, `message_id`);
`GET /conversations/{id}/read-receipts` answers it as `delivered_up_to`, for devices that were offline.
It is an aggregate, never per member (e2ee.md §4.4), and reciprocal through `send_delivery_receipts`
like read receipts: whoever turned it off neither sends nor sees it, and while any other member has
it off the conversation shows no delivered state. No polling: the frame rides the open gateway
connection, and the server ignores repeats per connection. Servers from before the feature ignore the
frame, and clients from before it ignore the event and the field.

### Message status indicator

Own messages show one indicator next to the time (issue #568), drawn with the app's SVG icons:

| State         | Indicator      | Source                                                    |
|---------------|----------------|-----------------------------------------------------------|
| Sending       | clock          | message still in the local outbox                         |
| On the server | one grey tick  | the server confirmed the send                             |
| Delivered     | two grey ticks | a device of every other member has it (DMs, group DMs)    |
| Read          | two blue ticks | the other member's read receipt reaches the message (DMs) |
| Failed        | alert, retry   | the server refused the send                               |

One read model per conversation kind (issue #635), never two saying the same thing: a DM shows the
two blue ticks and no "Seen by"; a group DM shows "Seen by <names>" under my newest message (it says
who) and its messages show one tick, or two grey ones once delivered (never blue); channels show one
tick only. Because both come from read receipts, they are reciprocal: someone with receipts off
neither sends nor sees them. Delivered ticks (issue #598) follow "Delivered" above; read wins over
delivered, and a message stays at one tick while the delivered state is unknown or off.

The rules live in teamcraft-client-api (`receipts`), so every platform decides alike. The mobile apps
read the result from the core: `MessageExtras::is_read` marks an own message the other member of a
DM read (issue #769), `is_delivered` one that reached every other member's device (two ticks in the
muted colour); inside an own bubble, which already has the accent, the two ticks take the
bubble's full text colour where the single tick is muted. Receipts sent while a device was offline
never arrive as events: desktop and web read the open DM's receipts after each connection, the apps
ask the core for them (`refresh_read_receipts`) when a conversation opens and when the connection
comes back. The group DM's "Seen by" line is not in the apps yet.

## Urgent messages

A sender marks a message urgent (`is_urgent`). Recipients acknowledge with
`PUT /conversations/{id}/messages/{message_id}/acknowledgement` (the sender gets `message_ack`;
`GET .../acknowledgements` lists them). In DMs and group DMs, recipients who have not acknowledged
are pushed again every `urgent_repeat_interval_secs` until `urgent_max_pushes` pushes in total; the
scheduler runs every `urgent_scheduler_interval_secs`, claims due reminders with
`FOR UPDATE SKIP LOCKED` (safe with several instances) and sends them through the normal routing, so
mutes and DND (with its overrides) still apply and a suppressed attempt still counts. A repeat replaces
the earlier notification on the phone instead of stacking up. All three are in `[notifications]`.

## Activity feed

The feed (mentions, reactions to my messages, replies in my threads) is **not** a server feature: the
server cannot read reactions or message text. The client core builds it from its decrypted local
messages (`teamcraft-core`, `ui_activity`), newest first, and the UI shows it with an unread state and
a jump to the message.

- Mention: a message from somebody else whose `mentions` list contains me (a reply that mentions me is
  a mention, not also a thread reply).
- Reaction: the newest reaction message of a person, message and emoji, if it adds the reaction and
  the message is mine. Removed reactions leave nothing.
- Thread reply: a reply from somebody else in a thread I started or replied in.
- Read state: **one rule, no flag of its own.** An entry is read when the read position of its
  target is at or after it: the thread's position for a message in a followed thread (derived from
  the thread's unread count, newest replies first), else the conversation's (the member's
  `last_read_message_id`, or counts of zero from `unread_update`, which reach every device). A
  reaction counts by the reaction message itself. So reading the message anywhere, on any device,
  clears the entry, and positions only move forward.
- Opening an entry reads up to it: the conversation's position moves to the entry (not to the end);
  a thread entry opens the thread, which reads it. "Mark all as read" moves the positions of every
  unread entry's target the same way. The rail badge and tab counts are counts of these entries.
- Limits: a thread the user doesn't follow (or beyond the first page of `GET /threads`) has no
  position of its own, so its entries follow the conversation's.
- Join requests: an admin's request to join a private channel the user owns (admin.md §2.4.1)
  waits above the entries of "All" with Approve and Decline until it is decided, withdrawn or
  expired. It has no read state: "Mark all as read" leaves it, and the badge counts it while it
  waits. Each device shows one system notification per request (Approve where the verified roster
  lists the admin, and Decline), remembered across restarts and withdrawn once the request is gone.
  Approve in the notification shares only what is posted from then on and says so; sharing the
  earlier history is a choice in Activity or the channel settings.

## Reactions to my messages

The conversation view shows a round button with a heart and a count above "jump to latest" while
others' reactions to the user's own messages there wait unseen (issue #1009). A click goes to the
oldest such message and lights it up. A reaction is seen once its message is in view of the watched
feed (focused window, no call screen over it), or when the user jumps to it; the count goes down
and the button goes away at zero.

- What counts: one per person, emoji and message, for reactions of somebody else to a listed
  message of the user (top-level, or a reply also sent to the conversation). Reactions toggle: the
  newest reaction message of a person and emoji wins, so a reaction taken back before it was seen
  no longer counts, and adding it again counts again.
- The index: the core keeps the counted reactions per conversation in its store
  (`unseen_reactions.rs`), written as reaction messages are stored, by the gateway and the
  catch-up alike. Reading the count looks at the conversation's index entries, never its
  messages, so it works offline and costs nothing per message. A reaction that arrives before the
  message it reacts to (history pages arrive newest first) waits until that message arrives.
- The seen state is part of the member's read state, synced across their devices:
  `my_membership.reactions_seen` in the conversation, and
  `PUT /conversations/{id}/reactions-seen` (`message_ids`, at most 100 per request) to add to it.
  The server can't tell reactions from other messages (they are encrypted); it keeps message IDs:
  `through_message_id` (everything up to it is seen) and the IDs seen one by one after it, at most
  `limits.max_seen_reactions` (200); beyond it the oldest fold into `through`. The state only
  grows: nothing seen becomes unseen, and every named ID must be a message of the conversation.
  The user's other devices get `conversation_update` and read the conversation again. Marks set
  while offline wait in the queue of pending actions; this device counts them down at once.
- Reactions from before the feature that the user had read (up to the read position) count as
  seen. Moving the read position doesn't see reactions: a reaction to an old message far above
  arrives while the user reads at the bottom.
- Limits: reactions to thread replies only in their thread are not counted (the thread has no
  button). A device that hasn't loaded the history a reaction belongs to counts what it has.

## Call history

`GET /calls/history` lists the calls of the conversations the caller belongs to, newest first. The
client shows them as missed (rang the caller, never joined), incoming and outgoing, with a button to
call back.

