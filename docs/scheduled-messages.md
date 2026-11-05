# Scheduled messages ("send later")

Issue #203. A person writes a message now and it goes out later: at the start of a colleague's
working day, not at midnight. This note states the choice, what the server learns, what happens when
the group changes, and what is not built yet.

## The choice: server-held

A scheduled message must go out on time with all of the sender's devices off. So the **server holds
it**: the sender's device encrypts and signs the message when it is scheduled, the server stores the
ciphertext and releases it at its time by its own clock, through the normal path of a message, so
members get the usual gateway event and push.

The earlier version of this feature kept the message on the sender's devices. That didn't meet the
requirement, and it is gone.

## The rule: the MLS epoch decides

A message is encrypted under a conversation's content key, and that key changes when a member is
removed or added "from join" (e2ee.md §4.2). A removed member still holds the old key, so a ciphertext
made today must not simply go out next week.

The obvious rule, comparing the message's content key with the conversation's current key, **can't be
built**: the server doesn't know the current key. It sees `content_key_id` per message only. The key
after a removal is derived from the MLS exporter, its ID is secret material nobody sends it, and
commits are opaque. Its only stand-in, "the key of the newest message", is still the old key right
after a removal, so that comparison would release under the very key it is meant to avoid.

What the server does know is the **epoch**: `mls_epoch_heads` moves in the same transaction as every
accepted commit.

- An entry records the epoch it was sealed in, and the server refuses it unless that is the current
  one (`409 epoch_conflict`).
- **Same epoch at its time: released.** No commit was accepted since. A removal counts for the group
  only once a member's device commits it: until then the removed person still is in the group, and an
  honest server doesn't deliver to somebody off its member list in that gap. So the key is as good as
  when it was sealed, and this is exactly as safe as sending at that moment. The release takes a shared
  lock on the epoch's row, so a commit is either before it (the entry waits) or after it (an ordinary
  race with a send).
- **Epoch moved: never released under that key.** The entry waits for a device of the sender to seal
  it again under the current key. The server counts every commit as a change, because it can't tell a
  removal from an addition. So additions and new devices also need sealing again, and a stale entry is
  more common than "the key changed" suggests. A flag "this commit removes nobody" is left out for now.

Rotations for message count or key age (§4.2) change the key without a commit. They need nothing: the
old key is still known to the same people, and members keep it in their rings.

### Sealing again

- A device **online** hears of the commit, merges it, and within 15 s seals the entry again, keeping
  its time (`seal_stale_scheduled`, in the upkeep every shell runs). A round that fails waits the
  upkeep's interval (1 min), doubling up to 10 min, and starts over once the list changes or the device
  connects again. A device without the group doesn't try: it isn't in the conversation any more, or its
  welcome is still to come.
- A device **offline** is woken: see "The wake" below.
- **Racing devices:** an upload names the entry's version and the current epoch. One wins; the other
  gets `409 version_conflict`, drops its work and reads the list.
- **Still stale at its time:** it is held with the reason `stale_epoch`, the sender's devices are woken
  once more, and it goes out as soon as one of them has sealed it. The sender sees that state.

So the promise, and what the screens say, is: **on time unless the group changed and all your devices
stayed off.** There is no "too late" hold any more: a held message that a device seals again goes out
then, however late, and the list has shown it as late all along.

### The wake

When a commit moves a conversation's epoch and somebody holds entries there, the server pushes to those
people's devices that aren't connected, kind `scheduled_seal`. The woken device reads the list, syncs
the group and uploads the new seal.

- **Payload:** the sealed wake-up of every push (e2ee.md §7), with the kind and conversation and no
  message or sender. On APNs the push provider sees a push to a device at a time, as for a `dismiss`. On
  FCM it also sees the kind: see Android below.
- **The gateway:** store apps get pushes through the push gateway, which refuses kinds it doesn't know.
  The server reads the gateway's `/v1/info` (at most hourly) and sends the kind only if it is listed. **A
  gateway without the kind means no wake for those phones,** not an error loop: such an entry waits for
  a device to run, as on desktop. A failed read keeps the list read before; with none read yet the wake
  goes to the push retry queue instead of being dropped.
- **Rate:** one wake for a burst of commits (`push.scheduled_wake_coalesce_ms`, 10 s), and at most one
  per user and conversation per `push.scheduled_wake_min_interval_secs` (5 min). A commit in that time
  waits for its end. No polling, and a commit in a conversation where nobody holds anything costs one
  indexed read and no push.
- **Wakes without a commit:** those queues are memory, so a restart loses them, and a push can get lost
  or come to nothing (a phone off). The scheduler therefore wakes the sender of an entry still stale in
  the last 15 minutes before its time, again every 5 minutes while it is, and of an entry held for an
  old epoch at its time, again every hour for a day after the hold. Only senders still in the
  conversation, and per entry, not per run: a phone that stays off costs at most these pushes.
- **Android:** FCM high priority, to get through Doze, marked outside the sealed payload with `v=0`, so
  that a wake-up that can't be opened shows no "New message". **Only this kind sets `v`, so it is a kind
  tag, not a generic "silent":** Google learns that the user of this device holds a scheduled message and
  that a group they are in just changed, though not which message or which group. This is accepted: the
  alternative is a "New message" that isn't one each time a wake-up can't be opened, and giving `v` to
  `dismiss` too wouldn't hide it, since high priority plus `v` stays unique. Sealing runs as WorkManager
  work with a network constraint and backoff. FCM may lower the priority of apps whose high-priority messages show nothing; these
  are rare, and this is why they aren't used for anything else. The F-Droid build gets the same through
  its UnifiedPush distributor, as reliable as that distributor is.
- **iOS:** a silent push (APNs background, priority 5), and a `BGProcessingTask` if that couldn't finish.
  iOS may defer or drop both, and doesn't deliver them after the user force-quit the app. **This isn't a
  promise: a stale entry whose only device is an iPhone can still wait.** The screen says so.
- **Desktop and web** seal when they run.

## What the server now learns

**Ahead of time**, for every scheduled message: that a ciphertext exists, its conversation, its sender
and sending device, its due time, its size, the thread it answers, its content key ID, whether it
mentions people and whom, and whether it is urgent. A message sent at once shows all this at once; a
scheduled one shows it days before. It also learns **when each edit, each new seal and each cancel
happens**. The server learns of plans before they happen, and that a message was scheduled at all, also
after its release if it kept track.

Not the text, the attachments, the reply target or anything else inside the ciphertext.

**It could withhold or delay a release**, as with any message it forwards, and it could release an entry
early. The sender's devices see the list, so a message that stays there after its time shows. They can't
prove a release happened at the right moment. **A cancel or edit isn't binding against a compromised
server:** it can keep the old ciphertext and release it anyway, signed by the sender as it is.

### The accepted risk

**A compromised server working with somebody removed before the message's time can hand them the text.**
The ciphertext sits on the server from the moment it is scheduled, under a key that person holds. The
epoch check stops an honest server from delivering it; it can't stop a server that gives the ciphertext
away. Before this feature, reaching a removed member took withholding a commit from the sender; here it
doesn't, for messages written before the removal. The product owner has accepted this. It is written down
in e2ee.md §4.4 too. Whoever must not read a text once removed shouldn't have it scheduled while they are
still in the conversation.

## What goes out, and what doesn't

Everything a send checks is checked again at the time, with nobody signed in. The sender's role is the
one their last sign-in brought.

| At the time                            | What happens                                                   | Reason in the list  |
|----------------------------------------|----------------------------------------------------------------|---------------------|
| All fine, same epoch                   | Released                                                       | –                   |
| The group moved                        | Held; goes out once a device seals it again                    | `stale_epoch`       |
| The device that signed it is gone      | Held the same way: members would refuse its signature          | `stale_epoch`       |
| The workspace turned scheduling off    | Held; the owner can send it now or cancel it                   | `policy_off`        |
| The sender left or was removed         | Never goes out; they can cancel it                             | `not_a_member`      |
| The sender may no longer post          | Held; they can cancel it                                       | `no_permission`     |
| The sender's account is deactivated    | Never goes out                                                 | `sender_inactive`   |
| The conversation was deleted           | Never goes out; removed when the conversation is               | `conversation_gone` |
| The thread it answers is gone          | Held; they can cancel it                                       | `thread_gone`       |

A hold other than `stale_epoch` stays until its owner acts. It doesn't go out by itself if the cause
goes away (the policy on again, say): by then it may be long out of date. Sealing again or a new time
makes an entry wait for its time again.

Mentions are metadata fixed when sealing. At release, people who left drop out of the list, so they
aren't notified.

## What recipients see

A released message is an ordinary message; the delay leaves no trace in its checks.

- **Ordering:** the server's ID and time at release decide its place, not when it was written. The
  signature and associated data bind conversation, sender, device, client message ID and key ID, not a
  time.
- **Sender checks:** a device that left is refused by the server time of the message (§4.4), and its
  leaving moves the epoch, so such a message is never released. The sender's other devices seal it again.
- **An older key:** a message released in its epoch uses that epoch's key, which every member holds.
  A member added "from join" is a commit, so the message is sealed again first and they can read it.
- **Heads:** the text carries the sender's heads of the workspace log and the channel's roles from when
  it was sealed. A reader who moved on since finds an older version, which neither `note_head` raises
  anything over (a fork is only the same version with another hash), so a late message causes no warning.
  It also tells nobody anything new, which the design of §4.6.3 already assumes of a device behind.

## Exactly once

The client message ID is the idempotency key. A release deletes the row and stores the message in one
transaction, only if the row is still at the version the scheduler read and the epoch still matches.

- **Two scheduler instances, or a rolling deploy:** `FOR UPDATE SKIP LOCKED` gives them different rows.
- **A crash:** a claim is a lease of 60 s, after which another instance takes the row. A crash after the
  commit loses that message's gateway event and push, as for a send; members get it at their next sync.
- **A cancel or edit racing the release:** whichever commits first decides. A later release finds nothing
  or another version and stops; a later cancel gets `404 scheduled_message_not_found`.
- **A retry of the schedule request** with the same client message ID answers with what is stored. The
  device compares that with what it asked for: another time or another text (a new reply target, say)
  becomes a change of the entry, another thread a cancel and a new entry, so a retry with a new time
  isn't reported as done under the old one. Composers keep the ID only after a failed try and only while
  the text stays the same, so a later message with the same words is a new one.

## The API

Only the sender's own entries, on every call, and only ciphertext, conversation and time.

| Request                                         | Does                                                                |
|-------------------------------------------------|---------------------------------------------------------------------|
| `POST /conversations/{id}/scheduled-messages`   | Holds a sealed message                                              |
| `GET /me/scheduled-messages`                    | The caller's list, soonest first, and the server's clock            |
| `PUT /me/scheduled-messages/{id}`               | New time and optionally a new seal, given the version it is based on |
| `DELETE /me/scheduled-messages/{id}`            | Cancels                                                             |
| `POST /me/scheduled-messages/{id}/send`         | Releases now; `409 epoch_conflict` if it must be sealed again first |

Every request is checked for membership, the right to post, the caller's own device, size, mentions
of members only, the epoch, and the time. The gateway event `scheduled_messages_update` tells the
sender's devices to read the list. A device decrypts it like any message of the conversation and keeps
the result in its encrypted local store. A device that holds no key for it yet says so. Text is never
logged.

## Limits on what a member can cost

A held message is at most `limits.max_scheduled_ciphertext_bytes` (64 KiB), so a list is bounded by that
times the cap; a longer one is refused with `413 payload_too_large`, and the apps say it is too long.
Held messages that nothing can solve (the sender left, the account is off, the conversation is gone) go
after `limits.scheduled_hold_retention_days` (30). A hold of another kind whose sender left since (one
held for an old epoch, say) becomes `not_a_member` then and counts from there. The list states the date
(`removes_at`) and the screens show it, the purge tells the owner's devices, and those keep the text,
marked as removed by the server, until the user dismisses it: the server's copy was the only one outside
the devices, so it can still be copied. A burst of commits makes one event and one push per sender and
conversation, a device reads the list at most every 5 s for events, and it opens only entries that changed.
Commits themselves have no limit of their own: the general limit per user applies
(`user_writes_per_minute`, 600).

## Open issues

- **A hostile member's garbage commit.** The server takes commits as opaque, so a member can move the head
  with bytes no device can merge. Devices then can't reach that epoch and every seal gets `epoch_conflict`,
  so held messages in that conversation stay stale, and send now with them. That fails safe, nothing goes
  out under the old key, but it stalls them for as long as the group stays that way, and a member can do it
  on purpose. The way out for the user is to cancel and send normally. A fix needs the server to tell a
  valid commit from garbage, which it can't.
- **A forked device.** A device whose group forked seals under its own idea of the current key. If its
  epoch number matches the head, the server takes it, and members on the other branch can't read the
  message, as with any message sent from a forked device. The difference from a send is that the sender
  may not be there to see it. Nothing here detects it.

## Admin policy

`scheduled_messages` in the signed workspace policy, edited in the console under People: `is_enabled`,
`max_horizon_days` (default 365, at most 3650) and `max_per_user` (default 50, at most 1000). **The server
enforces it.** Off, nothing new is taken and nothing goes out by itself. Send now still works, since that
is an ordinary send.

## Not built yet (issue #203)

- **Reminders.** "Remind me" on a message, synced `/remind`, native alarms on mobile, snooze and dismiss on
  all devices. Today `/remind` still lives in one browser's local storage.
- **Files.** The composer says they can't be scheduled yet.
- **Link previews:** a scheduled message goes without one.
- Recurring messages (PLAN.md).
