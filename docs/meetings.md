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
the change, and people taken off the list are removed from the group. The organizer signs the
invitee list, so a host's Join adds only people the organizer invited; somebody the server lists
besides knocks at the lobby (#1726).

## Model

| Thing              | Notes                                                                                            |
|--------------------|--------------------------------------------------------------------------------------------------|
| Meeting            | `scheduled` (planned, in the calendar, mailed) or `instant` ("Meet now", shared by link)         |
| Meeting chat       | A conversation of kind `meeting` (no space). Organizer = admin, invited users = members          |
| Space of a meeting | The space it was scheduled or started in, if the organizer is a member of it (`space_id`, #954)  |
| Invitee            | A TeamCraft user or an external person identified by e-mail; carries the answer (`rsvp`)         |
| Meeting link       | `<base>/meeting/<token>#v=<fingerprint>` for everybody; lets the holder knock (see "Links")       |
| Invite (guest)     | A personal one-time join link for somebody without an account (`meeting_invites`)                |
| Shared guest link  | One link per meeting for many guests (`meeting_invites.is_shared`); revocable, ends with it      |
| Lobby              | Guests and knocking members wait until a host (organizer, co-host or chat admin) admits them     |
| Host               | The organizer, the co-hosts the organizer names (#1720) and the chat's admins (see "Hosts")      |

## Flows

1. **Schedule** (`POST /meetings`): chat and invitees are stored, the iCalendar event with the join
   link is written to the configured CalDAV calendar (the meeting is rolled back if the calendar
   rejects it) and every invitee with an address gets an iMIP e-mail (REQUEST). The mail, the
   calendar event (`URL`, `LOCATION`, description) and "Copy link" carry the same meeting link. With "Notetaker joins the calls" (the first entry of the people to
   invite, when the signed policy has the notetaker on and lets the organizer ask it in, e2ee.md
   §8.2) the notetaker's bot account is invited like a member, so it is in the chat's group before
   any call, and the organizer's device puts a standing `join` request into the chat after setting
   it up; unticking it in the edit dialog sends a `stop` first. During a call, "Add notetaker" lets
   the organizer add it as an invitee then (one call's `join`); an invitee's "Add notetaker" moves
   the call into a room of theirs with the notetaker (step 7, e2ee.md §8.2, "In every call").
2. **Change / cancel** (`PATCH` / `DELETE /meetings/{id}`, organizer only): SEQUENCE increases,
   the event is replaced or deleted, staying invitees get an update, new ones an invitation,
   removed ones and everybody on cancellation a CANCEL. The chat of a cancelled meeting is archived
   and stays readable. Cancelling always asks first; of a repeating meeting it asks what (see
   "Repeating meetings"). The app's edit dialog (title, time, length, description, invitees, lobby,
   the repeat rule; a series stays a series) sends the whole invitee list; once the server saved it,
   the organizer's device calls `update_meeting_chat_members` with that list and the users taken off
   it, so later invitees join the chat's encryption group and removed ones leave it (a signed
   commit that rotates the key). The list comes from the organizer's request, never from the
   server's member list: users carried over from the server's list go in only where the
   organizer's signed invitee list names them (e2ee.md §4.6, "Meeting chats", "Only invited";
   #1726), and the organizer's device signs the new list into the admitters statement.
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
   **Starting** a meeting is joining its chat's call. A planned meeting's start is a notification
   at the scheduled time, not a ring (#1421): the server pushes "Meeting A starts in 5 min" (the
   user's `meeting_reminder_minutes`, default 5, `0` off) and "Meeting A has started" (`notifies_meeting_start`,
   default yes), each with Join, to every invited user who hasn't declined and to the organizer,
   whether or not anybody has started the call. No ringtone, no call screen, and the call starting
   rings nobody. A call starting early tells nobody by itself; the reminder and the start notice
   come at their time. The organizer's app labels the button of a planned meeting "Start"; "Meet
   now" is the separate meeting with nobody invited. Ringing stays for real calls and for people
   added to a running call on purpose (step 7; an organizer's "ring everyone" for an urgent start
   isn't built).

   *How it is sent* (`services::meeting_notices`, every 20 s, on every instance): the server works
   out what is due from the meetings as they are now, so a meeting that moved or was cancelled has
   nothing planned that could fire; a series is expanded per occurrence, cancelled occurrences and
   skipped holidays left out. Each notice is taken in the database first (table `meeting_notices`,
   one row per occurrence, kind and user; `INSERT ... ON CONFLICT DO NOTHING`), so a restart or a
   second instance sends none twice. The row is all that is new on the server: the title, time
   and invitees are the ones it already holds, and nobody's device or time is added. A server down
   at the time sends the start notice within 10 minutes after it, not later. A notice waiting
   for a retry is dropped when the occurrence no longer stands (`push_retries.meeting_*`).

   Pushes go to phones always (they keep no timers of their own; a phone in the background keeps
   its connection for half a minute and gets nothing over it) and to browsers without a connection.
   An open desktop or web app shows its own reminders (notifications.md, "Meeting reminders").
   The wake-up names the chat, the meeting and the occurrence's start, and the device takes the
   title from the chat it has, or fetches it, as for every notification: no meeting text is added
   to a push (e2ee.md §7). Mutes and do-not-disturb apply as to a ring; a user already in the
   meeting's call isn't told again. iOS and Android show it through the meetings channel with Join
   (a tap opens the chat); the web service worker uses the same tag as the page's reminder, so
   the start replaces the reminder. External invitees get the invitation mail only; they have no
   account to notify. A desktop app that isn't running can't be told (it has no push), nor can a
   phone without a push registration.

   *Dismissing a notice* on one phone takes it down on the user's others (#1473): Join, or swiping
   it away where the platform says so, makes the phone report the occurrence
   (`POST /meetings/{id}/notices/dismiss`, `{"starts_at": <occurrence start>}`), and the server sends
   a silent `dismiss` wake-up naming the meeting and the occurrence to the user's phones
   (notifications.md, "Meeting notices"). The report can name only an occurrence a notice went to the
   caller for (the `meeting_notices` row; anything else is 404, so nobody learns about a meeting
   they weren't told of), clears only the caller's own rows (`dismissed_at`), and is idempotent: of
   several reports, also from two devices at once, exactly one sends the withdrawal. A notice that
   comes later (the start after the reminder) is a row of its own and shows. Queued retries of the
   occurrence's notices on the user's devices are dropped with it.
4. **Guests** open the meeting link `<base>/meeting/<token>#v=<fingerprint>` (see "Links"). The
   web client sends the token from the path to `POST /meetings/join` (no authentication) together
   with a display name and receives a guest session token valid until the meeting's end plus
   `guest_grace_secs`. The guest then exists as an ordinary guest user, is put into the lobby,
   and the hosts get a `lobby_join` event; once the guest's page set up its device it says so
   (`PUT .../lobby/me/device`) and the hosts get `lobby_join` again. `POST .../lobby/{user}/admit`
   makes the guest a member of the meeting chat (`membership_add` to the members,
   `lobby_decision` to the guest); `deny` turns them away and ends their session. A guest reads
   and speaks only once a device of the organizer, or of an admitter the organizer signed (step
   9), added theirs to the chat's encryption group (see "Guests in the browser"): on a host's
   "admit", or by itself when the organizer chose "no lobby" and the guest came with the meeting's
   token (the server's `has_lobby` doesn't decide it; whoever's device let them in is told each
   time, with "Remove"). Each guest of the link
   gets a one-time invite of their own (as for the shared guest link, step 5). Personal one-time links (`#invite=<token>`, a host's "One-time
   link", and the links mailed to external invitees before #987) still work; their guests always
   wait for a host's click.
   Hosts see who waits on the meeting's card and, while in the meeting's call, on the call screen.
   The meeting list (`GET /meetings`) marks each meeting whose lobby the caller hosts with
   `can_host_lobby: true` (the rule of the lobby's `is_host`: organizer, co-host or chat admin, in
   the meeting chat, meeting scheduled; #1305), so the devices of a host who isn't the organizer ask
   for its lobby and show it on the card outside the call too. With an older server, which sends
   no flag, such a host sees it on the call screen only, while in the call. A knock (`lobby_join`)
   reloads the lobby at once, without polling.
5. **Shared guest link** (`PUT /meetings/{id}/shared-guest-link`, hosts only): since #987 this is
   the meeting link itself. Every guest who uses it gets a one-time invite of their own
   (`shared_invite_id` names the link), so the rest is step 4. The link lets at most
   `max_shared_link_guests` guests (default 50) **in**: a place counts once its guest is admitted
   and is free again when the guest is turned away, leaves or their access ends. At most as many
   may wait at once, each for 15 minutes (the lobby timeout), so a leaked link fills the lobby
   for a while, not the meeting. The link ends with the meeting (its end plus
   `guest_grace_secs`, or its cancellation). A guest who reloads the page keeps their place, their
   ephemeral device and, once admitted, the meeting (below, "Guests in the browser"). **Making the link anew** (`POST /meetings/{id}/link`, hosts: organizer, co-hosts and chat admins;
   `DELETE .../shared-guest-link` does the same for older apps) does in one transaction: a new
   token (the old link opens nothing any more, `404`), every shared and unredeemed personal link
   of the meeting revoked, and guests still waiting no longer counted as "came with the link"
   (they wait for a click). A join that raced the change is checked against the token again and
   waits for a click too. Guests already admitted stay. Every invitee with an address gets an
   updated invitation with the new link. Anyone with the link can knock, so a link that got around
   can fill the lobby until the organizer makes it anew.
6. **Knocking**: a member who opens the meeting link without being in the meeting chat asks to be
   let in and waits in the lobby (`POST /meetings/link/{token}/knock` with the token, which answers
   the meeting and the lobby state; `POST /meetings/{id}/lobby/me` for an old link with the ID);
   the hosts get `lobby_join` (once per knock: knocking again while waiting is no news) and admit or
   deny as for guests. Members in the chat are answered `admitted` right away; a member admitted
   before who has left the chat waits again. A member with an old ID link always waits for a host,
   also without `has_lobby`: meeting IDs are not secret. With the token in a meeting without a
   lobby, the organizer's device (or an admitter's in the call, step 9) admits the member by
   itself, as for guests. For meetings that are
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
   encrypted `call_moved` event with the room, the added people and the DM call's ID). The history
   shows it as a line ("Alice moved the call into 'Call with notetaker'") which, for whoever is in
   the room, opens it: there the summary lands (issue #1290).
   `call_move` alone moves nobody: once the announcement is in, every device in the DM's call checks
   it (announced by a member of the DM's group, the room's group holds nobody but the DM's members
   and the named people, and the announcement is for the call this device is in now) and joins the
   room's call by itself with its microphone and camera as they were, so the whole call moves
   together without anybody tapping. While it follows, the call screen says "X moved the call into
   a meeting room and added Y" (for the notetaker: where the audio goes); a follow that fails stays
   in the DM's call and keeps a button to try again. A followed announcement is forgotten, so it
   never moves the device again, and the end of the call clears the announcements made for it (also
   those a failed follow left behind) and the device's note of the call's ID. The DM keeps its members and history, and the added people never
   see it. If the server refuses the move (409), the adding app cancels the room it just made.
   Web, desktop, iOS and Android all do this. "Add notetaker" in a DM or group DM call is such a
   move with the notetaker as the added one: it isn't rung but asked in with a request in the
   room, and the others' banner says where the audio goes (e2ee.md §8.2, "In every call"). So is
   "Add notetaker" in a channel call of a user who doesn't manage the channel's members and in the
   call of a meeting the user doesn't organize: for it the server moves the call of any
   conversation (the room must be another one), and the conversation, like the DM, doesn't
   change.

   **Latecomers** (issue #1321). Whoever starts or joins the conversation's call after it moved
   (everybody followed, so the DM's call ended and theirs would be a new one) is pointed at the
   room before joining: the pre-join screen (desktop, web), dialog (Android) and notice (iOS) say
   "The call moved into 'Room', which is running now" with a button to open the room. The rule
   (`teamcraft_client_api::notetaker::moved_call_hint`): the newest `call_moved` announced in the
   conversation (a member of its encryption group sent it; the core reads the room, not the
   server), the user is a member of that room (the core names no other, so nobody is pointed at a
   room they can't open), the room's call has other people in it now (the server roster), and the
   announcement is at most 3 hours old. The hint only opens the room; it joins nothing. The
   announcement's call ID plays no part: after the move the old call is gone.

   | Condition                   | What happens                                                                                                                                                                                                                 |
   |-----------------------------|------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
   | The added person declines   | Their ring stops on that device; the others go on in the meeting room. A missed call is noted only if nobody answered at all.                                                                                                |
   | Network loss while adding   | Nothing changes before the meeting room's group is set up and the move is accepted: the DM call goes on, the dialog shows the failure and retries with the same idempotency key (the same meeting room, nobody added twice). |
   | Network loss while moving   | A device that misses `call_move` (resume window gone) stays in the DM's call, alone once the others moved; the meeting room is in its list and its call can be joined from there.                                            |
   | A participant's old client  | It ignores `call_move` and stays in the DM's call; it is a member of the meeting room (and in its group) and joins its call from there.                                                                                      |
   | A forged or redirected move | The server's `call_move` without a matching announcement shows nothing; a room holding somebody the announcement didn't name is refused.                                                                                     |
   | Two people add at once      | The second move into another room gets 409; that user's app says so and both follow the first room.                                                                                                                          |
   | Server restart mid-move     | Calls and rings live in memory: the ring of the added people is lost, they find the running call in the meeting room.                                                                                                        |
8. **Adding people in a meeting's call** (#1141): only the organizer changes who is invited, so
   "Add people" in a meeting's call invites the picked people to the meeting when the organizer
   presses it (the edit path of the meeting dialog: the invitee list as a whole, then the chat's
   encryption group from that list, e2ee.md §4.6). They get the meeting's update (and its mail),
   its chat, and join the running call from Meetings; nothing rings them yet. Anybody else in the
   call gets the meeting's link to hand out instead: a member who opens it knocks and waits for the
   organizer (see 6).
9. **Admitters** (#1103; design in e2ee.md §4.6, "Meeting guests"): so that a guest isn't stuck
   while the organizer is away, the organizer's device signs who else may let guests into the
   chat's encryption group: everybody with a device in the meeting chat's group as that device sees
   it, guests left out, with the identity keys it pinned for them, and whether they let in people
   of the link by themselves (the organizer's "no lobby"). It publishes the statement with
   `PUT /conversations/{id}/meeting-admitters` (the meeting's organizer only) when the meeting is
   scheduled or edited, and keeps it current for meetings that run or start within a day (every
   ten minutes at most per meeting, and only if the list changed or ends too soon). The server
   keeps the newest per meeting (a higher sequence only, `409` otherwise) and checks its shape;
   members of the chat and admitted guests read it (`GET /conversations/{id}/meeting-admitters`).
   - **Lobby on:** a host's "admit" (organizer or chat admin) lets the guest's device in from that
     host's device if the organizer named the host.
   - **Lobby off:** a member's app in the meeting's call reads the lobby too (`GET .../lobby`
     answers members of a meeting without a lobby with those who came with the link,
     `is_host: false`; the server sends them `lobby_join` as well) and, if the organizer's signed
     list names this user and says "no lobby", admits them by itself like the organizer's
     (`POST .../lobby/{user}/admit` is allowed to them for people of the link; turning people away
     stays with the hosts).
10. **Co-hosts and handing the meeting over** (#1720; see "Hosts").

## Hosts

A meeting has one **organizer** and any number of **co-hosts** the organizer names; the meeting
chat's admins host too (as before). The organizer picks co-hosts among the invited people in the
schedule and edit dialogs ("Co-hosts", under the people invited) and in the call's host controls
(each participant: "Make co-host", "Remove as co-host", "Hand over host role"); the call's tiles
and the host controls' list label the organizer "Host" and co-hosts "Co-host", and the meeting
card shows "Co-host" to a co-host.

| What                                                                 | Organizer | Co-host | Chat admin | Member              | Guest |
|----------------------------------------------------------------------|-----------|---------|------------|---------------------|-------|
| Admit and deny in the lobby (members and guests)                     | yes       | yes     | yes        | link arrivals only¹ | no    |
| One-time guest links, the shared guest link, a new link              | yes       | yes     | yes        | no                  | no    |
| Host controls in the call (mute all, remove, lock, end for everyone) | yes       | yes     | yes        | no                  | no    |
| Change and cancel the meeting, restore, take it off the lists        | yes       | no      | no         | no                  | no    |
| Name co-hosts, hand the meeting over (`PUT .../meeting-hosts`)       | yes       | no      | no         | no                  | no    |
| Publish the admitters (`PUT .../meeting-admitters`)                  | yes       | no      | no         | no                  | no    |

¹ Without a lobby, as the organizer's signed admitters say (step 9). Hosts can't be removed from
the call by the host controls.

**Signed, not the server's word.** Who hosts is a list the organizer's identity key signs
(e2ee.md §4.6, "Meeting hosts"): the co-hosts with their identity keys, and the organizer from then
on. The organizer's device signs it and publishes it with `PUT /conversations/{id}/meeting-hosts`
(the organizer only; a sequence not above the stored one gets `409`); the server checks its shape,
that it is signed as the caller and that everybody it names is a person (no guest, no bot, not
deactivated) in the meeting chat, keeps it and reads from it who the co-hosts are
(`meeting_co_hosts`, shown as `co_host_user_ids` in the meeting). `GET .../meeting-hosts` serves
every handover and the newest list to the chat's members and admitted guests, who check them: so a
co-host's device lets guests into the encryption while the organizer is away, and the guest accepts
its welcome without trusting the server. Taking somebody off the invitees takes them off the
co-hosts on the server; the organizer's next list leaves them out.

**Handover.** "Hand over host role" signs a list naming the new organizer (a person in the meeting
chat) and keeps the former organizer as a co-host. The server makes them the meeting's organizer
(and the chat's admin, the former one a member and an invitee who accepted), raises the calendar
`SEQUENCE`, writes the event again with the new `ORGANIZER` and sends the invitees the update
(a calendar or mailer that is down doesn't undo the handover; the next change writes the event).
From then on the new organizer changes, cancels and names hosts. A series is handed over as a
whole: hosts belong to the meeting, not to one occurrence. Links made before keep working until
they expire or are made anew: they carry the former organizer's fingerprint, and guests follow the
signed handover from it; links copied afterwards carry the new organizer's. The new organizer's
device signs the hosts again with its own key as soon as it sees the handover (and with the
admitters' upkeep), so guests of new links find the co-hosts too. A meeting can be handed over at
most 32 times (the chain a guest reads stays short).

**When the organizer leaves.** Leaving the call of a meeting with people in its lobby and nobody
else to let them in (no co-host) asks first: "Hand over to …" (up to five people of the call, no
guests) signs the handover and leaves, "Leave anyway" leaves. The desktop tray's "Leave call"
brings the call up with the same question; leaving from the system's call UI (CallKit, Android's
call notification) doesn't ask.

**In the apps.** Android and iOS label the tiles of a meeting's call "Host" and "Co-host"; its
organizer gets "Hosts" in the call's controls (each other member in the call: "Make co-host",
"Remove as co-host", "Hand over host role") and the same question when leaving, the lobby count
read from `GET /meetings/{id}/lobby`. The apps don't schedule meetings: there, co-hosts are named
in the call.

**Telling people.** Every change sends `meeting_update` to the organizer, the invitees and the
co-hosts before and after; a handover also `conversation_update` to the chat's members, whose
devices then read who organizes the chat. An app that sees its user became a co-host or the
organizer says so ("You are now a co-host of …"). A push tells the same to whoever was newly
named a co-host or handed the meeting (never the one who made the change), so phones and closed
browsers learn it with the app closed: `meeting_co_host` or `meeting_handover`, naming the meeting
chat and the meeting, no text; the device names it from the chat as for a reminder. It goes like a
meeting's notice (a mute or DND without an override keeps it back, an open desktop or web app gets
no push and says it itself, nobody in the meeting's call is pushed). Every change is an
audit entry (`meeting_hosts`, `meeting_handover`, with the co-hosts added and removed).

| Condition                                       | What happens                                                                                                    |
|-------------------------------------------------|-----------------------------------------------------------------------------------------------------------------|
| Two organizer devices change the hosts at once  | The second gets `409`; the UI shows the failure and the list reloads with the first change                      |
| The organizer's app is offline when it is asked | The change fails and the host controls say so; nothing changes on the server, the organizer tries again         |
| Network loss during a handover                  | The server took it or not as a whole (one transaction); a lost answer shows the result after the next reload    |
| A co-host without a device or identity key yet  | The organizer's device can't sign their key: the change fails with "unknown identity" and nothing is sent       |
| An app from before #1720                        | Ignores `co_host_user_ids`; a co-host on it decides in the lobby, its device lets guests in as an admitter only |
| The server serves an older or a forged list     | Devices that saw a newer one refuse it; a forged one (another key) is passed over (e2ee.md §4.6)                |

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

### Holidays

"Skip public holidays" in the repeat section (`recurrence.skips_holidays`, stored in
`meetings.skips_holidays`) leaves out the meetings of a series that start on a public holiday, on
the organizer's wall clock (#1053). The holidays come from a `HolidaySource`
(`crates/teamcraft-server/src/holidays`), set by admins in the console (admin.md §6.3):

- **The bundled table**, picked by `holidays.region`: `DE` for Germany's nationwide holidays and
  `DE-BY` and the other federal states for theirs; `AT` for Austria (its holidays are federal law;
  the state patron days only schools and the public service keep are left out); `CH` for
  Switzerland's federal holidays and `CH-ZH` and the other cantons for theirs (without the
  cantons' optional and half days). Movable feasts are computed from Easter, so the table needs no
  network and no yearly update. Holidays only parts of a region keep are left out.
- **An iCalendar feed** (#1426), `holidays.feed_url` (https), instead of the region or on top of
  it: its all-day `VEVENT`s are holidays (`DTSTART;VALUE=DATE`, several days with `DTEND` or
  `DURATION` up to 31 days, a yearly `RRULE` with `COUNT`, `UNTIL`, `INTERVAL`, `BYMONTH`,
  `BYMONTHDAY` or one ordinal `BYDAY`, without `EXDATE`s); timed and cancelled events are not.
  Another `RRULE` keeps only the event's own date. A background task fetches it every
  `holidays.feed_refresh_secs` (a day) and right after the settings change, and keeps the last
  good copy in memory and in `server_settings` (`holidays.feed_copy`: five years back to ten
  ahead, at most 5000 dates), which is what the source answers from: a feed that is down never
  holds up listing or ringing. Fetching: https only, no proxy, the address checked for the URL,
  every redirect (at most three) and every connection, so neither a URL nor a name pointing into
  the server's network is reached (`crates/teamcraft-server/src/outbound.rs`, shared with Web
  Push); 10 s to connect, 20 s for the answer and between two pieces of the body, 60 s in all,
  2 MiB at most. An HTML page (a captive portal), a calendar that is cut off or broken, one
  without all-day events and one with too many are refused, and the copy before stays. After a
  failure it tries again after a minute, doubling up to the refresh interval (at most six hours),
  a quarter more or less at random; the console shows the copy and the last error under the
  setting, and the health check `holiday_feed` fails once the copy is older than
  `holidays.feed_max_age_secs` (a week) or no fetch worked since the start.

**Per organizer** (#1441): an organizer in another state or country picks a region of their own
under Settings, Account, "Public holidays" (`GET`/`PUT /api/v1/me/holiday-region`, stored in
`users.holiday_region`, one of the table's regions; absent or empty: the instance's region again).
Their series skip that region's holidays instead of the instance's; the instance's feed counts on
top of either, since it holds the company's own closing days. It is a setting of its own and not a
profile field: the profile's update replaces every field, so apps that don't know the region
would clear it, and the profile is what others see. Guests have none (403). The list asks for the
regions of all organizers of the listed series in one query and builds one source per organizer;
one series (its own page, restoring, ringing) reads its organizer's.
The Android and iOS apps (#1450) have the same row in their settings (Android: under the name,
iOS: above the status), talking to the endpoint directly rather than through the FFI like the other
`/me` settings; a pick is saved at once, the picker waits meanwhile and goes back on a failure, and
the row is off while the device is offline. The meeting list reads the meetings whenever it is
opened, so it shows the skipped occurrences without more.

Without a region (the organizer's or the instance's) or a feed copy nothing is skipped.

A skipped meeting is listed like a cancelled one (`is_occurrence_cancelled`), marked
`is_occurrence_holiday` ("Holiday"); the calendar event and the invitations have it as `EXDATE`
(for the series' past and the next two years), and it doesn't ring. Restoring it keeps that
meeting (`meeting_kept_holidays`, at most 1000 per series); cancelling it is refused with 409, it
is off already. Like `EXDATE`, skipped meetings count towards `COUNT`.

**Calendar events stay current** (#1441, `holidays/resync.rs`). Every write of a series' event
records the holiday starts it left out (`meetings.calendar_holidays`). A background task walks
the scheduled series that skip holidays and writes the event of each again whose holidays are
not the recorded ones any more: a new feed copy with other dates, other holiday settings, an
organizer's new region, or the two years ahead moving on. It runs 30 s after the console's
holiday settings change, after a fetched feed copy differs from the one before, and after an
organizer picks another region (their series only), so changes that come together are one run;
otherwise once a week (10 % at random on top), and ten minutes after the start, which also catches
what was asked for before a restart (the requests are only in memory, the records are not). It
reads 50 series at a time with a second's pause between pages; a series whose event is current
costs a few queries and no write, so a second run writes nothing. Each write takes the meeting's
calendar lock and reads the meeting under it, the same lock every other write and the removal of
the event take, so the task never puts back an older event over a change made meanwhile. Nothing
else changes: the same `SEQUENCE`, and no mail, since the invitees' calendars follow the CalDAV
event and an update mail per holiday change would be noise. A series whose event can't be
written keeps its old record and is tried again by the next run, an hour later instead of a week;
five failures in a row end a run (the calendar or the database is down). The weekly run's last
success is the health check `holiday_resync` (late after three missed runs); without a calendar a
run has nothing to do.

| Condition                                | What happens                                                                                                                                           |
|------------------------------------------|--------------------------------------------------------------------------------------------------------------------------------------------------------|
| No region set, or the region changes     | Lists and ringing follow at once; calendar events with the holiday task, 30 s later                                                                    |
| An organizer picks another region        | Their lists and ringing follow at once; their series' calendar events 30 s later                                                                       |
| The feed is down, slow or not a calendar | The last good copy stays in use (also over a restart); the console and `holiday_feed` say so                                                           |
| The feed changes                         | Lists and ringing follow with the next fetch; calendar events with the task right after it                                                             |
| The calendar is down for the task        | The events keep their old holidays; the task tries again after an hour; `holiday_resync` after three weeks                                             |
| Restart while a run is asked for         | The run of the start, ten minutes later, compares every series and catches up                                                                          |
| A holiday is also cancelled              | Listed as cancelled; restoring takes the cancellation back and keeps the meeting on the holiday                                                        |
| Old clients                              | They see a skipped meeting as cancelled and can restore it; changing the rule there turns skipping off; the region isn't part of the profile they save |

| Condition                        | What happens                                                                                                          |
|----------------------------------|-----------------------------------------------------------------------------------------------------------------------|
| Two devices cancel the same date | The second gets 409 (cancelled already); the list converges on the next `meeting_update`                              |
| Calendar down while cancelling   | The cancellation stands (logged); the event follows on the next change of the meeting                                 |
| The series' start moves          | Cancelled dates are kept by their old start and no longer match; the moved series has all its meetings                |
| Server restart while ringing     | Rings live in memory: the ring is lost, the meeting's call goes on and shows in the list                              |
| Old clients                      | They see a cancelled occurrence as a cancelled meeting, ignore `weekdays` and `until`, and cancel only whole meetings |

## Links

Opening one in an installed app: [app-links.md](app-links.md).

What a host copies and sends, and what each link lets the holder do:

| Link                                         | Who it is for                 | What it does                                                     |
|----------------------------------------------|-------------------------------|------------------------------------------------------------------|
| `<base>/meeting/<token>#v=<fingerprint>`     | everybody: members and guests | Opens the meeting for its members; others knock; a lobby decides |
| `<base>/meeting/<token>#invite=<jwt>&v=<fp>` | one guest without an account  | One-time; always waits for a host's click                        |
| `<base>/meeting/<id>` (and `#invite=…`)      | old links from before #987    | Still open the meeting, always through the lobby                 |

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
   guest's page makes its own keys (the ephemeral device), and a device of the organizer, of a
   co-host or of an admitter seals the meeting chat's keys to them when it adds the device: a new
   epoch, so nothing said before the admission is readable. The page checks that the device that
   added it is a host's by the signed host lists that follow from the fingerprint `v` of the link
   (the organizer, also after a handover, or a co-host; #1720), or an admitter's the current
   organizer signed for the meeting. Guest sessions are also ended by the server when the
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
   the server answers the join with, and waits for a host's click. Mailed links carry the `v` the
   server writes, so a mail-only guest relies on the server for it (e2ee.md §4.6, "Where `v`
   comes from"), and a first-visit guest runs the page the server serves.
2. After joining, the page opens its client core with the guest token, enrolls the ephemeral device
   (`EnrollRequest.is_guest`), keeps the fingerprint for the meeting chat, and reports its key's
   fingerprint with the knock (retried on the next poll if that fails). It shows the guest's key
   code and the organizer's code; the host's lobby entry shows the same guest code, the
   organizer's app its own code, so they can compare out of band. If the organizer's app is too
   old to let guests in by this flow, the page says it needs an update. While nobody of the meeting is there yet it shows "Waiting for someone in the meeting to
   let you in"; that is the normal case for a guest who comes early.
3. Once an organizer's or admitter's device let the guest in (on "Let in", which the host's app offers when the
   guest's device is ready, or by itself without a lobby; `LetGuestIntoMeetingChat`), the page
   shows the slim meeting view: the meeting's name, the chat, "Join the call" and "Leave the
   meeting". The call is the browser call engine; the guest's call key binding is signed by the
   guest's device.
4. "Leave" calls `POST .../leave`, deletes the page's database (its keys and messages) and shows a
   goodbye. Closing the page without leaving keeps the encrypted data in the browser until the next
   guest page is opened here, which deletes it (or the guest clears site data); nothing reuses it.

A member's app copies a link with the organizer's fingerprint as its core has it pinned
(`PrepareGuestLink`, made once per meeting); until it has it, "Copy link" says "Verifying…" and
copies nothing: a link with the server's fingerprint is never handed out from the app.

**Reloading.** Once the server let the guest into the lobby, the page seals what it needs to
carry on (the session token, the meeting, the chat, the guest's user and the organizer's
fingerprint) with a fresh random key (XChaCha20-Poly1305, `teamcraft_crypto::guest_resume`), keeps
only the ciphertext in the tab's **session storage** and moves to the guest's own address,
`/meeting/<link>#r=<key>&v=<fingerprint>`. The key is in the fragment only, so it never reaches a
server. Session storage belongs to the tab: a reload (F5, the panic overlay's "Reload") or the
browser restoring the tab brings the guest back; a closed tab, the address in another tab or
another browser opens nothing and knocks anew (with `v`, so the organizer's key is still checked). A
reload opens the session, keeps the device's database instead of deleting it, and asks the server
for the lobby state: admitted goes straight into the meeting, waiting keeps the place, a
turned-away guest or a refused token forgets the copy and ends the page. The server decides each
time; the stored copy proves nothing it doesn't accept. Any guest request the server answers with
401 (the meeting's access over, the guest removed) forgets the copy too, and so do "Leave" and a new
knock in the tab. A reload whose device database is gone (site data cleared) forgets the copy and
starts over at the plain link. A call the guest was in is joined again with "Join call".

Not done yet: a host the organizer's list doesn't name (not in the chat's group when the organizer's device
last signed it) lets nobody's device in, and the guest waits for another; the size of
what a guest downloads is not measured yet (the guest entry path is the initial chunk; the
chat and call chunks load after admission).

## Gateway events

| Event                 | To                                                               | When                                                   |
|-----------------------|------------------------------------------------------------------|--------------------------------------------------------|
| `meeting_update`      | organizer and invited users; on a host change also the co-hosts  | created, changed, cancelled, answered, hosts changed   |
| `conversation_update` | the meeting chat's members                                       | the meeting was handed over (the chat's organizer)     |
| `lobby_join`          | hosts; without a lobby also the chat's other members (no guests) | a guest knocked or set up its device, a member knocked |
| `lobby_decision`      | the guest or member                                              | admitted or denied                                     |
| `call_move`           | members of the DM                                                | its call moved into a meeting room                     |

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
  cancelling one), holidays per organizer, host controls (mute all, remove, lock, end for everyone) and
  meetings created in other CalDAV clients.
- Per-organizer calendars (see above).
- Hosts (#1720, #1859): the apps have no lobby or admit UI, so a co-host on a phone can't let
  anybody in; leaving from the system's call UI (CallKit, Android's call notification) doesn't ask
  about the handover; the apps' schedule dialog has no co-host field.
- Meeting links: member clients reacting to `lobby_decision` instead of asking; making the link
  anew also turning away the guests still waiting from it; the desktop and mobile apps opening
  `/meeting/…` links themselves; hosts seeing how many guests the link let in; a reloaded guest
  page joining the call by itself when the guest was in it.
