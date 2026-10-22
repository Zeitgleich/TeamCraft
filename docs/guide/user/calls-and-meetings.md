# Calls and meetings

## Calls

Start an audio or video call from a direct message, a group message or a channel.

- **1:1 and group calls:** DMs and group DMs ring for up to 30 seconds (default) or until somebody
  else joins. Others see an **incoming call** card and can accept or decline. A call nobody picks up
  leaves a **missed call** entry in the conversation.
- **Huddles:** a call in a channel that people join and leave freely. A huddle bar shows who is in
  it; while you are in another view a pill in the sidebar brings you back.
- **Pre-join screen:** choose camera, microphone and speaker, watch the microphone meter, test the
  speaker, pick a background effect and mirror your self-view.
- **In the call:** mute, deafen, camera (up to 720p with several quality layers), screen share,
  raise hand, **push-to-talk**; layouts *gallery*, *active speaker* and *shared screen*.
- **Backgrounds:** blur (light or strong), four built-in scenes and, with the latest desktop,
  **your own pictures**. Own pictures are scaled and stripped of metadata on your device, encrypted,
  and synced to your other devices; nobody else can read them.

Calls are end-to-end encrypted; the media server only forwards packets. Video is H.264, using your
graphics hardware where the system offers it.

### Status

| Part                                              | Status                                               |
|---------------------------------------------------|------------------------------------------------------|
| Desktop calls, camera, screen share, push-to-talk | Works; needs the admin to run the media server (SFU) |
| Android calls (system call UI, Android Auto)      | Works                                                |
| Background blur and scenes                        | Works on desktop; the segmentation model is bundled  |
| iOS, browser calls                                | In preparation                                       |
| Reactions in calls, recording, live captions      | Coming soon                                          |

If calls fail to start with *service unavailable*, the admin has not configured the call service
([Admin: calls and SFU](../admin/calls-sfu.md)).

## Meetings

Open **Meetings** in the sidebar.

- **Start now** (or `/meet`) creates an instant meeting and a link to share.
- **Schedule** asks for title, time and length, description, members to invite, external invitees
  by e-mail, recurrence and whether guests wait in a **lobby**. Invitees get a calendar e-mail
  (when the admin configured mail) and members see the meeting in the list.
- **RSVP:** invited members answer in the app, e-mail invitees answer by replying to the invitation.
  The organizer sees a summary: accepted, maybe, declined, waiting.
- **Join** opens the meeting's own chat and the pre-join screen. The chat is encrypted like any other.
- **Hosts** (organizer, chat admins) see the **lobby**: people waiting, with *Let in* and *Turn away*,
  and **Host controls**: *Mute everyone*, *Lock meeting* (nobody new gets in), *Remove* a person and
  *End for everyone*.
- **Invite a guest:** a host creates a personal one-time link for a person without an account.
- While a meeting is on your calendar you show as **In a meeting** if your admin connected the calendar.

Meeting title, time, description and the invitee list are *not* end-to-end encrypted: they live in
the calendar and the invitation e-mails too.

Not available yet: editing a scheduled meeting and handling single occurrences of a recurring
meeting. Guests who join in the browser can wait in the lobby and get admitted, but audio and video
in the browser are coming soon. See [Guests](guests.md).
