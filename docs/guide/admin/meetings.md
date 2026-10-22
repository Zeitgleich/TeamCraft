# Meetings with CalCraft and mail

Scheduled and instant meetings with their own chat, calendar events, invitation e-mail, RSVP handling
and guests who wait in a lobby. Reference: [docs/meetings.md](../../meetings.md). Without the
`[meetings]` section the meetings endpoints answer 503.

## What leaves end-to-end encryption

Title, time, description and the invitee list are **not** encrypted: they are in the calendar and in
the invitation e-mails anyway. The meeting chat and media are encrypted like any other conversation.

## Configure

Uncomment and fill `[meetings]` in the server file ([example](../../../teamcraft.example.toml)):

| Key                          | Purpose                                                                                    |
|------------------------------|--------------------------------------------------------------------------------------------|
| `public_base_url`            | Join links are `<base>/meeting/<id>`; also the host part of iCalendar UIDs                 |
| `guest_token_secret`         | **Required**, 32+ random bytes (`openssl rand -base64 48`). Signs guest links and sessions |
| `guest_grace_secs`           | Links work until this long after the meeting ends (default 2 h)                            |
| `organizer_fallback_address` | Organizer of events when the organizer has no e-mail address                               |
| `max_invitees`               | Default 200                                                                                |
| `inbox_token`                | Bearer token of the inbox handler for RSVP e-mail; without it that endpoint answers 503    |

### Calendar (CalCraft or any CalDAV server)

```toml
[meetings.caldav]
calendar_url = "https://cal.example.com/dav/calendars/teamcraft/meetings/"
username = "teamcraft"
# password via TEAMCRAFT_MEETINGS__CALDAV__PASSWORD (an app password works)
```

All meetings go into **one collection written by one account**; per-organizer calendars would need
per-user credentials the server does not hold. A meeting is rolled back if the calendar rejects the
event. Without this section nothing is stored in a calendar.

### Mail (iMIP)

```toml
[meetings.smtp]
host = "smtp.example.com"
port = 587
security = "start_tls"        # or "tls" (465); "none" only for a local mail catcher
username = "teamcraft"
from_address = "meetings@example.com"
```

Invitations, updates and cancellations are sent as `text/calendar` mail with `invite.ics`; external
invitees get a personal join link. Texts live in `[meetings.mail_texts]` (translate them there;
placeholders `{title} {organizer} {start} {end} {description} {join_url}`). Without SMTP no mail is
sent.

### RSVP by e-mail

Invitees can answer by mail. Your inbox handler (or CalCraft) forwards the iMIP REPLY as
`text/calendar` to `POST /api/internal/v1/meetings/replies` with `Authorization: Bearer <inbox_token>`.
The handler vouches for the sender (check DKIM/SPF there): the server does not authenticate mail.
Keep `/api/internal/` off the public internet.

## Guests and the lobby

The organizer or a host creates a personal one-time link for a person without an account:
`<base>/meeting/<id>#invite=<token>`. The token sits in the URL fragment and never reaches servers
in requests until the page sends it. The guest enters a name, waits in the lobby and a host admits
or turns them away. Guests are ordinary guest users limited to the meeting until its end plus the
grace time. `has_lobby: false` skips the lobby.

Rotating `guest_token_secret` invalidates open links and guest sessions; do it between meetings.

## Not done yet

"In a meeting" presence from the calendar, recurring series exceptions, host controls (mute all,
remove, lock, end for everyone), meetings created in other CalDAV clients, per-organizer calendars,
and browser audio/video for guests.

## Development

`docker compose -f docker-compose.dev.yml up -d mailpit calcraft` starts a mail catcher (SMTP
`127.0.0.1:1025`, UI `http://127.0.0.1:8025`) and CalCraft (CalDAV `127.0.0.1:5232`);
`dev/init-calcraft.sh` creates the account.
