# Scheduled messages ("send later")

Issue #203. A person writes a message now and it goes out later: at the start of a colleague's
working day, not at midnight. This note states the choice between holding the message on the server
and holding it on the client, what the server learns, what it means for the user, and what is not
built yet.

## The choice: client-held

The message waits on the **sender's own devices**, and **any device of the sender that is online at
or after the time** sends it. The server holds nothing for it.

Holding a ciphertext on the server and releasing it at the time (the first idea in PLAN.md) is
unsound here:

- A message is encrypted under the conversation's current content key (e2ee.md §4). A key switches
  when a member is removed, or added "from join". A ciphertext made today may then be sent under a
  key a member removed in the meantime still holds, which is exactly what `ensure_mls_current` exists
  to prevent before every send (§4.6), and members who joined since may be unable to read it. The
  server can't re-encrypt, and the sender's device isn't there at release time to do so.
- The server would learn that a message exists, for which conversation and when it goes out, days
  before it does, and it could withhold or reorder releases without anyone noticing.

With client-held sending, a scheduled message is encrypted at the moment it goes out, under the key
that is current then, exactly like one written then. Members who joined since read it; members who
left don't.

**The price, said plainly in every client:** the message goes out only when one of the sender's
devices is online at or after its time. If all of them are off, it waits for the first one back. A
laptop that is closed over the weekend and a phone the OS won't wake mean a Monday 08:00 message may
leave at 09:30. The screens that offer "Send later" and list the waiting messages say this, and each
waiting message says where it stands (below).

A message with files can't be scheduled yet: the account data holds text, and picked files would
have to stay staged on one device. The composer says so instead of offering it.

## What the server learns

Nothing beyond what it learns today:

| What                         | The server sees                                                                                                                                                                   |
|------------------------------|-----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| A message being scheduled    | The key bag changed and its size (e2ee.md §6.4). Not that a message was scheduled, not for whom, not when. Drafts make the key bag change the same way.                           |
| A waiting message            | Nothing. Text, conversation and time are only in the account data, encrypted with the backup key.                                                                                 |
| The message going out        | An ordinary send: ciphertext, sender, conversation, at that moment. It can't tell it was scheduled, except that the sending device may have been idle before.                    |
| The admin's rule             | The signed workspace policy, as for link previews and the notetaker. The server stores the bytes and checks the ranges; it can't change the rule without an owner's signature.    |

One small addition to the wire: `GET /me/key-bag` answers with the server's clock (`server_now`),
which reveals nothing and is what clients need to not trust their own (below).

Text is never logged. Refusals and failures log only error kinds and counts.

## Where it lives

- **Data:** `scheduled` in the account data (`teamcraft-core/src/account_data.rs`), keyed by the
  message's client message ID, each entry stamped like saved items and drafts. A sent or cancelled
  message becomes a tombstone without its text, forgotten after 90 days like the others. Fields added
  later have defaults; a priority of a newer version reads as `Unknown`, so an entry never costs the
  whole account data.
- **Rules:** `teamcraft-client-api/src/scheduled.rs`: the status of a message and the refusals of a
  form, in one place. The mobile apps word what the core decides; their form checks repeat only the
  time and text rules, and the core checks again on the way in.
- **Sending:** `teamcraft-core/src/client/scheduled.rs`, called from the upkeep (`upkeep.rs`) every
  15 s. Every shell's loop runs the same upkeep, so desktop, web, Android and iOS do the same. While
  nothing is due this is one local read and no request.

## Sending once

1. A local read: anything due? If not, stop.
2. **Read the newest key bag** and merge it. The user may have cancelled or changed the message on
   another device, or another device sent it. A device back from a weekend offline therefore never
   sends what was cancelled meanwhile. Offline this read fails and the message stays due.
3. Put each due message into the outbox with **its entry's ID as client message ID**, encrypted when
   the outbox sends it. Writing the same ID again changes nothing (`enqueue_as`).
4. Mark it sent (a tombstone) and upload the key bag, so the other devices stop.

Two devices racing both reach the server with the same client message ID; `message_client_ids` keeps
one message and answers both with its ID. A crash between 3 and 4 finds the entry still waiting and
enqueues the same ID again. A retry after a lost answer is the outbox's usual retry.

A narrow race stays: a device edits or re-times a message in the seconds between another device's
key bag read and its upload. The other device sent the older text, and the edit, being newer, brings
the entry back; when it comes due, the server's deduplication drops it and the entry ends. Nothing is
sent twice, but that edit was too late. Devices of one user rarely do this within seconds of the time.

## Time

Every time is server time. The server's clock comes with each key bag answer and is kept as an offset
(`scheduled/server_clock_offset`), and step 2 refreshes it just before sending, so a device whose clock
runs ahead doesn't send early and one that runs behind doesn't send late. "In 1 hour" counts from the
server's clock; "tomorrow morning" (08:00) and "Monday morning" are what the sender's calendar and time
zone call that morning. A device that wakes up (sleep, resume, a corrected clock) checks at once.

## Where a message stands

| Status        | Means                                                                                   | Who acts                     |
|---------------|-----------------------------------------------------------------------------------------|------------------------------|
| `waiting`     | Its time hasn't come.                                                                   | Nobody                       |
| `due`         | Its time has come: it goes out as soon as one of the user's devices has a connection.   | The next device that is online |
| `missed`      | Its time passed longer ago than the workspace's late limit (default 24 h).              | The user: send, edit, cancel |
| `policy_off`  | The workspace turned scheduling off after it was written.                               | The user: send or cancel     |
| `unavailable` | This device doesn't know its conversation (the user left it, or it isn't loaded here). | The user: cancel             |

`missed` exists because a message written for Monday morning shouldn't surprise everybody the
following Thursday. Those needing the user are counted next to "Scheduled" in the sidebar (desktop,
web) and shown as a card or row on the main screen (Android, iOS). A message that goes out and is
then refused by the server (the sender lost the right to post) is a failed message in its
conversation, with "Try again" and "Delete", like any other.

## Admin policy

`scheduled_messages` in the signed workspace policy, edited in the console under People (admin.md
§6): `is_enabled`, `max_horizon_days` (default 365), `late_limit_hours` (default 24). Only member
clients can apply it, since the server sees no scheduled messages; a modified client can always send
a message later by itself, so nothing is lost by that. With scheduling off "Send later" isn't
offered, and messages already waiting stay with their owner as `policy_off`.

## Limits

At most 50 waiting messages per user with at most 8 KiB of text each: the account data travels in
the key bag, which is uploaded whole and limited to `limits.max_key_bag_bytes` (default 1 MiB).

## Closed apps

Desktop and web send while they run. On top of the upkeep:

- **Android:** when the app leaves the foreground it plans one WorkManager run at the next message's
  time, with a network constraint, which keeps the sync loop going until what is due has left. Doze
  and App Standby may defer it.
- **iOS:** the same with a `BGProcessingTask`. iOS decides when it runs and may not run it at all,
  certainly not after the user force-quit the app. A silent push waking the app for something else
  sends what is due too.

Neither is a promise, which is why the screens promise only "when one of your devices is online".

## Not built yet (issue #203)

- **Reminders.** "Remind me" on a message, synced `/remind`, native alarms on mobile, snooze and
  dismiss on all devices. They will sit in the account data the same way and share the time handling;
  today `/remind` still lives in one browser's local storage.
- Scheduled messages with files, and recurring ones (PLAN.md).
