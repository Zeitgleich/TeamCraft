# Data export (issue #211)

Two exports are asked of a chat that keeps personal data: a person's copy of their own data (GDPR
Art. 15 and 20), and an organisation's copy of conversations for auditors or courts (compliance,
legal hold). TeamCraft's server only holds ciphertext of messages and files (e2ee.md §4.4), so it
can produce neither alone: content can only be exported by a client that holds the keys.

| Export                             | Who                       | Status                                                     |
|------------------------------------|---------------------------|------------------------------------------------------------|
| Personal export ("Export my data") | the user, on their device | Implemented on desktop and web (this document)             |
| Server-held personal data          | the user, through the API | Implemented: `GET /api/v1/me/personal-data`                |
| Legal hold                         | owners, by signed policy  | Implemented on the server, desktop and web (§4.2)          |
| Compliance export                  | admins, through a vault   | Designed (§4.1); needs the vault node (e2ee.md §11)        |
| Audit log CSV                      | admins                    | Implemented in the admin console (§4.3)                    |

## 1. Personal export

Settings > Account > **Your data**. The user chooses which messages (only their own, or every message
they can read) and whether the files they sent go in, then **Export my data…**. The desktop asks where
to save the archive (the downloads folder, `teamcraft-export-<date>.zip`); the browser offers it as a
download when it is complete.

The export is done by this device's core (the desktop shell, or the core in the browser page) beside
its other work, like the background history pages: one bounded step at a time (a request, a page of
up to 100 messages, one file), so sends and new messages go on meanwhile.

1. **Account.** What the server holds about the user that it can read (`GET /me/personal-data`,
   §2), the account data of the key bag without its keys (settings, saved messages, drafts' text,
   saved searches; e2ee.md §6.4), and the account's security log (`GET /me/security-log`, paged).
2. **Conversations**, one after the other, in the order of their IDs: those the user is a member of
   when the export starts.
   - The history is first read from the server down to its beginning, page by page, into the local
     store (the same pages and the same record as scrolling up; `older_history.rs`). A conversation
     whose history the server only gives in part (joined later with history from joining on) is
     marked `is_history_limited`.
   - Then the messages are written from the store in display order, a page of the feed at a time with
     the thread replies and reactions of its messages. A conversation is cut into parts of 5,000
     messages.
   - After each part, the files the user sent in it are downloaded, checked against their hash,
     decrypted and written, one per step.
3. **Index and manifest.**

Memory stays bounded by a page of messages and one file (at most 64 MiB, the largest file that goes
in). The browser hands the archive to a `Blob` in pieces of 1 MiB, which the browser keeps (and may
move to disk); the page's buffer is wiped before it is reused.

**Network failures** wait and retry (5 s, doubling to 5 min, at once when the connection is back);
the card says it waits. A failure retrying won't fix (the disk is full, the server refuses) ends the
export and deletes what it wrote.

**Resuming (desktop).** After every whole entry of the archive the file is synced to disk and a
checkpoint is written into the encrypted local store: what was written, where the next step starts,
the counts. If the app stops in the middle, the card offers **Continue** (the partial file is cut back
to the checkpoint's length and the export goes on from there) or **Delete what was saved**. The
browser can't continue after a reload (the pieces went with the page); its checkpoint is deleted on
the next look.

**Privacy.**

- The archive is plaintext. The card says so before the export starts. The desktop writes it only to
  `<chosen file>.part` next to the file the user picked, renamed when complete; nothing goes to a
  temporary folder. A cancelled or failed export deletes the partial file at once, and a discarded one
  too; a rename that fails deletes it as well. Signing out ends a running export and deletes an
  unfinished one; quitting the app keeps it, to continue after the next start.
- No key is ever written: not the identity or device keys, not conversation content keys, not the
  files' keys, not the trust records of the key bag.
- Deleted messages, and those past the workspace's retention, are not on the server and not in the
  store, and are not brought back. Messages this device has no key for are listed with the metadata
  the server gave (kind `unreadable`), without text.
- Nothing of the content is logged: the log has counts, stages and error kinds.
- Other people's files are listed by name, type and size, not downloaded, in either scope.

### 1.1 Archive format, version 1

A ZIP file. Every entry is stored (not compressed) with a data descriptor; archives above 4 GiB or
65,535 entries use ZIP64. Names are UTF-8. Everything is below one folder:

```
teamcraft-export/
  index.html                                    for people: what is where, one row per conversation
  manifest.json                                 written last: every other file with size and SHA-256
  conversations.json                            one summary per conversation
  account/personal-data.json                    the server's answer of GET /me/personal-data
  account/account-data.json                     settings, saved messages, drafts, saved searches
  account/security-log.ndjson                   what happened to the account, newest first
  conversations/<conversation-id>/messages-0001.ndjson
  conversations/<conversation-id>/messages-0002.ndjson   (5,000 messages per part)
  files/<conversation-id>/<attachment-id>/<file name>     the user's own files
```

IDs are the server's (decimal strings); times are Unix milliseconds (UTC).

**`manifest.json`**

| Field            | Meaning                                                                               |
|------------------|---------------------------------------------------------------------------------------|
| `format`         | `teamcraft-data-export`                                                               |
| `version`        | `1`; a field that changes meaning raises it, new fields don't                         |
| `created_at_ms`  | when the export started (the device's clock)                                          |
| `user_id`        | whose export it is                                                                    |
| `scope`          | `own` (the user's messages) or `all` (every message the device could read)            |
| `includes_files` | whether the user's files were chosen                                                  |
| `max_file_bytes` | the largest file that goes in (64 MiB)                                                |
| `counts`         | `conversations`, `messages`, `unreadable_messages`, `files`, `files_missing`          |
| `files`          | `{path, size, sha256}` of every other entry of the archive                            |
| `missing_files`  | `{conversation_id, attachment_id, file_name, reason}`: `too_large`, `gone`, `damaged` |
| `not_included`   | what the archive can't hold, as `code: explanation`                                   |

**`conversations.json`**: `[{id, kind, name, parts, messages, files, is_history_limited}]`; `kind`
is the server's (`channel`, `dm`, `group_dm`, …), `name` the channel's or the other participants'.

**`messages-NNNN.ndjson`**: one JSON object per line, in the conversation's order; a thread's replies
follow the message they answer. Absent fields are empty.

| Field                                | Meaning                                                                           |
|--------------------------------------|-----------------------------------------------------------------------------------|
| `id`                                 | the server's message ID (absent on lines the device derived)                      |
| `client_message_id`                  | the sender's ID of the message (128 bit, base64url)                               |
| `thread_id`                          | for a reply: the thread, which is the ID of the message it answers                |
| `sender_id`, `sender_name`, `is_own` | who wrote it                                                                      |
| `sent_at_ms`                         | when the server received it                                                       |
| `kind`                               | `text`, `file`, `event`, `unreadable` or `other` (a kind of a newer app)          |
| `text`                               | the text as shown, after the last edit                                            |
| `is_edited`, `earlier_versions`      | `[{text, replaced_at_ms}]`, oldest first                                          |
| `mentions`                           | user IDs the message mentions                                                     |
| `priority`                           | `important` or `urgent`                                                           |
| `reply_to`                           | `{client_message_id, author_id, excerpt}` of the quoted message                   |
| `forwarded_from`                     | the original author's name of a forward                                           |
| `link`                               | the address of the link preview                                                   |
| `files`                              | `[{attachment_id, name, media_type, size_bytes, path}]`; `path` is in the archive |
| `reactions`                          | `[{emoji, user_ids}]` as they stand                                               |
| `is_pinned`                          | pinned in the conversation                                                        |
| `event`                              | what an event says (joined, renamed, …), as the app keeps it                      |
| `unreadable`                         | why there is no text: `waiting`, `not_available` or `failed`                      |

With the scope `own`, a part holds only the user's messages (no events), with every reaction to them.
A file's `path` is absent for other people's files, files not chosen and files above the limit; a path
that is listed in `missing_files` didn't make it into the archive.

**`account-data.json`**: `{settings, saved_messages, drafts, saved_searches,
recovery_passphrase_set_at_ms, recovery_kit_made_at_ms}`. `settings` are the synced settings as the
apps write them (`{value, version, writer}` per name, e2ee.md §6.4).

**`security-log.ndjson`**: the server's audit entries about the account (`GET /me/security-log`):
`{id, actor_user_id, action, target_kind, target_id, detail, created_at}`, metadata only.

The format makes the counterpart possible later (erasure, Art. 17): every record names the server's
IDs of the conversations, messages and files it came from.

## 2. Server-held personal data

`GET /api/v1/me/personal-data` answers everything the server holds about the caller that it can
read, in one answer (`teamcraft-proto` `PersonalDataResponse`, version 1). It reads through the same
services the single endpoints use, so it shows exactly what they show the caller, for every role, and
nothing of anybody else. A section the caller's role may not read is absent.

| Field                   | Source                                                                         |
|-------------------------|--------------------------------------------------------------------------------|
| `account`               | ID, username, display name, kind, role, organization, created, deactivated     |
| `profile`               | `GET /me/profile`                                                              |
| `privacy`               | `GET /me/privacy`                                                              |
| `notification_settings` | `GET /me/notification-settings`                                                |
| `email_digest`          | `GET /me/email-digest`                                                         |
| `holiday_region`        | `GET /me/holiday-region`                                                       |
| `devices`               | `GET /devices`: names, push channel kind (never the token), when added         |
| `spaces`                | `GET /spaces`                                                                  |
| `conversations`         | `GET /conversations` with the caller's membership (role, read position, mutes) |
| `scheduled_messages`    | when and where; the size of the sealed text, not the text                      |
| `key_bag`               | that the encrypted key bag exists, its version and size; never its ciphertext  |

Held by the server and not in this answer: the ciphertext of messages and files (the client exports
what it can decrypt), the audit entries about the account (paged apart, `GET /me/security-log`), and
what admins did as the actor of the instance's audit log.

## 3. GDPR articles and features

| Article                      | Feature                                                                     |
|------------------------------|-----------------------------------------------------------------------------|
| Art. 15 (access)             | Personal export; `GET /me/personal-data`; Settings > Security shows devices |
| Art. 20 (portability)        | The archive: JSON and NDJSON, documented above, versioned                   |
| Art. 16 (rectification)      | Profile and name in Settings > Account (where the provider allows)          |
| Art. 17 (erasure)            | Account deletion by an admin (admin.md §3.2.1); self-service: not yet       |
| Art. 5(1)(e) (storage limit) | Retention policies (admin.md); deleted content is not exported              |

## 4. Compliance export and legal hold

An admin can't export content: the server has none in plaintext, and admins hold no keys (admin.md).
An organisation that must be able to hand conversations to an auditor or a court therefore needs a
member that keeps them: the **vault node** (e2ee.md §10.2, §11). Its design is below (§4.1); legal
hold (§4.2) works without it and is built.

Without the vault node:

- an admin's own personal export holds only what that admin can read, like anyone's;
- there is no way, for an admin or the server operator, to export other people's messages;
- a legal hold keeps the ciphertext on the server, so the vault (later) or a member's own device
  (now) can still read what the hold kept.

### 4.1 Compliance archive (design, issue #211; not built yet)

**Who holds the keys.** Only the vault node. It is a TeamCraft device of a dedicated **archive
account** (a system account, MLS credential type `system:archive`), running `teamcraft-vault` on the
organisation's own hardware: the shared core (`teamcraft-core`) without a UI, with its own encrypted
store. It joins conversations like a member device and receives the content keys through MLS like any
member (e2ee.md §4.3). Admins, owners and the server operator hold no archive keys; whoever runs the
vault's machine does, which is why it is on company hardware and not on the VPS.

**Turning it on.** Only by the signed workspace policy (`compliance_archive`):

| Field     | Meaning                                                                                    |
|-----------|--------------------------------------------------------------------------------------------|
| `mode`    | `off` (default) or `on`; the server refuses `on` until the vault node ships                |
| `archive` | the archive account's user ID and the fingerprint of its identity key, as the owner pinned |
| `covers`  | `all` conversations (DMs too), or the IDs of the spaces whose channels it covers           |

The fingerprint is in the owner's signature, so the server can't swap in another "archive" device: a
member app adds only devices of that account whose identity key matches. Every member app shows a
line in each covered conversation when archiving starts or stops; the server can't switch it on
silently (admin.md §2.3).

**What member apps do.** In a covered conversation they add the archive account's devices (the
workspace admins' signature on the policy is the authority, e2ee.md §4.6), share the history with
them, and refuse to send while the archive is missing from the group for more than a short grace
(the composer says "Waiting for the archive"), so the archive is complete. They show a banner
"Messages in this conversation are archived by your organization", from the verified policy, which
the server can neither hide nor fake. Guests are told before they join a covered conversation.

**Exporting.** An admin asks for an export in the console: which conversations or people, which
period, and a reference. A second admin approves it (`exports.approvals_required`, default 2, in the
signed policy). Both are signed with the admins' device keys; the server only relays the request.
The vault verifies the signatures against the verified roster and the policy itself, then writes an
archive in the format of §1.1 (`scope`: `compliance`, with the request in `manifest.json`),
encrypted to the recipient key named in the request, and hands it out through the server (which sees
its size, never its content). Whether the people concerned are told is a policy setting
(`exports.notify_people`, default on).

**What the server learns.** That archiving is on and which account and devices archive (they are
members like any other); export requests: who asked, who approved, the scope, the reference, when
it ran and how large the result was. Never content, never keys, never the recipient's private key.

**Audit.** Turning archiving on or off, every export request, approval, refusal and completion,
and every legal hold placed or released are entries of the audit log (admin.md §4), metadata only.

**Retention on the archive.** The vault applies its own retention to its copy (the policy's
`retention`, plus a separate archive period), and a legal hold suspends it there too.

### 4.2 Legal hold (implemented)

A legal hold keeps a conversation: what retention, the inactivity policy, a deletion or a member's
deletion of their own message would remove stays on the server until the hold is released. Holds
are part of the signed policy (`compliance_archive`), placed and released by owners in
Administration > Retention. A hold names a conversation (`legal_holds`) or a person (`person_holds`,
issue #2135).

| Field             | Meaning                                                                      |
|-------------------|------------------------------------------------------------------------------|
| `conversation_id` | the held channel, direct message or group; the server refuses meeting chats  |
| `reference`       | the organisation's case reference, 1 to 64 characters, read by every member  |
| `placed_at`       | when the owner placed it, by the owner's clock; shown only                   |

A person's hold has `user_id`, `reference`, `placed_at` and `scope`: `all` (the default, left out of
the JSON) holds every channel, direct message and group the person is in, `direct` only their
direct messages and groups. Meeting chats go with their meeting, which a hold can't stop, so neither
a hold by name nor a person's hold covers them.

At most 500 holds, conversations and people together, each conversation and each person once. A new
hold by name must name an existing channel, direct message or group, a person's hold a known account.
The server and the owners' apps refuse other values; an unknown `scope` is refused when signing and
read as `all` by apps, so a newer app's narrower hold never shows as no hold.

**What a person's hold covers.** On the server: the conversations the person is a member of, the
direct messages and groups opened for them (their participants, which stay when an account is
deleted), and the conversations they leave while the hold lasts (recorded when the membership goes:
leaving, removal, account deletion; `legal_hold_departures`). Releasing the hold forgets those. A
conversation the person left before the hold isn't covered. The server keeps the held people beside
the policy entry (`legal_hold_people`), written in the transaction that stores the entry, and a
deletion reads both under its lock (below). On a member's device: the direct messages and groups
whose participants include the person, and the conversations whose member list (as the device knows
it) names them; the banner comes from one shared function of the core
(`teamcraft_core::workspace::legal_holds`), which every app reads.

Owner apps before person holds re-sign the policy without `person_holds`, which releases them; each
release is an audit entry, so it doesn't go unnoticed.

**What a hold does on the server.**

- Retention skips the conversation: no message, attachment or search index segment of it is deleted.
- The inactivity policy may archive it but doesn't delete it.
- Deleting the channel, deleting its space, and the last member's leave that would delete a private
  channel are refused with `legal_hold`; the apps say why. Leaving a whole space with
  `delete_if_last` goes through, but a held private channel it would delete stays, without members.
- Deleting an account keeps the held direct messages and groups that would go with it, with their
  ciphertext, attachments and MLS state; the remaining participant still sees the conversation with
  the deleted user. Once the hold is released, the account housekeeping deletes such a direct message
  like one a deletion left behind (issue #1066); a group DM stays.
- **A member's deletion of their own message.** The app sends the encrypted delete as before and
  names the withdrawn message in the request (`deletes`, its client message ID): metadata only, the
  server learns which of the sender's messages was withdrawn, not what it said. Without a hold the
  server deletes that message's ciphertext at once. Under a hold it marks it deleted (`deleted_at`)
  and keeps it: history no longer lists it, so devices see the same as without a hold. When no hold
  keeps the conversation any more, the server removes the marked messages: right after the policy
  entry that releases a hold, and with every retention run, so a restart, a release stored on another
  instance or a failure catches up. Only a message the caller sent counts; another user's is ignored.
  The request is part of the queued message, so a deletion made offline arrives with it and is
  judged by the holds when it arrives: a hold placed meanwhile keeps the message. A repeated request
  (a retry, another device) changes nothing more. Edits and reactions stay until retention, as the
  server can't tell which message they belong to; apps before this send no `deletes`, and their
  deletions leave the ciphertext until retention, as before.
- A hold and a deletion at the same moment can't both win (#2143). Appending a policy that places a
  hold share-locks the conversations it holds by name and those the newly held people are in, and
  checks the named ones still exist, in the transaction that stores the entry; every deletion (a
  channel, a space, a direct message with a deleted account, a withdrawn message, the removal of
  withdrawn messages after a release) locks the rows it deletes and only then reads the newest
  policy and the held people, in its own transaction. Retention does the same for each conversation
  before it deletes anything of it, and deletes the objects in the storage only for rows deleted under
  that check. Whichever comes second sees the other: the hold is refused for the deleted conversation
  (a person's hold just doesn't cover it), the deletion is refused with `legal_hold`, the message is
  marked instead of deleted, or retention skips the conversation.
- Releasing the hold returns it to the normal rules; the next retention run catches up.

**Who is told.** Every member of a held conversation sees a banner "This conversation is under legal
hold (reference)." that also says a message they delete stays on the server until the hold is
released. It shows the same for a person's hold that covers the conversation, with that hold's
reference. On desktop and web it is `LegalHoldNotice`; on Android and iOS the conversation screen
draws it from `SecuritySnapshot.legal_holds`, which the apps read with the chat snapshot (the same
verified policy, with person holds already expanded by the core), and a deletion the apps know to
be held is refused at once with the `legal_hold` text (`TeamCraftError::LegalHold`; the server's
refusal maps to it too). It comes from the verified policy, so the server can't show it for a
conversation nobody held; like any policy update, it can delay or withhold a newer policy (a new
hold or a release) from a member, and who is in a conversation is the server's word, as every
member list is. Holds are not
secret: the signed policy is readable by every signed-in user, which is why the console asks for a
reference and not a name or a reason (a person's hold names its account, as the roster does).

**Audit.** Every hold placed or released is an audit entry (`legal_hold_placed`,
`legal_hold_released`) with the signing owner, the conversation or the person (target `user`, with
the scope), the reference and the policy version. Removing withdrawn messages after a release is an
entry `legal_hold_deletions_applied` per conversation, with how many went.

**Limits.** The server is untrusted: a hostile server can delete ciphertext whatever the policy says.
A hold protects against the routine deletions, not against the operator; the vault's own copy (§4.1)
is what protects against that. The console offers the channels and the people; it doesn't list other
people's direct messages (admin.md §2.1), so a direct message is held through a person's hold with the
scope `direct`, or by its ID through the API.

### 4.3 Audit log CSV

Administration > Audit log > **Export** downloads the log as a CSV file (`audit-log-<date>.csv`;
the desktop app saves it to the downloads folder). Admins and owners only (`ReadAuditLog`).

- **What is in it.** What the view shows, from the whole log and not only the loaded page: the actor
  and the period are applied by the server, the action and the search text by the console on each
  page. The search text matches the raw entry (action name, target, detail), not the names the
  table resolves. Columns: `time` (UTC, ISO 8601), `actor_user_id`, `actor` (display name, empty
  if unknown), `action` (the server's action name), `target_kind`, `target_id`, `detail` (JSON
  text). Metadata only, like the log.
- **Paged, not loaded.** The console reads `GET /api/v1/admin/audit` newest first, one page at a
  time (`before` is the cursor), and writes each page into pieces of a `Blob` that the browser
  holds; nothing holds the whole log in memory. A server page that claims more but doesn't advance
  stops the export with an error.
- **Spreadsheets.** Cells are quoted per RFC 4180. A cell that starts with `=`, `+`, `-`, `@`, a tab
  or a return gets a leading `'`, so a hostile detail text or name is shown, not run as a formula.
  The file starts with a UTF-8 byte order mark so spreadsheets read umlauts.
- **Audit.** Before the first page is read the console calls `POST /api/v1/admin/audit/export`,
  which writes an `audit_exported` entry with the actor and the filters (action name, period,
  whether a search text was used; the text itself is not recorded). A failed record stops the export.
- **Failure.** The button is a `PendingAction`: disabled while it runs, a failure (network, refusal)
  is an error toast and no file is offered.

### 4.4 What remains

| What                                                          | Issue |
|---------------------------------------------------------------|-------|
| Vault node, archive mode and approved export requests (§4.1)  | #2134 |

## 5. Manual test steps

Desktop and web, with a test account in a few conversations, one of them older than what the device
holds (scroll up to see that it loads more), one file sent by the account and one by somebody else.

- [ ] Settings > Account > Your data: "Only mine", files on, **Export my data…**. The desktop asks
      where to save; cancel the dialog: nothing starts. Again, save: the bar moves through the stages,
      the app stays usable (send a message meanwhile).
- [ ] Open the archive: `index.html` lists the conversations, links work; `manifest.json` lists every
      file; `sha256sum` of a file matches its entry. The oldest message of the long conversation is in
      its part; only the account's messages; its file is under `files/`, the other person's is only
      named.
- [ ] "Every message I can read": other people's messages are in, with reactions and thread replies.
- [ ] Turn the network off during the export: the card says it waits; on again: it goes on and
      completes.
- [ ] **Stop export** in the middle: the card says it stopped; no `.part` file is left next to the
      target (desktop).
- [ ] Desktop: quit the app in the middle of a large export, start it again: the card offers
      Continue. Continue: the archive completes and opens. Again, and **Delete what was saved**: the
      `.part` file is gone.
- [ ] Desktop: export to a folder that is full or read-only: the card says the file couldn't be
      written, no `.part` file stays.
- [ ] Web: reload during an export: the card shows the start again, nothing is left.
- [ ] German: the card and the archive's `index.html` are in German.

Legal hold (an owner's account on desktop or web, a member's account in a second browser):

- [ ] Administration > Retention > Legal hold: place a hold on a channel with a reference. An empty or
      too long reference shows its error at the field and nothing is signed.
- [ ] The member opens the channel: the banner names the reference. Release the hold: the banner goes.
- [ ] With the hold in place, deleting the channel or its space is refused with "under legal hold".
- [ ] Android and iOS: the member opens the held channel: the banner names the reference, in English
      and in German. Delete the channel from its menu: "This conversation is under legal hold." Release
      the hold: the banner goes without reopening the channel.
- [ ] Administration > Audit log lists "Legal hold placed" and "Legal hold released" with the owner.
- [ ] Place a hold on a person (A person, All their conversations): the table says what it covers.
      The member, in a channel and a direct message with that person, sees the banner in both; in a
      conversation without them, none. Change to "Their direct messages and groups": the channel's
      banner goes, the direct message's stays.
- [ ] With the person held, the person deletes one of their messages in the direct message: it
      disappears for both as usual. Reload: it stays gone. Release the hold: nothing changes for
      the members; the audit log lists "Held deletions applied".
- [ ] With the person held, delete the other participant's account: the direct message stays for
      the held person, with the deleted user. Release the hold: within the next housekeeping round
      it goes.
- [ ] Offline: turn the network off, delete an own message in a held conversation, turn it on: the
      deletion goes through and the message stays hidden.

Audit log CSV (an admin's account on desktop or web):

- [ ] Administration > Audit log > Export: a file `audit-log-<date>.csv` is saved; the button is busy
      meanwhile. Opening it in a spreadsheet shows umlauts correctly.
- [ ] With an actor and a period picked, the file holds only those rows; the log then lists "Audit log
      exported".
- [ ] Offline: Export shows an error toast and offers no file.
