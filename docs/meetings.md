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
"Meeting chats").

## Model

| Thing              | Notes                                                                                            |
|--------------------|--------------------------------------------------------------------------------------------------|
| Meeting            | `scheduled` (planned, in the calendar, mailed) or `instant` ("Meet now", shared by link)         |
| Meeting chat       | A conversation of kind `meeting` (no space). Organizer = admin, invited users = members          |
| Invitee            | A TeamCraft user or an external person identified by e-mail; carries the answer (`rsvp`)         |
| Meeting link       | `<base>/meeting/<id>` for members; no credential (see "Links")                                   |
| Invite (guest)     | A personal one-time join link for somebody without an account (`meeting_invites`)                |
| Shared guest link  | One link per meeting for many guests (`meeting_invites.is_shared`); revocable, ends with it      |
| Lobby              | Guests and knocking members wait until a host (organizer or chat admin) admits them              |

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
   the guest); `deny` turns them away and ends their session. `has_lobby: false` lets guests of a
   personal link in at once.
5. **Shared guest link** (`PUT /meetings/{id}/shared-guest-link`, hosts only): the same URL shape
   and token as a personal link, but the invite it names is marked shared. Asking again returns the
   same link. Every guest who redeems it gets a one-time invite of their own (`shared_invite_id`
   names the link), so the rest is step 4, except that these guests **always** wait in the lobby,
   also when the meeting has none. The link admits at most `max_shared_link_guests` guests (default
   50), ends with the meeting (its end plus `guest_grace_secs`, or its cancellation) and a host can
   revoke it (`DELETE`, idempotent): nobody new gets in with it, guests already waiting or admitted
   stay. A link asked for after a revocation is a new one with a new URL. A guest's place is
   taken before their account is made and given back if the account or the redemption fails, so
   failed joins don't use up the link; a retried join after a lost answer does take a second place.
   Anyone with the link can use it, so a link that got around can use up all places until a host
   revokes it (the app says so next to the link).
6. **Knocking** (`POST /meetings/{id}/lobby/me`): a member who opens the meeting link without being
   in the meeting chat asks to be let in and waits in the lobby; the hosts get `lobby_join` (once
   per knock: knocking again while waiting is no news) and admit or deny as for guests. Members in
   the chat are answered `admitted` right away; a member admitted before who has left the chat
   waits again. Members always wait for a host, also without `has_lobby`: meeting IDs are not
   secret. For meetings that are over or cancelled, outsiders get the same 404 as for a meeting
   that doesn't exist, so knocking doesn't tell them which IDs are meetings; members of the chat
   get 409. Denying a member only refuses them (the decision is final for that meeting); their
   account stays. The server sends `lobby_decision` to the knocking member too; the web client
   doesn't act on it yet and asks `GET .../lobby/me` every few seconds while it waits and the page
   is visible.

## Links

What a host copies and sends, and what each link lets the holder do:

| Link                                      | Who it is for                | What it does                                       |
|-------------------------------------------|------------------------------|----------------------------------------------------|
| `<base>/meeting/<id>`                     | people in the organization   | Opens the meeting for its members; others knock    |
| `<base>/meeting/<id>#invite=<token>`      | one guest without an account | One-time; lobby unless the meeting has none        |
| the same, from the shared guest link      | any number of guests         | Reusable, revocable; always the lobby              |

The web client shows the meetings screen for `/meeting/<id>` and then opens the meeting (pre-join
screen) or knocks; a link with `#invite=` shows the guest page instead, before any sign-in. Members
who are signed out come back to the link after signing in. The app offers "Copy link" on every
meeting that is not over and in a dialog right after scheduling or "Meet now"; hosts copy the shared
guest link or a new one-time link with one click each (the link is made and copied at once).

Rate limits: `POST /meetings/join` counts against the per-address limit for requests without
credentials (`security.rate_limits.public_requests_per_minute`) and failed redemptions (401)
against `auth_failures_per_minute`; knocking and making links are writes of the member
(`user_writes_per_minute`). The shared link's guest limit bounds what one leaked link can do.

Guest sessions are HS256 tokens signed with `guest_token_secret`; every API call and gateway
connection accepts them like an access token and treats the holder as a guest. Guests can't schedule
meetings and see nothing until they are admitted.

## Gateway events

| Event            | To                          | When                                            |
|------------------|-----------------------------|-------------------------------------------------|
| `meeting_update` | organizer and invited users | created, changed, cancelled, answered           |
| `lobby_join`     | hosts                       | a guest redeemed a link, or a member knocked    |
| `lobby_decision` | the guest or member         | admitted or denied                              |

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
- Meeting links: member clients reacting to `lobby_decision` instead of asking; revoking the shared
  guest link also turning away the guests still waiting from it; the desktop app opening
  `/meeting/<id>` links itself; hosts seeing how many guests a shared link let in.
