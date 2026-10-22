# Notifications and status

## Status

Open the status menu (your avatar):

- **Online**, **Away**, **Do not disturb**, **Offline**.
- **Custom status** with an emoji, a text and an expiry (30 minutes, 1 hour, today, this week, never).
  Presets: In a meeting, Commuting, Out sick, On vacation.

Presence and custom status are visible to everybody who can see you; the server sees them because it
has to show them.

## What notifies you

| Message in                                | Notifies when the level is |
|-------------------------------------------|----------------------------|
| Direct message, @mention, followed thread | All, Mentions              |
| Other channel messages                    | All                        |
| Muted conversation                        | Never                      |

Set the level per conversation (channel menu) or the defaults under **Settings > Notifications**:
levels, push timing, extra keywords, thread replies, and a **do-not-disturb schedule** (weekdays,
start and end time, optional list of senders allowed through).

Rules the server applies for push:

- A muted conversation or do-not-disturb never pushes.
- No push while another of your devices is active, unless push mode is *always*.
- Urgent messages repeat but never override mute or do-not-disturb.

Desktop notifications show the message text only if **Show message text** is on. Keyword
notifications are evaluated on your device (the server cannot read your messages).

## Push notifications

| Device  | Status                                                                       |
|---------|------------------------------------------------------------------------------|
| Browser | Web Push when your admin configured it (see [Admin: push](../admin/push.md)) |
| Android | FCM or UnifiedPush; the wake-up is decrypted on the device                   |
| iOS     | Coming soon                                                                  |
| Desktop | Notifications while the app runs                                             |

Push payloads are sealed for your device; the push service cannot read them. Until a browser can
open sealed payloads it shows a generic notification.
