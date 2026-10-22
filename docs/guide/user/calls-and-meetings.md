# Calls and meetings

## Calls

Start an audio or video call from a direct message, a group message or a channel.

- **1:1 and group calls:** DMs and group DMs ring for up to 30 seconds (default) or until somebody
  else joins. Others see an **incoming call** card and can accept or decline.
- **Huddles:** a call in a channel that people join and leave freely. A huddle bar shows who is in
  it; while you are in another view a pill in the sidebar brings you back.
- **Pre-join screen:** choose camera, microphone and speaker, watch the microphone meter, test the
  speaker, pick a background effect and mirror your self-view.
- **In the call:** mute, deafen, camera, screen share, raise hand; layouts *gallery*, *active speaker*
  and *shared screen*.

Calls are end-to-end encrypted; the media server only forwards packets.

### Status

| Part                                                     | Status                                                   |
|----------------------------------------------------------|----------------------------------------------------------|
| Desktop calls, camera (H.264), screen share              | Built; needs the admin to run the media server (SFU)     |
| Background blur and images                               | Shaders exist; the segmentation model is not bundled yet |
| Reactions in calls, recording, captions, own backgrounds | Coming soon                                              |
| Android calls, iOS                                       | Coming soon                                              |
| Browser calls                                            | Coming soon (the browser build has no media yet)         |

If calls fail to start with *service unavailable*, the admin has not configured the call service
([Admin: calls and SFU](../admin/calls-sfu.md)).

## Meetings

Open **Meetings** in the sidebar.

- **Start now** creates an instant meeting and a link to share.
- **Schedule** asks for title, time and length, description, members to invite, external invitees
  by e-mail, recurrence and whether guests wait in a **lobby**. Invitees get a calendar e-mail
  (when the admin configured mail) and members see the meeting in the list.
- **RSVP:** invited members answer in the app, e-mail invitees answer by replying to the invitation.
- **Join** opens the meeting's own chat and the pre-join screen. The chat is encrypted like any other.
- **Hosts** (organizer, chat admins) see the **lobby**: people waiting, with *Let in* and *Turn away*.
- **Invite a guest:** a host creates a personal one-time link for a person without an account.

Meeting title, time, description and the invitee list are *not* end-to-end encrypted: they live in
the calendar and the invitation e-mails too.

Not available yet: editing a scheduled meeting, handling single occurrences of a recurring meeting,
host controls (mute all, remove, end for everyone) and the "in a meeting" status from the calendar.
Guests who join in the browser can wait in the lobby and get admitted, but audio and video in the
browser are coming soon. See [Guests](guests.md).
