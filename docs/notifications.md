# Threads, unread counts and notifications (server state)

Server side of PLAN.md §3 and §5 (milestone M5). The server sees routing metadata only: which thread
a message belongs to (`thread_id`), who is mentioned, the urgency bit and read positions. Message
content stays encrypted; keyword notifications and the activity feed are built on the client.

## Threads

- A reply follows the thread for the sender (again after an unfollow, since replying is explicit),
  the author of the root message and mentioned members (unless they turned off
  `auto_follow_mentioned_threads`). Somebody who unfollowed by hand is not brought back by other
  people's replies.
- `PUT` / `DELETE /conversations/{id}/threads/{thread_id}/follow` follow and unfollow manually
  (following starts "from now": existing replies count as read).
- `PUT /conversations/{id}/threads/{thread_id}/read` moves the thread read position forward.
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
