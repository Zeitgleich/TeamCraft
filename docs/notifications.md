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

The DND schedule is typed (weekdays, start/end minute, a fixed UTC offset the client keeps current,
an allow list of senders) and stored as JSON text the database does not interpret. The server
evaluates it in the push routing, so it needs no timezone database.

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

For each recipient without a live connection: muted (level `none` or mute-until) never pushes; DND
(manual or scheduled, allowed senders excepted) suppresses all pushes; no push while another device
is active unless the push mode is `always`; mentions, DMs and replies in followed threads push, other
messages only in `all` conversations. Urgent messages don't override a mute or DND.

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
mutes and DND still apply and a suppressed attempt still counts. All three are in `[notifications]`.

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

