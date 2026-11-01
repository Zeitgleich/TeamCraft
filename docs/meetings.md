# Meetings

Server side of PLAN.md §9 and ARCHITECTURE.md §6.4: scheduled and instant meetings with their own
chat, CalDAV events, iMIP invitations, RSVP handling, and guests without an account who wait in a
lobby. Media (the call itself) is the existing call stack; a meeting's chat conversation is the
conversation the call belongs to.

## What is visible to the server

Title, time, description and the invitee list are **not** end-to-end encrypted: they live in the
calendar and in the invitation e-mails anyway. The meeting **chat and media are encrypted** like any
other conversation (e2ee.md). Guests get an ephemeral device and show up as "Guest (unverified)".
The server creates the chat without an encryption group; the organizer's app sets it up with the
users the organizer invited, and a host's "admit" lets an admitted person in (e2ee.md §4.6,
"Meeting chats"). A meeting from before that change gets its group the first time its organizer
opens or joins it; people invited later are added by the organizer's app when the organizer saves
the change, and people taken off the list are removed from the group.

## Model

| Thing              | Notes                                                                                            |
|--------------------|--------------------------------------------------------------------------------------------------|
| Meeting            | `scheduled` (planned, in the calendar, mailed) or `instant` ("Meet now", shared by link)         |
| Meeting chat       | A conversation of kind `meeting` (no space). Organizer = admin, invited users = members          |
| Invitee            | A TeamCraft user or an external person identified by e-mail; carries the answer (`rsvp`)         |
| Meeting link       | `<base>/meeting/<token>#v=<fingerprint>` for everybody; lets the holder knock (see "Links")       |
| Invite (guest)     | A personal one-time join link for somebody without an account (`meeting_invites`)                |
| Shared guest link  | One link per meeting for many guests (`meeting_invites.is_shared`); revocable, ends with it      |
| Lobby              | Guests and knocking members wait until a host (organizer or chat admin) admits them              |

## Flows

1. **Schedule** (`POST /meetings`): chat and invitees are stored, the iCalendar event with the join
   link is written to the configured CalDAV calendar (the meeting is rolled back if the calendar
   rejects it) and every invitee with an address gets an iMIP e-mail (REQUEST). The mail, the
   calendar event (`URL`, `LOCATION`, description) and "Copy link" carry the same meeting link. With "Notetaker joins the calls" (the first entry of the people to
   invite, when the signed policy has the notetaker on and lets the organizer ask it in, e2ee.md
   §8.2) the notetaker's bot account is invited like a member, so it is in the chat's group before
   any call, and the organizer's device puts a standing `join` request into the chat after setting
   it up; unticking it in the edit dialog sends a `stop` first. During a call, "Add notetaker" lets
   the organizer add it as an invitee then (one call's `join`).
2. **Change / cancel** (`PATCH` / `DELETE /meetings/{id}`, organizer only): SEQUENCE increases,
   the event is replaced or deleted, staying invitees get an update, new ones an invitation,
   removed ones and everybody on cancellation a CANCEL. The chat of a cancelled meeting is archived
   and stays readable. Cancelling always asks first; of a repeating meeting it asks what (see
   "Repeating meetings"). The app's edit dialog (title, time, length, description, invitees, lobby,
   the repeat rule; a series stays a series) sends the whole invitee list; once the server saved it,
   the organizer's device calls `update_meeting_chat_members` with that list and the users taken off
   it, so later invitees join the chat's encryption group and removed ones leave it (a signed
   commit that rotates the key). The list comes from the organizer's request, never from the
   server's member list.
3. **Answer**: invitees answer in the app (`PUT /meetings/{id}/rsvp`) or by e-mail. The e-mail path
   is `POST /api/internal/v1/meetings/replies` with the iMIP REPLY as `text/calendar`, called by the
   inbox handler that receives the mail (or by CalCraft) with the `inbox_token`. That caller vouches
   for the sender by passing the authenticated envelope sender in `X-Envelope-Sender`; the server does
   not authenticate mail itself, but accepts a reply only for exactly that address. A reply answers
   for one attendee (RFC 5546) and is short; anything else is refused.
   **Listing and reminders** (`GET /meetings`): a repeating series is stored once, with its first
   occurrence; the list expands it into one entry per occurrence in the range (same meeting ID,
   shifted start and end; without `to`, 90 days ahead). Occurrences repeat on the organizer's wall
   clock in the organizer's profile time zone (UTC without one), so "every Monday 09:00" stays at
   09:00 across a daylight-saving change; a monthly day that a month lacks takes the month's last
   day, and a wall-clock time the zone skips moves an hour on. The clients remind per occurrence.
   They read the list again when the gateway sends `meeting_update` (a meeting created, moved or
   cancelled elsewhere) and after each reconnect, not on a timer.
   **Starting** a meeting is joining its chat's call. When that call starts at most
   `start_ring_lead_secs` (default an hour) before an occurrence that isn't cancelled, or during
   it, it rings the invited users who haven't declined (gateway `call_ring` and a ring push, as
   for a call moved into a meeting), also when the organizer starts it early. Their ringing ends
   with the ring timeout, not when the first of them joins. The organizer's app labels the button
   of a planned meeting "Start"; "Meet now" is the separate meeting with nobody invited.
4. **Guests** open the meeting link `<base>/meeting/<token>#v=<fingerprint>` (see "Links"). The
   web client sends the token from the path to `POST /meetings/join` (no authentication) together
   with a display name and receives a guest session token valid until the meeting's end plus
   `guest_grace_secs`. The guest then exists as an ordinary guest user, is put into the lobby,
   and the hosts get a `lobby_join` event; once the guest's page set up its device it says so
   (`PUT .../lobby/me/device`) and the hosts get `lobby_join` again. `POST .../lobby/{user}/admit`
   makes the guest a member of the meeting chat (`membership_add` to the members,
   `lobby_decision` to the guest); `deny` turns them away and ends their session. A guest reads
   and speaks only once an organizer's device added theirs to the chat's encryption group (see
   "Guests in the browser"): on the host's "admit", or by itself in a meeting with
   `has_lobby: false` when the guest came with the meeting's token. Each guest of the link gets a
   one-time invite of their own (as for the shared guest link, step 5), so the link admits at most
   `max_shared_link_guests` guests. Personal one-time links (`#invite=<token>`, a host's "One-time
   link", and the links mailed to external invitees before #987) still work; their guests always
   wait for a host's click.
5. **Shared guest link** (`PUT /meetings/{id}/shared-guest-link`, hosts only): since #987 this is
   the meeting link itself. Every guest who uses it gets a one-time invite of their own
   (`shared_invite_id` names the link), so the rest is step 4. The link admits at most
   `max_shared_link_guests` guests (default 50) and ends with the meeting (its end plus
   `guest_grace_secs`, or its cancellation). **Making the link anew** (`POST /meetings/{id}/link`,
   organizer only; `DELETE .../shared-guest-link` does the same for older apps) gives the meeting a
   new token: the old link opens nothing any more (`404`), guests already waiting or admitted stay,
   and every invitee with an address gets an updated invitation with the new link. A guest's place
   is taken before their account is made and given back if the account or the redemption fails,
   so failed joins don't use up the link; a retried join after a lost answer does take a second
   place. Anyone with the link can knock, so a link that got around can use up all places until
   the organizer makes it anew (the app says so next to the link).
6. **Knocking**: a member who opens the meeting link without being in the meeting chat asks to be
   let in and waits in the lobby (`POST /meetings/link/{token}/knock` with the token, which answers
   the meeting and the lobby state; `POST /meetings/{id}/lobby/me` for an old link with the ID);
   the hosts get `lobby_join` (once per knock: knocking again while waiting is no news) and admit or
   deny as for guests. Members in the chat are answered `admitted` right away; a member admitted
   before who has left the chat waits again. A member with an old ID link always waits for a host,
   also without `has_lobby`: meeting IDs are not secret. With the token in a meeting without a
   lobby, the organizer's device admits the member by itself, as for guests. For meetings that are
   over or cancelled, outsiders get the same 404 as for a meeting or token that doesn't exist, so
   knocking doesn't tell them which IDs are meetings; members of the chat get 409. Denying a member
   only refuses them (the decision is final for that meeting); their account stays. The server
   sends `lobby_decision` to the knocking member too; the web client doesn't act on it yet and asks
   `GET .../lobby/me` every few seconds while it waits and the page is visible.
7. **A call becomes a meeting**: a DM never takes more people, so "Add people" in a DM or group DM
   call moves the call into a new meeting room instead. The adding user's app starts an instant
   meeting with the people in the call and the added ones as invitees (`POST /meetings/instant` with
   `invitees`; no calendar entry, no e-mail), founds the meeting chat's encryption group with exactly
   those users (e2ee.md §4.6, "Meeting chats"), and only then calls
   `POST /conversations/{dm}/call/move` with the meeting chat and the added people. The server checks
   that the caller is in the DM's call and organizes the meeting, refuses a second move of the same
   call into another room (409: two people adding at once don't split the call), tells the DM's
   members (`call_move`), and rings the added people in the meeting chat: when its call starts, or
   at once if it runs. Only one of them answering, or the ring timeout, ends that ringing; the people
   moving over don't. The adding app then announces the move in the DM's encryption group (an
   encrypted `call_moved` event with the room, the added people and the DM call's ID, not shown in
   the history).
   `call_move` alone moves nobody: a device in the DM's call shows "X moved the call into a meeting
   room and added Y" once the announcement is in, and on the user's tap checks it (announced by a
   member of the DM's group, the room's group holds nobody but the DM's members and the named
   people, and the announcement is for the call this device is in now) before it joins the room's
   call with its microphone and camera as they were. Only the device that added the people follows
   without the tap; the user's other devices ask too. A followed announcement is forgotten, so it
   never moves the device again, and the end of the call clears the announcements made for it (also
   those a failed follow left behind) and the device's note of the call's ID. The DM keeps its members and history, and the added people never
   see it. If the server refuses the move (409), the adding app cancels the room it just made.
   Web, desktop, iOS and Android all do this.

   | Condition                   | What happens                                                                                                                                                                                                                 |
   |-----------------------------|------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
   | The added person declines   | Their ring stops on that device; the others go on in the meeting room. A missed call is noted only if nobody answered at all.                                                                                                |
   | Network loss while adding   | Nothing changes before the meeting room's group is set up and the move is accepted: the DM call goes on, the dialog shows the failure and retries with the same idempotency key (the same meeting room, nobody added twice). |
   | Network loss while moving   | A device that misses `call_move` (resume window gone) stays in the DM's call, alone once the others moved; the meeting room is in its list and its call can be joined from there.                                            |
   | A participant's old client  | It ignores `call_move` and stays in the DM's call; it is a member of the meeting room (and in its group) and joins its call from there.                                                                                      |
   | A forged or redirected move | The server's `call_move` without a matching announcement shows nothing; a room holding somebody the announcement didn't name is refused.                                                                                     |
   | Two people add at once      | The second move into another room gets 409; that user's app says so and both follow the first room.                                                                                                                          |
   | Server restart mid-move     | Calls and rings live in memory: the ring of the added people is lost, they find the running call in the meeting room.                                                                                                        |

## Repeating meetings

A series repeats every N days, weeks or months (`RRULE` `FREQ`, `INTERVAL`), daily and weekly
series optionally only on chosen days of the week (`BYDAY`, the organizer's days: "Monday to
Friday"), and ends never, after a number of meetings (`COUNT`) or on a date (`UNTIL`, the end of
that day in the organizer's time zone; not both). The first meeting is the stored start, also on a
day the rule doesn't name (RFC 5545); the app asks for its day to be one of the chosen ones. The
calendar gets the rule next to a UTC `DTSTART`, so its `BYDAY` days move with the start when the
organizer's day and the UTC day differ (a meeting at 00:30 in Berlin).

Cancelling a meeting of a series asks what to cancel (`DELETE /meetings/{id}?scope=…&occurrence=…`,
`occurrence` the start as listed):

| Scope        | What happens                                                                                                                                     |
|--------------|--------------------------------------------------------------------------------------------------------------------------------------------------|
| `occurrence` | That meeting is listed as cancelled (`is_occurrence_cancelled`), the calendar event gets an `EXDATE`, invitees a CANCEL with its `RECURRENCE-ID` |
| `following`  | The series ends before it (`UNTIL`, `COUNT` dropped), invitees get the changed series; from the first meeting on it is the whole series          |
| `series`     | The default, and the only scope of a meeting that doesn't repeat: the whole meeting is cancelled as before                                       |

`POST /meetings/{id}/restore` takes a cancellation back: with `occurrence` that one meeting (the
`EXDATE` goes, invitees get the series again), without it the cancelled meeting (scheduled again,
calendar event and update mails, chat no longer archived) while it is not over. A cancelled series
is listed once, marked, at the meeting that was next when it was cancelled, then it is gone; the
organizer can restore it there or take it off everybody's lists at once (`POST
/meetings/{id}/hide`). At most 1000 meetings of a series are cancelled one by one; beyond that the
rule ends it. Ending the call of a series for everyone (host controls) ends that call, not the
series.

Public holidays: a single meeting on a holiday is cancelled like any other; a holiday calendar
that skips them by itself is a follow-up (v1.1).

| Condition                        | What happens                                                                                                          |
|----------------------------------|-----------------------------------------------------------------------------------------------------------------------|
| Two devices cancel the same date | The second gets 409 (cancelled already); the list converges on the next `meeting_update`                              |
| Calendar down while cancelling   | The cancellation stands (logged); the event follows on the next change of the meeting                                 |
| The series' start moves          | Cancelled dates are kept by their old start and no longer match; the moved series has all its meetings                |
| Server restart while ringing     | Rings live in memory: the ring is lost, the meeting's call goes on and shows in the list                              |
| Old clients                      | They see a cancelled occurrence as a cancelled meeting, ignore `weekdays` and `until`, and cancel only whole meetings |

## Links

What a host copies and sends, and what each link lets the holder do:

| Link                                         | Who it is for                       | What it does                                                    |
|----------------------------------------------|-------------------------------------|-----------------------------------------------------------------|
| `<base>/meeting/<token>#v=<fingerprint>`     | everybody: members and guests       | Opens the meeting for its members; others knock; a lobby decides |
| `<base>/meeting/<token>#invite=<jwt>&v=<fp>` | one guest without an account        | One-time; always waits for a host's click                       |
| `<base>/meeting/<id>` (and `#invite=…`)      | old links from before #987          | Still open the meeting, always through the lobby                |

The **token** is 32 base64url characters (192 random bits, e2ee.md §4.6, "Meeting guests"); the
server resolves it to the meeting, and it is all the server needs. The **fragment** `v` is the
fingerprint of the organizer's identity key: no secret, it lets a guest's page check that the
device admitting it is the organizer's. Browsers never send the fragment to a server.
`MeetingResponse.join_url` is the full link with the fingerprint of the key the server holds; the
app copies it with the fingerprint the copying device has pinned. The invitation e-mails and the
calendar event carry `join_url`. The organizer makes the link anew with "New link" (old links stop
working, updated invitations go out).

The web client shows the meetings screen for `/meeting/<token>` or `/meeting/<id>` when signed in
and then opens the meeting (pre-join screen) or knocks. Signed out, the same link shows the guest
page, with "Sign in" for members, who come back to the link after signing in. The app offers "Copy
link" on every meeting that is not over and in a dialog right after scheduling or "Meet now"; hosts
copy a one-time link with one click (the link is made and copied at once).

Rate limits: `POST /meetings/join` counts against the per-address limit for requests without
credentials (`security.rate_limits.public_requests_per_minute`) and failed redemptions and unknown
tokens (401, 404) against `auth_failures_per_minute`; knocking and making links are writes of the
member (`user_writes_per_minute`). The link's guest limit bounds what one leaked link can do.

Guest sessions are HS256 tokens signed with `guest_token_secret`; every API call and gateway
connection accepts them like an access token and treats the holder as a guest. Guests can't schedule
meetings and see nothing until they are admitted.

## Guests in the browser

A guest's page keeps going after "You are in" (#446; design in e2ee.md §4.6, "Meeting guests"):

1. After joining, the page enrolls an ephemeral device for the guest session (no account, no backup)
   and publishes its key packages while the guest waits in the lobby.
2. A host's "admit" makes the guest a member on the server and lets their device into the chat's
   encryption group from the join on. The guest's page then shows a slim meeting view: the meeting
   chat (history from the join on) and the call, nothing else of the app (no sidebar, spaces or
   settings). Everybody sees the guest as "Guest (unverified)".
3. **Token scope and expiry.** The session token is valid until the meeting's end plus
   `guest_grace_secs` and is accepted only on a short list of routes (conversation and messages of
   the guest's chats, the MLS and key routes of their own device, device registration, the lobby state
   of the meeting, `leave`, calls). Anything else answers 403. Guests only ever see conversations
   they are members of, which for a guest is the one meeting chat.
4. **Keys at admission.** The link carries no secret (e2ee.md §4.6, "Meeting guests"). The
   guest's page makes its own keys (the ephemeral device), and an organizer's device seals the
   meeting chat's keys to them when it adds the device: a new epoch, so nothing said before the
   admission is readable. The page checks that the device that added it is the organizer's
   (the fingerprint `v` from the link). Guest sessions are also ended by the server when the
   meeting's access ends (end plus `guest_grace_secs`) or it is cancelled, by a sweep at least
   every ten minutes. A guest sees the other attendees' names, never their e-mail addresses. A
   guest can't replace their identity key.
5. **Leaving.** `POST /meetings/{id}/leave` (guests only) ends the guest's session: their place in
   the lobby is closed, the account is deactivated (devices flagged removed, gateway connections
   closed, removed from the chat and the call) and the hosts' devices commit the removal from the
   encryption group. The page wipes its store. A guest who is turned away, or whose access ends, is
   treated the same.
6. **Limits.** Joining counts against the per-address limits (above); a guest's writes count
   against `user_writes_per_minute`; devices and key packages against `limits.max_devices_per_user`
   and the key package limits like any user's. The shared link's guest limit bounds how many guests
   one link lets in.

### What the guest's page does

`components/guest_join.rs` and `guest_meeting.rs`; `auth/guest.rs` is the guest's session as an
`Authenticator`, so the same client core and chat views run for a guest:

1. The page reads the token from the path and `v` from the fragment (`o` of older links). A
   link without a fingerprint (one mailed before #987) still joins, anchored on the fingerprint
   the server answers the join with, and waits for a host's click.
2. After joining, the page opens its client core with the guest token, enrolls the ephemeral device
   (`EnrollRequest.is_guest`), keeps the fingerprint for the meeting chat, and tells the hosts its
   device is ready. While nobody of the meeting is there yet it shows "Waiting for the organizer to
   let you in"; that is the normal case for a guest who comes early.
3. Once an organizer's device let the guest in (on "Let in", which the host's app offers when the
   guest's device is ready, or by itself without a lobby; `LetGuestIntoMeetingChat`), the page
   shows the slim meeting view: the meeting's name, the chat, "Join the call" and "Leave the
   meeting". The call is the browser call engine; the guest's call key binding is signed by the
   guest's device.
4. "Leave" calls `POST .../leave`, deletes the page's database (its keys and messages) and shows a
   goodbye. Closing the page without leaving keeps the encrypted data in the browser until the next
   guest page is opened here, which deletes it (or the guest clears site data); nothing reuses it.

A member's app copies a link with the organizer's fingerprint as its core has it pinned
(`PrepareGuestLink`; the copy buttons wait for it, and fall back to the server's `join_url`).

Not done yet: reloading the page loses the guest's device (the guest knocks again as a new guest);
only the organizer's devices let guests in (a co-host's "admit" waits for one of them; letting
other member devices admit needs an organizer-signed list of admitters, a follow-up); the size of
what a guest downloads is not measured yet (the guest entry path is the initial chunk; the
chat and call chunks load after admission).

## Gateway events

| Event            | To                          | When                                            |
|------------------|-----------------------------|-------------------------------------------------|
| `meeting_update` | organizer and invited users | created, changed, cancelled, answered           |
| `lobby_join`     | hosts                       | a guest knocked or set up its device, a member knocked |
| `lobby_decision` | the guest or member         | admitted or denied                              |
| `call_move`      | members of the DM           | its call moved into a meeting room              |

## Configuration

`[meetings]` in the server configuration (`teamcraft.example.toml` documents every key). Without it
the endpoints answer 503. `guest_token_secret` is required (at least 32 bytes). CalDAV, SMTP and the
inbox token are optional; without SMTP no e-mail is sent, without CalDAV nothing is stored in a
calendar. E-mail subjects and texts are templates in the configuration, so admins translate them
there.

All meetings go into one CalDAV collection written by one account: per-organizer calendars would
need per-user credentials, which the server does not hold. Users see their meetings in TeamCraft and
in the invitation e-mails.

## Development

`docker compose -f docker-compose.dev.yml up -d mailpit calcraft` starts Mailpit (SMTP on
`127.0.0.1:1025`, mails at <http://127.0.0.1:8025>) and CalCraft built from a pinned commit (CalDAV
on `127.0.0.1:5232`). `dev/init-calcraft.sh` creates the account the server writes with. Tests use a
fake calendar and mailer; one test sends through the real SMTP sender to Mailpit when it runs.

## Not done yet

- "In a meeting" presence from the calendar (PLAN.md §9), moving a single meeting of a series (only
  cancelling one), a holiday calendar, host controls (mute all, remove, lock, end for everyone) and
  meetings created in other CalDAV clients.
- Per-organizer calendars (see above).
- Meeting links: member clients reacting to `lobby_decision` instead of asking; making the link
  anew also turning away the guests still waiting from it; the desktop and mobile apps opening
  `/meeting/…` links themselves; hosts seeing how many guests the link let in.
