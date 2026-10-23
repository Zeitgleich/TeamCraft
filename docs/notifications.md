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
`PUT /conversations/{id}/read-position`.

## Preferences

| What                              | Where                                                   |
|-----------------------------------|---------------------------------------------------------|
| Level (all / mentions / none)     | `GET`/`PUT /conversations/{id}/notification-settings`   |
| Mute until a time                 | same; also shown as `muted_until` in `my_membership`    |
| Do-not-disturb schedule           | `GET`/`PUT /me/notification-settings` (`dnd_schedule`)  |
| Push mode (`away_only`, `always`) | same                                                    |
| Auto-follow mentioned threads     | same                                                    |
| Send read receipts                | same (default: `[notifications] read_receipts_default`) |

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

## Push routing

For each recipient without a live connection the server decides from metadata it has anyway
(`push::routing`). In order:

1. **Muted** (level `none`, or muted until a later time, own or inherited from the space): never pushes.
2. **Do not disturb** (manual, timed, or the weekly schedule): suppresses the push unless an override
   applies: the sender is on the user's allow list, or the message is urgent and the user lets urgent
   messages through. Overrides apply to manual and scheduled DND alike.
3. **Attention**: no push while a phone of the user is in the foreground (it gets the event live). While
   a desktop or web session of the user is *attentive* (see below), pushes to phones wait for the grace
   delay and go out only if the message is still unread then. Push mode `always` skips both.
4. **Level**: mentions, DMs and replies in followed threads push; other messages only in `all`
   conversations; integration channels only for mentions.

Calls ring every device of the callee (desktop, web and phones alike) unless the conversation is muted or
DND is on without an override.

## Mobile push delivery

The push design is in e2ee.md §7: the server seals a content-free wake-up for the device, the app opens
it, fetches the message and decrypts it itself. This section is about *when* a push goes out and what the
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

| Topic                  | Target                                                                                                                                                                                                                                         |
|------------------------|------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| Attention              | A desktop or web session is attentive only while its window is focused and visible, the user gave input within `attention_input_secs` (default 120 s), the OS idle time is below that, and the screen is not locked. It reports `attention` on change and every 30 s while attentive; the server forgets a report after `notifications.attention_lease_secs` (default 75 s), so a crashed or sleeping desktop stops holding pushes back. |
| Grace delay            | While a desktop is attentive, phone pushes wait `notifications.mobile_push_delay_secs` (default 120 s) and go out only if the message is still unread. Reading on the desktop meanwhile drops them.                                              |
| Read elsewhere         | Reading a conversation (or a thread) on any device sends a `dismiss` wake-up to the user's other phones that were pushed for it: Android cancels the notification, iOS removes delivered notifications of that thread and updates the badge (silent background push). The web service worker closes its notification for the conversation. |
| Calls                  | A ring sends a `call` wake-up (high priority, TTL = ring timeout) to every device without a live connection; iOS through PushKit (VoIP, only for calls), Android as a full-screen incoming-call notification on the "calls" channel. The end of the ring sends `dismiss` with the call ID. |
| DND                    | Weekly schedule with several ranges, evaluated in the user's IANA time zone (`time_zone`; fixed offset as fallback); quick DND (1 h, until tomorrow morning, custom) through presence `until`; per-conversation mute until; overrides for selected people and (configurable) urgent messages. Stored on the server, so every device and the push routing see the same; the apps check it again before showing anything. |
| Priority               | FCM `HIGH` and APNs priority 10 for everything a user should see (messages, mentions, calls, test); `dismiss` goes as a normal-priority data message (FCM) or a background push (APNs `content-available`).                                    |
| TTL per kind           | `[push.ttl]`: messages 1 day, calls the ring timeout, dismiss 1 day, test 5 minutes.                                                                                                                                                             |
| Retries                | A send that failed for a temporary reason (5xx, timeout, connection) is tried again up to `push.max_retries` times with backoff while its TTL lasts; a refused or gone registration is not.                                                       |
| Duplicates and order   | The apps remember the message IDs they showed (per conversation) and ignore a second wake-up for the same message; MessagingStyle and iOS threads sort by the message time, not by arrival.                                                     |
| Badge                  | The wake-up carries the user's unread count (conversations with unread mentions or DMs plus unread thread mentions); iOS sets it on the notification, Android on the notification (`setNumber`) and launcher.                                 |
| Android channels       | `messages` (default), `mentions` (high), `calls` (max, full-screen intent, ringtone), `other` (low: test pushes, sync). Users change sound and importance in the system settings; the app links there.                                         |
| Android extras         | MessagingStyle per conversation with conversation shortcuts and a group summary, inline reply and mark read, battery-optimisation guidance, UnifiedPush distributor detection and choice.                                                    |
| iOS extras             | Mutable-content alerts through the NSE as communication notifications, thread identifier per conversation, relevance score, time-sensitive for urgent, badge.                                                                                  |
| Diagnostics            | Mobile screen: test push (the server sends a sealed `test` wake-up end to end), provider, last push received, notification permission, battery optimisation, channel settings, DND state. Admins see per user and device how many pushes were sent, failed and suppressed (counts only). |

### Diagnostics API

- `POST /api/v1/devices/{id}/push/test`: sends a `test` wake-up to one of the caller's own devices and
  answers how the push service took it (`sent`, `not_registered`, `unavailable`, `rejected`, `gone`).
  At most one test per device every `push.test_interval_secs`.
- `GET /api/v1/admin/users/{id}/push-health`: per device the provider and counts since the server
  started (sent, failed, suppressed by DND, held for an attentive desktop, dismissals) with the time of
  the last success and failure. Counts only: no message, conversation or token.

### What only real devices can show

Emulators and simulators do not reproduce Doze, App Standby buckets, OEM battery managers, PushKit's
"report a call or be killed" rule or NSE memory limits. Before a release, check on hardware: a DM to a
phone in Doze rings within seconds; a ring wakes a locked phone (Android full-screen, iOS CallKit);
reading on the desktop removes the phone's notification; DND schedule across a DST change; badge after
reading elsewhere; the test push on each provider (gateway FCM, gateway APNs, UnifiedPush).

## Read receipts

Moving the read position in a DM or group DM sends `read_receipt` to the other members and
`GET /conversations/{id}/read-receipts` lists the positions. Receipts are reciprocal: whoever turned
them off neither sends nor sees them. Channels have none.

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
- Read mark: the sent time of the newest entry I looked at, kept in the account data of the key bag
  (`activity_read`, e2ee.md §6.4), so it follows me across devices; reading only moves it forward.

## Call history

`GET /calls/history` lists the calls of the conversations the caller belongs to, newest first. The
client shows them as missed (rang the caller, never joined), incoming and outgoing, with a button to
call back.

