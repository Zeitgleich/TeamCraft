# Bug reports ("Report a problem")

Issue #1362. People run into problems, most often in meetings, and a description alone rarely tells
the team enough to fix them. "Report a problem" collects what the app knows about its recent life,
shows the person exactly that, and sends it with their description to the people of the
organization who look after TeamCraft. This note states where reports go, what they contain, what
the server learns and how the log stays free of content and secrets.

## Where a report goes: an encrypted group DM

A report is an **ordinary end-to-end encrypted message** in the group DM of the reporter and the
**report recipients**: the description as the message text, the report itself as a text file
(`teamcraft-report-<date>.txt`) and, if the person adds one, a screenshot. Nothing new crosses the
server: it stores a message and files it can't read, like any other DM.

- **Recipients** are part of the signed workspace policy (`bug_reports.recipients`, admin.md §2.3),
  so a hostile server can't redirect reports to an account of its choosing. Owners set them in
  Administration → Security → Problem reports. With none set, reports go to the owners and admins of
  the verified roster: a new workspace needs no setup. An owner app from before the setting that
  signs the policy again drops the list, which means "owners and admins" until an up-to-date owner
  signs again.
- **The reporter is left out** of the recipients (an admin reporting a problem writes to the other
  recipients); a workspace where nobody else would receive it refuses the report in the dialog
  ("Nobody is set up to receive problem reports"), before anything is queued.
- **Recipients see reports as DMs**, one conversation per reporter, with the usual notification. They
  read, ask back in the same conversation, and forward to the developers by hand where that helps
  (e.g. a GitHub issue written by a person, without data the reporter wouldn't want there). Nothing
  ever goes to GitHub or any other service automatically: TeamCraft has no dependency on a tracker.
- **What the server learns:** that the reporter has a group DM with these people and sent a message
  with one or two files at that time, and the files' sizes. Not that it is a report, not its content.

The alternatives were a server endpoint (the server would read the report, or need a new encryption
scheme for it) and a fixed "support" channel (every reporter would see every other report). The DM
reuses the outbox, uploads, retries and the E2EE that already exist and keeps reports apart.

## What a report contains

The person sees the whole report text before sending ("What will be sent"), with the screenshot
optional and off until they add one:

| Part             | Content                                                                                       |
|------------------|-----------------------------------------------------------------------------------------------|
| Description      | What the person typed (1 to 4000 characters); the message text                                |
| App              | App version, platform (desktop, web, Android, iOS), OS and browser as the platform names them |
| Connection       | Whether the app is online and connected to the server                                         |
| Call (if in one) | Call and conversation ID, kind, secured, reconnecting, limited network (TCP fallback), the     |
|                  | number of participants and their user IDs, own mute/camera/screen state                       |
| Log              | The recent log lines of this device (below), oldest first                                     |
| Screenshot       | Optional: an image the person picks (PNG or JPEG, up to 10 MiB)                               |

Names of participants, conversation names and message content are never part of it: IDs only.

## The debug log

Every app keeps the recent `tracing` events of its own process in a **bounded ring in memory**
(`teamcraft_core::debug_log`), never on disk:

- **What:** `info` and above from TeamCraft's own crates, `warn` and above from dependencies. Each
  line is a UTC time, the level, the module and the event's message with its fields.
- **Bounds:** 500 lines by default (`DebugLogSettings::max_lines`), each cut at 1024 characters, so the
  ring holds at most about 0.5 MB. Older lines drop out. A restart starts empty.
- **Where:** desktop: the shell process (core, calls) plus the page; web: the page; Android and iOS:
  the core in the app (the same ring the diagnostics export of #1094 shares).

### Redaction

security.md already forbids logging content, file names, keys, tokens, passwords and push payloads.
The ring does not rely on that alone; every line passes a redaction step before it is stored:

- **Fields by name:** a field whose name says it holds content or a secret (`body`, `text`,
  `content`, `message_text`, `file_name`, `name`, `title`, `email`, `token`, `password`,
  `passphrase`, `secret`, `key`, `signature`, `payload`, `url`, `query`, `cookie`, `authorization`,
  `code`, and names ending in `_key`, `_token`, `_secret`, `_name`) is written as `[redacted]`.
  Numbers and booleans are always kept: they are counts, sizes, durations and flags.
- **Values by shape**, in the message and in every remaining field:
  - URLs keep scheme, host and path; query and fragment become `?[redacted]` (presigned upload links
    and share links carry their secrets there).
  - `Bearer …` / `Basic …`, JSON Web Tokens (`eyJ….….…`) and `key=value` pairs whose key names a
    secret become `[redacted]`.
  - E-mail addresses become `[email]`.
  - Long runs of hex or base64 (24 characters or more, which is what keys, hashes and tokens look
    like; IDs are decimal) become `[redacted]`.
  - File paths (`/home/…`, `C:\…`, `~/…`) become `[path]`: they carry user and file names.
- Tests feed events with message bodies, tokens, keys, URLs with signatures, e-mail addresses and
  paths through the ring and assert none of them comes out.

The redaction is defence in depth. The person still sees every line before it is sent and can
decline.

## Sending and robustness

- **Queued first.** "Send" stores the report on the device (`Table::SyncState`, `bug_report/<id>`)
  and closes the dialog with "Report saved: it is sent when you are online" or "Report sent". The
  upkeep every shell runs delivers queued reports: it opens (or finds) the group DM with the
  recipients of the verified policy and puts the message in the outbox with its files. From there it
  is a normal message: sent with retries, shown as pending or failed with "Try again" in the DM.
- **Idempotent.** The report's ID is the message's client message ID. A delivery cut short (crash,
  lost answer) runs again and finds the message already in the outbox; the server deduplicates the
  send on the same ID. Opening the group DM is idempotent on the server (same members, same DM).
- **Bounded.** At most 5 reports wait at once (a sixth is refused in the dialog); a report that can't
  be delivered for 7 days is dropped with a log line. Sizes: description 4000 characters, log 500
  lines, screenshot 10 MiB and the server's upload limit.
- **Offline, restart, process death:** the queue lives in the store, so a report taken offline goes
  out on the next start or connection, like any message.
- **Recipients change while queued:** delivery reads the recipients when it runs, so a report
  queued before a policy change goes to the current recipients.
- **Not applicable:** clock skew (the report's time is shown as the device's time and labeled so;
  ordering comes from the server), multi-device (each device reports its own log).

## Who can report

Every signed-in member, from the account menu ("Report a problem…") and from the call's overflow
menu. Guests of meetings can't: they share no DM with the organization's admins, and their meeting
host can be asked directly.

## Platforms

| Platform | State                                                                                  |
|----------|----------------------------------------------------------------------------------------|
| Desktop  | Dialog with preview, log of shell and page, screenshot picked from a file              |
| Web      | Dialog with preview, log of the page, screenshot picked from a file                    |
| Android  | Screen with preview from Settings → Help, log of the core, screenshot from the gallery |
| iOS      | Core and FFI ready (`send_bug_report`, `bug_report_text`); the screen is a follow-up   |
