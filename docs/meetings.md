# Meetings

Server side of PLAN.md §9 and ARCHITECTURE.md §6.4: scheduled and instant meetings with their own
chat, CalDAV events, iMIP invitations, RSVP handling, and guests without an account who wait in a
lobby. Media (the call itself) is the existing call stack; a meeting's chat conversation is the
conversation the call belongs to.

## What is visible to the server

Title, time, description and the invitee list are **not** end-to-end encrypted: they live in the
calendar and in the invitation e-mails anyway. The meeting **chat and media are encrypted** like any
other conversation (e2ee.md). Guests get an ephemeral device and show up as "Guest (unverified)".

## Model

| Thing          | Notes                                                                                       |
|----------------|---------------------------------------------------------------------------------------------|
| Meeting        | `scheduled` (planned, in the calendar, mailed) or `instant` ("Meet now", shared by link)    |
| Meeting chat   | A conversation of kind `meeting` (no space). Organizer = admin, invited users = members     |
| Invitee        | A TeamCraft user or an external person identified by e-mail; carries the answer (`rsvp`)    |
| Invite (guest) | A personal one-time join link for somebody without an account (`meeting_invites`)           |
| Lobby          | Guests wait until a host (organizer or chat admin) admits them; `has_lobby: false` skips it |

## Flows

1. **Schedule** (`POST /meetings`): chat and invitees are stored, the iCalendar event with the join
   link is written to the configured CalDAV calendar (the meeting is rolled back if the calendar
   rejects it) and every invitee with an address gets an iMIP e-mail (REQUEST). External invitees get
   a personal join link in it.
2. **Change / cancel** (`PATCH` / `DELETE /meetings/{id}`, organizer only): SEQUENCE increases,
   the event is replaced or deleted, staying invitees get an update, new ones an invitation,
   removed ones and everybody on cancellation a CANCEL. The chat of a cancelled meeting is archived
   and stays readable.
3. **Answer**: invitees answer in the app (`PUT /meetings/{id}/rsvp`) or by e-mail. The e-mail path
   is `POST /api/internal/v1/meetings/replies` with the iMIP REPLY as `text/calendar`, called by the
   inbox handler that receives the mail (or by CalCraft) with the `inbox_token`. That caller vouches
   for the sender by passing the authenticated envelope sender in `X-Envelope-Sender`; the server does
   not authenticate mail itself, but accepts a reply only for exactly that address. A reply answers
   for one attendee (RFC 5546) and is short; anything else is refused.
4. **Guests**: the personal link is `<base>/meeting/<id>#invite=<token>`; the token is in the
   fragment, which browsers never send to servers. The web client sends it to
   `POST /meetings/join` (no authentication) together with a display name and receives a guest
   session token. The token is **one-time** (the database marks the invite redeemed) and only valid
   until the meeting's end plus `guest_grace_secs`. The guest then exists as an ordinary guest user,
   is put into the lobby, and the hosts get a `lobby_join` event. `POST .../lobby/{user}/admit`
   makes the guest a member of the meeting chat (`membership_add` to the members, `lobby_decision` to
   the guest); `deny` turns them away and ends their session.

Guest sessions are HS256 tokens signed with `guest_token_secret`; every API call and gateway
connection accepts them like an access token and treats the holder as a guest. Guests can't schedule
meetings and see nothing until they are admitted.

## Gateway events

| Event            | To                          | When                                  |
|------------------|-----------------------------|---------------------------------------|
| `meeting_update` | organizer and invited users | created, changed, cancelled, answered |
| `lobby_join`     | hosts                       | a guest redeemed a link and waits     |
| `lobby_decision` | the guest                   | admitted or denied                    |

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

- "In a meeting" presence from the calendar (PLAN.md §9), recurring-series exceptions, host controls
  (mute all, remove, lock, end for everyone) and meetings created in other CalDAV clients.
- Per-organizer calendars (see above).
