# Notifications and status

## Status

Open the status menu (your avatar):

- **Online**, **Away**, **Do not disturb**, **Offline**. Without input you turn away automatically
  after a while (15 minutes unless you or your admin chose otherwise, at least 5) and back to online
  as soon as you use the app again. **Away** chosen in the menu stays until you pick another status;
  **Online** goes back to the automatic status.
- **Custom status** with an emoji, a text and an expiry (30 minutes, 1 hour, today, this week, never),
  or one of the **presets** your admin offers. Presets: In a meeting, Commuting, Out sick, On vacation
  unless your admin chose others. `/status` does the same from the composer.
- **In a call** and **In a meeting** appear by themselves while you are in a call or your calendar
  says you are busy.

Presence and status are visible to everybody who can see you; the server sees them because it has
to show them. Admins can switch statuses off or allow presets only.

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
- No push while another of your devices is active, unless push mode is *always*. While a desktop is
  active, pushes to your phone wait two minutes and go out only if the message is still unread.
- Urgent messages repeat but never override mute or do-not-disturb.

Desktop notifications show the message text only if **Show message text** is on. Keyword
notifications are evaluated on your device (the server cannot read your messages).

## What a push shows

Push payloads are sealed for your device; the push service cannot read them. Your device opens the
payload and shows what your admin allows on lock screens: only "New message", the sender and
conversation, or also the beginning of the text.

| Device  | Status                                                            |
|---------|-------------------------------------------------------------------|
| Browser | Web Push, also with the tab closed, when your admin configured it |
| Android | FCM or UnifiedPush wake-ups, decrypted on the device              |
| iOS     | In preparation                                                    |
| Desktop | Notifications while the app runs                                  |

## E-mail digest of missed mentions

If your admin turned it on, you can opt in to an e-mail digest: one mail per
interval (an hour by default) that says only **how many** mentions are still unread, such as "3 new
mentions", with a link to the app. It never says where, from whom or what, because the server cannot
read your messages. Mentions you read in time are never mailed; muted conversations are skipped. It
needs an e-mail address on your account and starts from the moment you switch it on.

*Coming soon:* a switch in Settings. Until then your admin can tell you how to turn it on through
the API (`PUT /me/email-digest`).
