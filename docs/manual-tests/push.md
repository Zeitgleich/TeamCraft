# Manual test: push notifications on real devices

What emulators and simulators can't show: Doze and App Standby, phone makers' battery managers, a
killed app, the iOS notification service extension's limits, CallKit, silent-push throttling. Run this
before every release on at least one Android phone (FCM build from the Play/test track, and the
UnifiedPush build if it is shipped) and one iPhone (TestFlight build). Target behaviour:
[notifications.md](../notifications.md), "Mobile push delivery".

Write down for every run: date, server version, app versions, phone model and OS version, push path
(gateway / direct / UnifiedPush and distributor). Mark each item ✅, ❌ (with what happened and the
time it took) or "n/a".

## 0. Setup

- Server with push configured as in production (`[push.gateway]` for the store apps). Two accounts:
  **A** (the tester, phone + desktop app) and **B** (sender, any client).
- A's phone: notifications allowed, no system DND, app signed in. A's desktop app signed in.
- A and B share a DM, a channel with notification level "all", and a channel with "mentions".
- Keep the phone's screen visible or record it; note the seconds between send and alert.
- Android only, for Doze tests: USB debugging on, `adb` on the computer.

## 1. Diagnostics and test push

- [ ] Phone → Settings → Notification diagnostics shows the provider (Android: FCM or the UnifiedPush
  distributor; iOS: APNs), notifications allowed, every channel's importance (Android), battery state.
- [ ] "Send test push": outcome "sent", then "received after N ms" within 10 s.
- [ ] Android full flavor: the app never asks to turn off battery optimisation; the screen only shows
  the state. UnifiedPush build: the screen names the distributor and points to *its* battery setting.
- [ ] Turn notifications off for the app in the system settings: the diagnostics show it; turn on again.
- [ ] Admin console / `GET /api/v1/admin/users/{A}/push-health`: the test push counts as sent for A's
  phone; no conversation, text or token appears.

## 2. Delivery

For each state, B sends a DM to A; expect an alert with sender and text (unless the workspace's push
content policy says otherwise) within ~5 s.

- [ ] App in the foreground, other chat open: in-app banner or notification, no duplicate.
- [ ] App in the background (home screen).
- [ ] Phone locked, screen off for 1 minute.
- [ ] App swiped away / killed (Android: from recents; iOS: swipe up in the app switcher).
- [ ] Android Doze: `adb shell dumpsys deviceidle force-idle`, then send; expect the alert anyway
  (high-priority FCM). Afterwards `adb shell dumpsys deviceidle unforce`.
- [ ] Android, phone left untouched on the desk for 1 hour, then send (real Doze, OEM battery manager).
- [ ] iOS Low Power Mode on.
- [ ] Phone offline (airplane mode) while B sends three messages, then online: the messages arrive
  (as one conversation notification on Android), none twice.
- [ ] Mentions: an @mention in the "mentions" channel alerts; a plain message there does not; a plain
  message in the "all" channel alerts (Android: "Messages" channel, mentions on "Mentions").
- [ ] Thread reply in a thread A follows alerts and opens the thread.

## 3. Content, grouping, app lock

- [ ] Several messages in one conversation: one notification per conversation (Android
  MessagingStyle, iOS one thread), newest last, ordered by send time.
- [ ] Tapping opens the right conversation; inline reply sends (Android; iOS long-press → reply);
  "mark as read" removes the notification.
- [ ] Urgent message from B: alerts (iOS: time-sensitive), and the repeat a few minutes later replaces
  the first notification and alerts again; acknowledging stops the repeats.
- [ ] App lock on (Settings): new and already shown message notifications say "New message from
  <name>", no text, no reply action — also in the unlocked shade; calls still ring with the caller.
  App lock off: the text comes back.
- [ ] One conversation shows one notification and no group summary; a second conversation adds the
  summary, which names both conversations and does not alert.
- [ ] Watch (Wear OS emulator paired with the phone, or a vendor bridge such as Garmin Connect): one
  message is one watch notification with sender and text; reply and mark as read work from the watch.
  With "sender only" or the app lock it reads "New message from <name>", still one notification. With
  the phone locked and the system set to hide sensitive content, the lock screen and bridges that
  follow it show "New message from <name>" (never the text); with "nothing", "New message".
- [ ] Badge on the app icon (iOS; Android launchers that show numbers) matches conversations with
  unread mentions and DMs.

## 4. Calls

- [ ] B calls A while A's phone is locked: Android full-screen incoming call with ringtone; iOS an
  incoming-call alert (or CallKit if the filtering entitlement is granted) — within ~3 s.
- [ ] Same with the app killed.
- [ ] Answer from the lock screen joins the call; decline stops the ring.
- [ ] A answers on the desktop: the phone stops ringing within a few seconds.
- [ ] B hangs up before anyone answers: the phone stops ringing; a missed call appears.
- [ ] A in DND (quick DND): no ring on the phone.
- [ ] Android: the ring stops by itself after about 45 s if nothing else happens.

## 5. Read elsewhere

- [ ] B sends a DM; the phone shows it. A reads it on the desktop: the phone's notification
  disappears (Android within seconds; iOS usually within a minute — iOS throttles silent pushes, and
  after a force-quit none arrive until the app is opened). The badge drops.
- [ ] Two conversations notified, A reads only one on the desktop: only that one disappears.
- [ ] Thread: A reads a followed thread on the desktop: the thread's lines/notification go, the
  conversation's other messages stay.
- [ ] Opening the app on the phone removes notifications of everything already read elsewhere.

## 6. Desktop attention and grace delay

Default grace delay 2 minutes (`[notifications] mobile_push_delay_secs`).

- [ ] A works in the desktop app (window in front, typing or moving the mouse): B's DM shows on the
  desktop; the phone stays quiet; after 2 minutes unread the phone alerts.
- [ ] Same, but A reads the DM on the desktop within the 2 minutes: the phone never alerts.
- [ ] A locks the computer's screen right after B's DM: the phone alerts about a minute later (the
  desktop reports "away" after a minute), not only after 2 minutes.
- [ ] A has the desktop app open but in the background (another app in front) for more than a
  minute: B's DM reaches the phone at once.
- [ ] Desktop closed or asleep: the phone alerts at once.
- [ ] Settings → Notifications → "Use desktop activity to hold phone notifications" off: the phone
  alerts at once while the desktop is merely open; with the desktop in active use (presence "online")
  it waits as before. On again: back to the behaviour above.
- [ ] Web client in a browser tab: as the desktop app (focus, visibility, input only).

## 7. Do not disturb

- [ ] Quick DND "1 hour" on the phone: no alerts for DMs and mentions; the desktop and other devices
  show A as DND; ending DND brings alerts back.
- [ ] Quick DND set on the desktop applies to the phone too.
- [ ] Weekly schedule with a range that covers now: silent; a range that doesn't: alerts.
- [ ] Two ranges on different weekdays (e.g. a night range and a weekend range) behave per day.
- [ ] Change the phone's time zone (system settings) while a schedule is active: the schedule follows
  the stored time zone; after editing and saving on the phone it follows the new one.
- [ ] Allow list: B on A's allow list reaches A during DND; another sender does not.
- [ ] "Urgent messages break through" on: an urgent DM reaches A during DND; an urgent message in a
  channel does not (unless the scope "everywhere" is set via the API).
- [ ] Per-conversation mute presets (1 h, 8 h, 1 day, 1 week, forever): no alerts from that
  conversation; unmute restores them.
- [ ] DND is in the diagnostics screen (state, until when, schedule).

## 8. Registration, sign-out, gateway token binding

- [ ] Sign out on the phone: B's DMs no longer reach the phone at all (no generic "New message"
  either). Sign in again: pushes work again (test push).
- [ ] Uninstall the app, B sends: nothing breaks for A's other devices; the server forgets the dead
  registration (push health shows the failure, no retries forever).
- [ ] Reinstall, sign in: test push works.
- [ ] Store apps with the gateway: the device's token is sealed for the gateway (the server stores no
  raw APNs/FCM token); the admin console shows APNs/FCM as active through the gateway.
- [ ] A second server (staging) the same phone signs into afterwards gets its own registration; signing
  out of one doesn't stop the other's pushes.

## 9. Platform specifics

**Android**

- [ ] Channel settings: change "Mentions" to silent in the system settings; the app respects it and the
  diagnostics show it.
- [ ] Full-screen intent permission (Android 14+): if revoked, the diagnostics show it and calls fall back
  to a heads-up notification.
- [ ] UnifiedPush build: with two distributors installed the app lets A choose; with none it explains
  what to install; uninstalling the distributor shows "unregistered" in the diagnostics.
- [ ] Android Auto (if available): messages read aloud, reply by voice works.

**iOS**

- [ ] Focus mode with TeamCraft allowed: time-sensitive urgent messages break through; others follow
  the Focus.
- [ ] Notification summary scheduled: normal messages go into the summary, urgent ones don't.
- [ ] Without the filtering entitlement: muted or DND pushes that still arrive show quietly (no sound,
  no banner) — note what iOS displays.
- [ ] CarPlay (if available): messages are announced.

## 10. Outages and retries

The server keeps wake-ups that failed for a temporary reason in a durable queue (notifications.md,
"Retries"). The automated test `push_retry` covers a killed server against a fake gateway; this
checks it end to end.

- [ ] Block the server's way to the push gateway (firewall rule or wrong DNS entry for the gateway
  host), B sends a DM: the admin console shows APNs/FCM as failing; nothing arrives.
- [ ] Kill the server process (`kill -9`) while the gateway is blocked, start it again, unblock the
  gateway: the DM arrives on the phone within a few minutes, once.
- [ ] Block for longer than the message TTL (`[push] ttl_secs`, lowered for the test): after unblocking,
  the stale wake-up never arrives.
- [ ] A reads the DM on the desktop while the gateway is blocked; after unblocking the phone shows
  nothing for it.
- [ ] UnifiedPush build: stop the distributor app (force-stop) and start it again, or switch the phone
  offline for ten minutes: the app registers again by itself (diagnostics show "registered" again
  within minutes after the distributor or network is back; no user action).
- [ ] UnifiedPush build with two distributors: uninstall the one in use; the app moves to the other by
  itself and the test push works.

## 11. Android device matrix (Doze and phone makers' battery managers)

Phone makers change how Android treats apps in the background; this is where pushes get lost in
practice (<https://dontkillmyapp.com>). For each phone run section 2 ("Phone locked, screen off",
"App swiped away", "Doze", "untouched for 1 hour"), section 4 (ring with the app killed) and the
UnifiedPush lines of section 10 where the UnifiedPush build applies. Use the app's default settings:
the full flavor never asks for a battery exemption, so the result must hold without one.

Fill in date, app version, OS/skin version and the result per cell; leave "needs device test" until a
person has run it.

| Maker / skin                       | Example model    | Android | FCM: locked       | FCM: killed       | FCM: 1 h idle     | Ring killed       | UnifiedPush       | Notes                               |
|------------------------------------|------------------|---------|-------------------|-------------------|-------------------|-------------------|-------------------|-------------------------------------|
| Google Pixel (stock)               | Pixel 8          | 15/16   | needs device test | needs device test | needs device test | needs device test | needs device test |                                     |
| Samsung One UI                     | Galaxy S24 / A55 | 15/16   | needs device test | needs device test | needs device test | needs device test | needs device test |                                     |
| Xiaomi HyperOS / MIUI              | Redmi Note 13    | 14/15   | needs device test | needs device test | needs device test | needs device test | needs device test |                                     |
| OnePlus OxygenOS / Oppo ColorOS    | OnePlus 12       | 15      | needs device test | needs device test | needs device test | needs device test | needs device test |                                     |
| Huawei EMUI (no Google services)   | P30              | 10–12   | n/a (no FCM)      | n/a (no FCM)      | n/a (no FCM)      | needs device test | needs device test |                                     |
| Motorola (near stock)              | Moto G           | 14/15   | needs device test | needs device test | needs device test | needs device test | needs device test |                                     |
| Fairphone / LineageOS / GrapheneOS | Fairphone 5      | 14/15   | needs device test | needs device test | needs device test | needs device test | needs device test | GrapheneOS: sandboxed Play services |

What to note when a cell fails: the delay until the alert came (or "never"), whether opening the app
brought it, the push health in the admin console (sent / failed counts), and the skin's battery
setting for the app ("optimised", "restricted", "unrestricted"). A failure that only goes away with an
exemption is documented in the user help for that maker, not worked around in the app.

## Result

Note failures with their time stamps and the server's push health for the phone. File an issue per
failure; the release waits until sections 2, 4 and 5 pass on both platforms.
