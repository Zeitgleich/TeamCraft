# Gateway (realtime events)

`GET /api/v1/gateway` upgrades to a WebSocket. All frames are JSON text; the types live in
`teamcraft-proto` (`GatewayClientMessage`, `GatewayServerMessage`, `GatewayEvent`). Unknown frame and event
types are ignored on both sides, so new ones can be added without breaking old clients.

## Connection flow

1. Server sends `hello` with the heartbeat interval.
2. Client sends **`identify`** (`token` = OIDC access token, `device_id`, optional `activity`) for a new
   session, or **`resume`** (`token`, `session_id`, `last_seq`, `device_id`) to continue one. The token travels in
   this first frame and never in the URL. Wrong token, a device of somebody else or a missing role end the
   connection after an `error` frame.
3. Server answers `ready` (with the `session_id`) or `resumed` and then the missed events, or
   `session_invalid`: the client then identifies again and refetches state over REST.
4. The client sends `heartbeat` at the announced interval (answered by `heartbeat_ack`), also while the REST
   sync after `ready` and long commands run: it keeps reading the connection then and applies the events that
   arrived once the work is done, in their order. A connection without any frame for
   `heartbeat_timeout_secs` is closed. After `connection_max_age_secs` the server closes it so a
   fresh token is checked; clients reconnect with `resume`.

## Events and resume

Every `event` frame has a `seq` that grows by one per session. The server keeps the last
`resume_buffer_events` events of a session, at most `resume_buffer_bytes` of them, for `resume_window_secs`
after the connection dropped. A client that reconnects with its last seen `seq` receives exactly the missed
events; if they were dropped from the buffer (or the window passed) it gets `session_invalid`. A connection
that can't keep up is dropped the same way and resumes: as soon as its queue is full, or an event it has not
taken yet falls out of its buffer.

Each event is serialized once and shared by every session it goes to, so a big channel costs the event once,
not once per member; the byte limit bounds what one stalled session keeps alive. `max_buffered_bytes` bounds all
buffers of the process together (counted per session, so the real memory stays below it). When it is reached
the slowest sessions go first: those waiting to be resumed (longest waiting first), then connected ones that are
behind (most bytes behind first); they get `session_invalid` and sync afresh. If only healthy sessions are left,
the events they already took are dropped from their buffers.

`typing_start` is ephemeral: it comes with `seq` `0`, is not buffered and is never replayed (a late typing
indicator is wrong anyway). Clients keep the highest `seq` they saw, so `0` leaves their resume position alone.

When the server shuts down (SIGTERM) it closes every gateway and relay socket with close code **1012**
("service restart"); clients reconnect at once (with jitter) and resume or, after a restart, sync afresh.
A late `detach` of a connection the client already replaced by a resume never closes the resumed one.

Events go to the *users* that are members at publish time, so a removed member stops receiving a
conversation's events immediately. Event types: `message_create`, `conversation_update`, `membership_add`,
`membership_remove`, `mls_message`, `space_update`, `space_member_add`, `space_member_remove`, `typing_start`,
`presence_update`, `unread_update` (the user's counts of a conversation after their read position moved;
see notifications.md, "Unread and mention counts"), and more listed in `teamcraft-proto`'s
`GatewayEvent`. `conversation_update` and `space_update` carry only IDs: refetch the object over REST (it
differs per viewer).

## Client frames

| Frame          | Effect                                                                                       |
|----------------|----------------------------------------------------------------------------------------------|
| `typing`       | `typing_start` to the other members (members only, throttled to one per 3 s per conversation) |
| `set_activity` | the device is `active` or `idle`; feeds presence                                              |
| `heartbeat`    | keeps the connection alive                                                                    |

## Presence

Each connected device reports `active` / `idle`. The server aggregates per user: online if any device is
active, away if devices are connected but none is active, offline otherwise. The manual choice (`PUT
/api/v1/me/presence`) overrides: *do not disturb* (optionally until a time) shows `dnd` while a device is
connected; *away* shows `away` while a device is connected, whatever the devices report, until the user
picks another status; *invisible* shows offline; *auto* (the "Online" choice) follows the devices again.

The workspace's away timeout (`presence.away_timeout_minutes`, 15 for a new workspace, at least 5;
stored policies with less are applied as 5, absent lets the devices decide) marks a device that said
`active` but showed no sign of life for that long as timed out (away). Unlike a device's own `idle`,
which only the device ends, any sign of life ends a timeout at once: `set_activity active`, `typing`,
an attentive `attention` report or a heartbeat while attentive. A `set_activity` that makes an idle or
timed-out device active always goes through; other activity changes are throttled to one per second,
and a repeated report only counts for the timeout (no presence refresh). Desktop and web report
`active` on input while their window is focused and visible (at most every 30 s), when the window
comes to the front, after every reconnect or resume (a resumed session keeps what the server last
knew; a new one identifies with the last report), and when the user picks "Online". Their own "Away
after" (privacy settings) defaults to 15 minutes without input. The desktop app counts input anywhere
on the computer (the OS idle time, polled every 5 s, also while the window is in the background or in
the tray) as input and treats a locked screen as away at once, unless automatic away is off; only the
idle duration and the lock state leave the OS layer, no key or pointer data.

Changes are sent as `presence_update` to everybody who shares a conversation with the user (and to the
user's own devices). A disconnected device counts as gone only after `presence_offline_grace_secs`, so
reconnects don't flicker. `GET /api/v1/presence?user_ids=…` returns the last known state; guests only
learn about people they share a conversation with.

## Limits of M1

State is in memory per server process; running several server instances needs a shared bus (Postgres
`LISTEN/NOTIFY`). A do-not-disturb end time is evaluated when presence is recomputed, not by a timer.

## Device-linking relay (`/api/v1/rendezvous`)

A separate WebSocket for adding a device (e2ee.md §5). The new device and an existing device of the same
user both connect, send `join` with their access token and the same random `rendezvous_id` (32 hex
characters, chosen by the device that shows the QR code or prompt), and the server forwards **binary frames**
between them in order without interpreting them; the devices run their own authenticated key exchange on top.
Text frames are control frames (`joined`, `peer_joined`, `peer_left`, `expired`, `error`) and never relayed.
Limits (config `[rendezvous]`): lifetime, frame size, total bytes, frames held for a peer that has not joined,
open relays per user. A relay serves exactly two devices of one user, and ends when either side leaves.
