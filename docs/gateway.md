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

### The sync after `ready` (issue #714)

A fresh session costs what changed since the device was last caught up, not what the workspace holds:

- Right after `ready` the client sends its first reads at once: `GET /sync/heads`, the spaces and
  conversations, the roster and policy log, the first page of welcomes, threads and unread counts,
  notification settings, key package status, key bag, its own device list, presence of the people it
  knows and the open link requests. None needs another's answer; each step of the sync takes its answer
  instead of asking again. Reads wait for `ready`, so a change after a read always arrives as an event.
- `GET /sync/heads` names per conversation the newest message and MLS entry the caller can read and a
  digest of the member list, per space a digest of its member list. The digest is not computed from the
  members: database triggers give each list a new random token whenever its answer would change (a
  member added or removed, a role, expiry or sponsor, or a member's username, name, kind, organization,
  deactivation or deletion; migration 0075), and the digest covers that token and the directory policy
  that formats the names. So the answer costs one row per conversation, whatever the lists' size.
  The client keeps the heads it caught
  up to (one record in its store) and reads member lists, MLS queues and history only where a head moved,
  or where local work needs a read anyway (no group yet, a commit of its own not landed, messages waiting
  for keys, an earlier failure). The heads only let the client skip reads; nothing read is trusted
  because of them. A server without the endpoint (404) makes the client read everything, as before.
- A resume reads the once-per-connection data (counts and threads, presence, link requests, its own
  summary) at once, and when the connection was heard from within the last minute (a network blip, not
  an app started again) already while the server answers `resume`: the session kept every event in
  between, so nothing read then can be missed. With events to replay the counts are read only after the
  replay (`counted_through`).
- Every connection, fresh or resumed, ends its catch-up with the presence of the people it knows and,
  where the app handles calls, the calls running in the user's conversations (`GET /calls`, issue
  #976): call events lost with a forgotten session would otherwise leave a call on screen that ended.
  The list is read after the replay, so it can't undo a newer event.

The client logs where the time of each connection went (`gateway connection caught up` with the phases;
`Client::last_connection_phases`); docs/performance.md, "Coming back: requests and round trips", has the
numbers.

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
A `typing` frame may name the thread a reply is typed in (`thread_id`, optional); `typing_start` then carries
it, and clients show it in that thread only (#882). Without it (older clients) typing shows in the conversation.

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
| `typing`       | `typing_start` to the other members (members only, throttled to one per 3 s per conversation and thread) |
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
an attentive `attention` report or a heartbeat while attentive. Sending a message goes further: it
makes the sending device active at once, also when it said `idle` (a stale lock or idle reading can't
keep somebody away who is writing), until the device's next report or the timeout. A `set_activity`
that makes an idle or timed-out device active always goes through; other activity changes are
throttled to one per second, and a repeated report only counts for the timeout (no presence refresh).
Desktop and web report `active` on input in their window (at most every 30 s, whether or not the
webview counts the window as focused), when the window comes to the front, after every reconnect or
resume (a resumed session keeps what the server last knew; a new one identifies with the last report),
and when the user picks "Online". Their own "Away after" (privacy settings) defaults to 15 minutes
without input. The mobile apps report `active` when they come to the foreground and on touch (at most
every 30 s), `idle` when they go to the background, and repeat the last report after every reconnect or
resume, so a phone the server timed out while it was away comes back online when it is opened (issue
#472). The desktop app counts input anywhere on the computer (the OS idle time, polled every
5 s, also while the window is in the background or in the tray) as input and treats a locked screen as
away at once, unless automatic away is off; only the idle duration and the lock state leave the OS
layer, no key or pointer data. A lock reading that input in the window contradicts (a locked screen
passes none) is stale and ignored until the OS reports the screen unlocked.

The server recomputes a user's presence one refresh at a time, each from the devices' current state,
so overlapping refreshes (two devices, the periodic refresh, a disconnect) never announce an older
state last. Changes are sent as `presence_update` to everybody who shares a conversation with the user
(and to the user's own devices). A disconnected device counts as gone only after
`presence_offline_grace_secs`, so reconnects don't flicker. `GET /api/v1/presence?user_ids=…` returns
the last known state; guests only learn about people they share a conversation with.

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
