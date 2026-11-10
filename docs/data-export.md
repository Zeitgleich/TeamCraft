# Data export (issue #211)

Two exports are asked of a chat that keeps personal data: a person's copy of their own data (GDPR
Art. 15 and 20), and an organisation's copy of conversations for auditors or courts (compliance,
legal hold). TeamCraft's server only holds ciphertext of messages and files (e2ee.md §4.4), so it
can produce neither alone: content can only be exported by a client that holds the keys.

| Export                             | Who                       | Status                                                     |
|------------------------------------|---------------------------|------------------------------------------------------------|
| Personal export ("Export my data") | the user, on their device | Implemented on desktop and web (this document)             |
| Server-held personal data          | the user, through the API | Implemented: `GET /api/v1/me/personal-data`                |
| Compliance export, legal hold      | admins, through a vault   | Not implemented: needs the vault node (e2ee.md §11)        |
| Audit log CSV                      | admins                    | Not implemented (docs/guide/admin/users-devices-guests.md) |

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

## 4. Compliance export (not implemented)

An admin can't export content: the server has none in plaintext, and admins hold no keys (admin.md).
The design is in e2ee.md §11: when the signed workspace policy turns `compliance_archive` on, a vault
node (`teamcraft-vault`, a member device on company hardware with the reserved credential
`system:archive`) is added to every covered conversation, receives the content keys through MLS like
any member, and exports EML or JSON per conversation, with legal hold suspending its retention.
Members see a banner that the conversation is archived. Without it:

- an admin's own personal export holds only what that admin can read, like anyone's;
- there is no way, for an admin or the server operator, to export other people's messages.

What remains (issue #211): the vault node and its export job API (admin-approved, optional two-person
rule, an audit entry per export with its scope), legal hold, the audit log CSV export, and the
member-visible banner.

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
