# Crash reports

Issue #1508. Crashes and errors reach the team without anybody pressing "Report a problem"
([bug-reports.md](bug-reports.md)): panics, uncaught exceptions, JVM and NDK crashes, ANRs, and a
sample of handled errors, with a stack trace, the app version and the platform. Self-hosted only:
no Firebase, no SaaS. The instance's operator decides where reports go, and nothing is sent until an
admin sets that up.

This note states the format, what a report contains and what it never contains, how it is scrubbed,
how sending stays out of the app's way, and how stack traces become readable (symbols).

## Format and backend

- **Wire format: the Sentry envelope protocol** (`POST /api/<project>/envelope/`). It is the
  de-facto open standard; several self-hosted backends accept it, so the backend is replaceable
  (vendor-neutral rule). The app only knows a **DSN** (`https://<public key>@<host>/<project>`), from its server.
- **Default backend: GlitchTip** (decided 2026-11-08): Postgres + Valkey, errors plus uptime checks.
  It has a built-in MCP server and a Sentry-style API, so the developers' tools can read and triage
  issues directly. Bugsink (smaller, errors only) and Sentry self-hosted (heavy) take the same DSN.
- **Who runs it:** by default the publisher of the apps (like Crashlytics for a store app): its DSN
  is built into the server, and every instance's apps report there unless the instance's admin
  sends them to an own backend or turns them off (Settings, below).
- **Deployment:** an optional service of [teamcraft-deploy](https://github.com/Zeitgleich/teamcraft-deploy)
  (`glitchtip_enabled`, also on a host of its own). Create an organization and a project in
  GlitchTip; the project's DSN (Settings → Client Keys) goes into the build variable (publisher)
  or into Administration → Crash reports (own backend).

### Who builds the reports

| Platform          | Builds and sends                                                    | Why                                                   |
|-------------------|---------------------------------------------------------------------|-------------------------------------------------------|
| Desktop, server   | `teamcraft_client_api::crash_reporting` (Rust panics)               | One scrubber for every Rust process; no SDK defaults  |
| Web (wasm + page) | The same module, from the wasm panic hook and `window.onerror`       | No browser SDK: no JS bundle, no download size        |
| Android           | sentry-android (JVM, NDK, ANR) + the core module for Rust panics    | NDK and ANR capture need the platform SDK             |
| iOS               | sentry-cocoa (crashes, app hangs, MetricKit)                        | Same reason                                           |

The Rust `sentry` crate was not taken: its defaults collect what this note forbids (host name,
loaded modules, environment variables, `SENTRY_*` overrides) and it has no browser transport, so the
web would still need its own path. The envelope is three JSON lines; building it in the shared
module keeps one code path, one scrubber and one set of tests for desktop, server and web, and adds
no dependency (`serde_json` and `url` are already there). A browser SDK (`@sentry/browser`, tens of
KB gzipped) would also break the web download budget (robustness.md) for no gain.

## What a report contains

| Field                | Content                                                                             |
|----------------------|-------------------------------------------------------------------------------------|
| Event ID             | 16 random bytes                                                                     |
| Time                 | When it happened (device clock; only for display, never for ordering)               |
| Level                | `fatal` (crash), `error` or `warning` (handled, sampled)                            |
| Error                | Type (`panic`, `TypeError`, …) and the **scrubbed** message, at most 512 characters  |
| Stack trace          | Function names, source files inside their crate (`teamcraft-core/src/runner.rs`),   |
|                      | line and column, whether the frame is TeamCraft's own; at most 100 frames           |
| Breadcrumbs          | The last 50 lines of the redacted debug log ring (bug-reports.md, "The debug log")  |
| Release, environment | `teamcraft@<version>` and the admin's environment name (`production`)                |
| Platform             | desktop, web, android, ios or server; OS name and version                          |
| Installation ID      | A random ID per installation, so a crash's reach can be counted (below)             |

The platform SDKs add what they need for their crashes: device model and OS version, ANR and app
hang thread dumps, NDK registers and the loaded libraries' debug IDs, MetricKit diagnostics on iOS,
and the core's log lines as breadcrumbs, scrubbed (below).

## What is never sent

- Message content, drafts, file names, conversation or channel names, people's names, e-mail
  addresses, keys, tokens, passwords, push payloads, URLs with their query or fragment.
- A user ID, account, workspace name, device name, host name, IP address (the backend sees the
  sender's address as any web server does; GlitchTip doesn't store it unless configured to).
- Screenshots, view hierarchy, session replay, network bodies, UI text, environment variables.

The report type has **no field** for any of these (no `user` beyond the installation ID, no
`server_name`, `request`, `extra` or free-form context), so a caller can't add them by accident.

## Scrubbing

Every report passes `crash_reporting::scrub` before it is queued, on top of security.md (nothing
logs content) and the debug log's own redaction:

- **Quoted text goes whole:** `"…"` becomes `"[redacted]"`. That is where a `Debug` struct or a
  `{:?}` puts its values (`Person { name: "…" }`), whatever they look like.
- **Then the debug log's redaction by shape** (bug-reports.md, "Redaction"): URLs keep scheme,
  host and path, query and fragment go; bearer tokens, JWTs, secret `key=value` pairs and long hex
  or base64 runs go; e-mail addresses and paths become `[email]` and `[path]`.
- **Source paths lose the build machine:** a frame keeps the path inside its crate, Cargo package
  or the standard library (`/home/<user>/…/crates/x/src/a.rs` becomes `x/src/a.rs`), anything else
  only its file name; a script URL only its path.
- **Lengths are cut:** message 512, type 128, function 256 and breadcrumb 512 characters; 100
  frames (the innermost), 50 breadcrumbs (the newest).
- Breadcrumbs run through the same steps again, though the ring redacted them already.

A test builds a report from a session full of secrets (message text, a person's name, a file name,
a JWT, a password, key material, an e-mail address, a home directory, presigned and share URLs) and
asserts that the envelope contains none of them (`scrub.rs`,
`a_report_from_a_session_with_secrets_contains_none_of_them`).

**Residual risk:** a panic message built with `format!` from a bare, unquoted value
(`panic!("no member {name}")`) can't be told from the developer's words. Such messages are a bug
under security.md already (no content in logs or errors); reviews look for them.

### The Android SDK

sentry-android (`sentry-android-core` and `sentry-android-ndk`, flavor `full` only; the `libre`
build carries no SDK and shows no switch) captures JVM crashes, ANRs and NDK crashes. Auto-init is
off in the manifest and its start-up providers are removed; `crash/CrashReports.kt` starts it with
`SentryAndroid.init` only while the signed-in server's `crash_reports` is set and the user didn't
opt out (or the instance makes reports mandatory), and closes it when that ends.

- **Where the setting comes from:** after every sign-in and on every return to the foreground (at
  most once a minute), the app fetches `GET /api/v1/server-info` of its server and hands it to the
  FFI's `crash_reports_config`, which checks it with `CrashReportingConfig::from_info` (an
  unusable DSN or environment means off). Offline, or an answer that isn't a `200`, keeps the last
  known setting. The setting is cached in app-private preferences (never backed up), so a crash
  early in the next start is reported; changing the server forgets it.
- **Options:** release `teamcraft@<version>`, the instance's environment, no personal data
  (`dataCollection`: no user info, cookies, headers, query parameters, bodies or file paths), no
  screenshot, view hierarchy, threads, session replay, profiling or sessions, tracing only at the
  trace rate ("Performance data"), and no
  automatic breadcrumbs (activity, app and system events, network, user interaction, app
  components); `beforeBreadcrumb` drops whatever else would add one. NDK and ANR capture are on.
- **`beforeSend`** (`SentryEventScrub`): handled errors are sampled with the instance's rate
  (crashes and ANRs always go; the SDK's own `sampleRate` is unset because it would sample crashes
  too); `request`, `server_name`, `extra` and `logger` go; `user` is only the installation ID
  (also set on the scope, which the NDK side syncs); exception values and the message pass
  `CrashScrubber`, a port of the rules above (quoted text, then the debug log's redaction by
  shape); the breadcrumbs are the last 50 lines of the core's redacted ring (`recent_log_lines`),
  scrubbed again. A JVM test builds an event from a session full of secrets and asserts that the
  serialized event contains none of them (`SentryEventScrubTest`).
- **Symbols:** with `-Pteamcraft.proguardUuid=<uuid>` the `fullRelease` variant gets a generated
  asset `sentry-debug-meta.properties` (`io.sentry.ProguardUuids=<uuid>`); without it nothing is
  generated. See "Symbols" for the upload.

### iOS

sentry-cocoa (Swift Package Manager, product `Sentry`, the app target only; the notification and
share extensions report nothing) is wired the same way: `TeamCraft/Model/CrashReports.swift` starts
it with `SentrySDK.start` from the app delegate (the last known setting) and after every sign-in
and return to the foreground (`server-info` through the same FFI check), and closes it when
reports go off. Options: `sendDefaultPii`, `attachScreenshot`, `attachViewHierarchy`,
`enableAutoBreadcrumbTracking`, `enableNetworkBreadcrumbs`, `enableCaptureFailedRequests`,
and `enableAutoSessionTracking` off; tracing (app start, screen loads, frames) only at the trace
rate, without network, file or Core Data spans ("Performance data"); no profiling or replay;
`enableAppHangTracking` and `enableMetricKit` on. `beforeSend`
(`CrashEventScrub`) and `CrashScrubber.swift` do what the Android side does, with the same test
cases (`CrashScrubberTests`). The settings and the notice are the same as on Android.

## Installation ID

A random 128-bit ID, made on first use and stored on the device. It is the report's `user.id`, so
the backend counts "installations affected" and crash-free rates; it is never derived from the
account, the device or the instance, and it is rotated when the user turns reports off and on again.

## Settings

- **Where reports go (admin):** Administration → Crash reports → "Crash reports go to":
  - **The publisher's service** (default): the backend of whoever built the apps and the server,
    like the push gateway. The release workflow builds its DSN into the server image (repository
    variable `TEAMCRAFT_CRASH_REPORTS_DSN`); the console names its host. A build without one (a
    fork, a build from source) sends nothing in this mode.
  - **An own backend:** the instance's own DSN (GlitchTip or any Sentry-protocol backend).
  - **Nowhere:** crash reports are off.

  Plus the sample rate for handled errors (crashes are always sent) and the environment name.
  Stored as an instance setting in the database (`settings.crash_reports`) and served to the apps
  in `GET /api/v1/server-info` (`crash_reports`, absent while nothing is sent), so store apps work
  for every instance without a rebuild. Nothing to send: nothing is captured, and queued reports
  are dropped.
- **Mandatory or optional (admin):** "People may turn crash reports off" (on by default). Off:
  reports are mandatory on this instance; the privacy switch stays on, disabled, and says that the
  organization requires them. The operator then needs a legal basis of its own (for example
  legitimate interest under GDPR art. 6(1)(f)) and tells people in its privacy policy; reports
  carry no personal content either way (above).
- **Notice (admin):** "Tell people on the first start" (on by default). On: the first start after
  the admin enabled reports shows a short notice once, with a link to the privacy switch. Off for
  an operator who informs people another way (privacy policy, onboarding).
- **User:** Settings → Privacy → "Send crash reports" (on by default when the admin enabled them,
  unless the instance makes them mandatory), with a link to this page. Turning it off drops the
  queued reports at once and rotates the installation ID when turned on again.

## Sending and robustness

- **Queued first.** A report is built and queued at once (a panic hook can't wait for the
  network); the shell stores the queue (desktop: a file next to the store, written in the hook;
  mobile and web: the local store) and sends it later.
- **Through the app's own server.** The Rust apps post envelopes to
  `POST /api/v1/crash-reports` of their TeamCraft server, which forwards them to the DSN in the
  instance settings (8 s timeout, public https addresses only, at most 256 KiB, the public rate
  limit). The web client's content security policy allows only its own origin, the desktop page
  only the shell; the backend sees the server's address instead of every user's; and the apps need
  no second origin through proxies and captive portals. The server learns nothing new: reports are
  scrubbed on the device. Off: `404 crash_reports_off`, and the app drops its queue. The backend's
  `429` comes back as `429` with its `Retry-After`; a backend that is down or fails is a `503` with
  `Retry-After`; a backend's refusal is a `400` and the report is dropped. sentry-android and
  sentry-cocoa send their reports to the DSN directly (no CSP there), with their own offline cache.
- **Bounded.** At most 20 reports and 512 KiB; the oldest drop out. A report older than 7 days or
  failed 8 times is dropped.
- **A dead backend never slows the app.** Sends run in the background with a 10 s timeout
  (`SEND_TIMEOUT`). A flush stops at the first failure, so an unreachable backend gets one request
  per flush, never a storm. Failed reports back off from 30 s, doubling, up to 6 h. A `429` (or
  `503` with `Retry-After`) pauses all sends for the time the backend asks. Other `4xx` drop the
  report.
- **No idle traffic.** There are no timers: the shells flush on start, after a capture and when the
  connection comes back. An app that doesn't crash sends nothing.
- **Sampling.** Fatal reports are always sent; handled errors are sent with the admin's sample rate.

### Lost sign-ins

A sign-in that the user didn't end and that the app loses is reported as a handled warning of type
`LostSignIn` (desktop and web page, `auth::report_lost_sign_in`), so "I have to sign in all the
time" shows up without a bug report. The message is one of a few fixed texts naming the path: the
provider refused the stored or refresh token (`invalid_grant`), the web page was signed in before
and has no saved sign-in any more, its saved sign-in can't be read (IndexedDB failed after
retries), or the DPoP key of the sign-in is gone. The breadcrumbs (the redacted debug log) show
what led there. At most three per page life, sampled like other handled errors; it is queued before
the page goes to the identity provider and sent on the next start.

### Storage failures

Every IndexedDB failure of the page (`teamcraft_core::store::IdbFailureHook`) is a handled warning
of type `IndexedDbError` with the operation and the `DOMException` name (`request: UnknownError`),
never the exception's message, which may name a database or a key. `VersionError` (a newer app
upgraded the database) and `ConstraintError` (a key another page stored first) are expected and
not reported. At most five per page life; the service worker reports none.

## Processes without a page

- **Desktop shell** (the core, calls, the shell's own code): its panic hook writes the panic
  (message, location, backtrace, the shell's redacted log lines) to `crash-reports-pending.json` in
  the app's data directory, at most five, synchronously (the process aborts right after). The page
  takes them on its next start (`crash_reports_take_native`) and reports them like its own, with the
  instance's setting and the user's choice, which the shell doesn't know.
- **The server** reports its own panics straight to the backend in force (publisher's or own):
  built and scrubbed in the hook, sent from a thread of its own (a panicking request never waits
  for the backend), at most 20 per process and one per place in the code. No breadcrumbs: the
  server keeps no debug log ring. Platform `server`, a random installation ID per start.
- **Mobile core:** a Rust panic in the FFI reaches Kotlin or Swift as an exception, which the
  platform SDK reports (with its `beforeSend` scrubbing).

## Performance data

Issue #1521. How long the operations people wait for take on their devices, as Sentry
`transaction` items to the same backend as crash reports (GlitchTip → Performance), so the
budgets in robustness.md ("Performance and resources") are checked against real use, per platform
and release. Where to read them: [performance.md](performance.md), "Numbers from real use".

### What is measured, and what each measure answers

A duration alone doesn't find a bug; each measure carries the steps and tags that say why it was
slow. The yardstick is #1466 (a chat open waited seconds behind forty chat list reads on the read
thread) and the Android fixes #1431, #1428 and #1375: each of them must be visible in the data.

| Transaction             | From → to                                                                   | Apps                       | Answers                                                                                                                         |
|-------------------------|-----------------------------------------------------------------------------|----------------------------|---------------------------------------------------------------------------------------------------------------------------------|
| `start.channel_list`    | Process start (cold) or activity create (warm) / page navigation → chat list shows the core's rows | All                        | How long a start takes, by `start` (cold, warm) and `entry` (launcher, notification, share, link); `first_frame` step and `start_cache` (hit, miss) say whether the first frame showed the cached list (#1431, #1428); `activities` counts copies of the activity (#1375); status `internal_error` when the chat list couldn't be read |
| `channel.switch`        | Opening a conversation → its first read is shown (rows or empty state)      | All                        | Where a slow open spent its time: `source` (seed, memory, store) and the core's jobs as steps (below)                           |
| `conversation.catch_up` | Opening a conversation, or the app back in the foreground, while it is behind → the newest server message is stored and shown | All | "The push came, the chat lacks it", even when the switch was fast from the seed; `source` (push, open, foreground), `gap_fill`, `resync` |
| `history.older_page`    | A page of older history asked for from the server while the reader scrolls back → its rows show | All | Whether scrolling back waits on the server (`core.action load_older_history`, the core's `older_history_page` step) or stalls the list: web and desktop add the refresh that reads the rows (`ui.refresh`) and the frame that shows them (`ui.render`, measurement `render_ms`) |
| `feed.scroll_back`      | The reader's first scroll up in a conversation → scrolling rests (at least a screen back) | Web, desktop | Whether scrolling back is smooth: `longest_frame_ms`, `frames_over_50ms`, and how often the list made up for rows above the reader in the top spacer (`spacer_corrections`) or by moving the scroll position under them (`scroll_corrections`, what ended a wheel's smooth scrolling in #1595) |
| `message.send`          | Sending a plain message → the server's message replaced the pending row     | All                        | Send latency, with the engine's jobs as steps                                                                                   |
| `sync.reconnect`        | The core connecting again → connected                                       | All                        | How long reconnects take; `link` = `slow`                                                                                       |
| `file.download`         | An attachment's download and decryption                                     | Web, desktop               | Download speed per `rendition`, `bytes`                                                                                         |
| `call.setup`            | Joining a call → its media secured                                          | Web, desktop               | Call setup time                                                                                                                 |
| SDK: app start, `ui.load` | Process start → first frame; a screen until displayed; slow/frozen frames | Android, iOS               | The platform's view of the same start                                                                                           |

**The core's jobs as steps (mobile).** Everything the apps ask of the core runs on two serial
queues: the read thread (local store) and the engine thread (network, sync, sending). The FFI
records every job (`perf_jobs.rs`): its function's name in the code (`conversations`, `messages`,
`send_message`), when it was queued, started and done, and how many jobs were queued with it.
The local store adds its waits for another thread's group of writes (`store.lock_wait`) and how long
a group of writes held it (`store.write_group`, `timing.rs`). When a measure ends, the jobs that
overlapped it become its steps: `queue.wait` (queued → started, with `teamcraft.queue_depth`) and
the run (`core.read`, `core.engine`, `store.lock_wait`, `store.write_group`), at most 24, the longest.
So #1466 reads in GlitchTip as: `channel.switch` 6.4 s, of which `queue.wait messages` 6.2 s at
queue depth 41, next to forty `core.read conversations` steps. A test reproduces it
(`reads.rs`, `a_chat_open_behind_a_flood_names_what_it_waited_for`).

**Slow jobs as warnings.** A job that waits or runs longer than the instance's threshold
(Administration → Crash reports → `slow_job_ms`, 1000 ms by default, 0: never) is sent as a warning
event `Slow core job: <name>`, grouped per job, tagged with `job`, `queue`, `wait_ms`, `run_ms` and
`queue_depth`, sampled like handled errors; at most one per job a minute and 20 per process. It
shows slow jobs that no measure was open for (a background sync holding the read thread).

**Named steps inside the core (all platforms).** `teamcraft_core::timing::step("name")` times the
rest of a function; about seventy places carry one, among them: store open and catalog build
(`store_open`, `catalog_build`), client open and MLS state (`client_open`, `mls_storage_load`,
`mls_group_load`, `mls_storage_save`), the chat list and snapshot (`chat_list_build`,
`to_conversation`, `entry_counts`, `flush_entry_tallies`, `account_data_read`,
`snapshot_read_full`, `conversation_view`), the gateway and sync (`gateway_connect`,
`gateway_fresh_sync`, `full_sync`, `read_ahead_fresh`, `resync_marked`), catch-up
(`catch_up_conversations`, `catch_up_conversation`, `catch_up_read_first_pages`, `mls_catch_up`,
`mls_process`, `mls_decrypt_batch`, `catch_up_messages`, `history_gap_fill`, `join_welcomes`),
sending and groups (`send_entry`, `seal_message`, `commit_change`, `post_commit`), files
(`file_upload`, `file_download_decrypt`, `file_encrypt`, `thumbnail_make`, the Files view's page
`files_page`), push
(`push_notification`, `push_preview`, `push_catch_up`) and upkeep. Steps under 5 ms are not kept;
until a measure asks, a step costs one atomic load. Mobile gets them through the FFI with the queue
jobs; the web page reads its own core's ring; the desktop shell times each request of the page
(named by its method, `messages`) and hands its ring to the page (`perf_core_steps`), which keeps a
name only if it looks like code (`[a-z0-9_]`, at most 48 characters).

Not yet measured: uploads and MLS commits as transactions of their own (they show as steps). A start that showed a sign-in page first is not a signed-in
start and is not measured. A measure the user moved on from is dropped, not sent as slow.

### What a transaction contains

- Its name (above), the protocol's `op`, start and end time, status (`ok`, `internal_error`,
  `cancelled`, `deadline_exceeded`, `unavailable`).
- Steps (spans, at most 32; the core's jobs at most 24): an operation and a description from the
  code (`app.start` / `store-read`, `db.query` / `core_messages`, `queue.wait` / `conversations`),
  their times and status; on mobile the queue depth and queue name as span data, nothing else.
- Tags (at most 8) and measurements (at most 8) from the code: `source`, `account`, `start`,
  `entry`, `start_cache`, `gap_fill`, `resync`, `rendition`, `link`; `bytes`, `activities`.
- Release, environment, the platform tag, the OS name and the installation ID, as in a report.

**Privacy by construction:** in `crash_reporting::Transaction`, the name is an enum, and span
operations and descriptions, tag names and values and measurement names are `&'static str`: there
is no field a caller could fill with text at run time, so a transaction can't carry a message, a
name, a file name, an ID or a URL. The page keeps measures that start and end in different places
by a local key (a conversation's or a pending message's ID), which never leaves the page. No
breadcrumbs. The tests check the shape and the bounds (`transaction.rs`) and that the relay
forwards transactions and nothing else besides events.

On Android and iOS the SDKs build transactions; their `beforeSendTransaction` (Android:
`SentryTransactionScrub`) or `beforeSend` (iOS: `CrashEventScrub.prepareTransaction`) drops the
request, server name, extras and breadcrumbs, keeps only the installation ID as user, scrubs tag
values, and on Android scrubs span descriptions ([CrashScrubber]: a URL keeps scheme, host and
path) and drops span tags and span data (full URLs, queries, file paths) except the core jobs'
queue depth and queue name. iOS turns network, file and Core Data tracing
off, so no span names a URL or a file; Android has no HTTP integration (the app's HTTP runs in the
core). The Android measures of its own (`Perf.kt`) take their names from the core
(`perf_measure_name`), so every platform reports the same transaction names.

### Sampling and sending

- **Trace rate (admin):** Administration → Crash reports → "Share of measured operations sent as
  performance data", `trace_percent` in the setting and in `server-info`. **100 % by default for
  now**: the instance has a handful of users and the team needs the data; meant to be lowered as
  instances grow, without a rebuild. An older server's `server-info` has no `trace_percent`: no
  performance data. Only where crash reports are on and the user didn't opt out (or reports are
  mandatory).
- **Decided at the start** of an operation (`CrashReporter::should_trace`): an unmeasured
  operation costs a random number.
- **A buffer of its own:** at most 50 transactions and 256 KiB, 3 tries, 2 days; the oldest drop
  out. Crash reports have their own queue, so performance data never pushes one out, and a flush
  sends crash reports first.
- **No timers, no idle traffic:** transactions go with the crash reports' flushes (start, after a
  capture, back online) and when 5 are queued; at most 5 per flush, one request each (the protocol
  takes one transaction per envelope), so the relay's per-address rate limit
  (`public_requests_per_minute`, 30 by default) keeps room for crash reports. A `429` pauses both
  queues. The page keeps the buffer in memory and stores it in `localStorage` when hidden or
  closed.
- **Relay:** `POST /api/v1/crash-reports` checks each item's type and forwards `event` and
  `transaction` items only (attachments, sessions, replays: `400`), same size limit.

## Symbols: readable stack traces

| Platform         | What the report carries                          | Symbol upload (release workflow)                 |
|------------------|--------------------------------------------------|--------------------------------------------------|
| Desktop, server  | Function names, resolved on the device           | None needed for names; see below for lines       |
| Web (wasm)       | Function index and offset (names are stripped)   | None (gap, below)                                |
| Android (Kotlin) | Obfuscated names (R8)                            | R8 `mapping.txt` under the build's ProGuard UUID |
| Android (NDK)    | Addresses and debug IDs of `libteamcraft_ffi.so` | The unstripped `.so` (ELF debug files)           |
| iOS              | Addresses and debug IDs                          | dSYMs from the Xcode archive (#1515)             |

- **Rust on desktop and server** resolves frames in the process (`std::backtrace`): release builds
  keep their symbol table, so function names are there; file and line need debug info in the
  binary (`debug = "line-tables-only"`, a few MB more per binary) and are left out for now. Sending
  addresses plus debug images for server-side symbolication would need the `sentry` crate's
  debug-image code; not worth it while function names suffice.
- **wasm** (`profile.web-release` strips the name section to save download size): a browser frame
  is `wasm-function[1234]:0x5678`, and GlitchTip doesn't symbolicate wasm. To read one, build the
  release's commit with `strip = false` (the build is reproducible) and map the frame with
  `wasm-tools addr2line` or `llvm-symbolizer`. Keeping the name section in the shipped module would
  cost download size against the budget; panics carry their message and location either way.
- **Android:** the release workflow (`release.yml`, job `android`) gives the full release a fresh
  ProGuard UUID (`-Pteamcraft.proguardUuid`, written to the app's `sentry-debug-meta.properties`)
  and uploads `mapping.txt` under it (`sentry-cli upload-proguard`) and the unstripped libraries
  of `merged_native_libs/fullRelease` (`sentry-cli debug-files upload`). `sentry-cli` is pinned by
  version and SHA-256. GlitchTip takes both through its Sentry-compatible debug file API; older
  GlitchTip versions failed to apply mappings uploaded without a `kind` (GlitchTip backend issue
  504), so check the first deobfuscated crash.
- **Upload is optional.** The workflow uploads when the secrets `CRASH_REPORTS_URL`,
  `CRASH_REPORTS_ORG`, `CRASH_REPORTS_PROJECT` and `CRASH_REPORTS_AUTH_TOKEN` exist and skips the
  steps otherwise, so forks and other publishers build without them. A failed upload warns; the
  release goes on.

## Store declarations

- **Play data safety** (apps/android/README.md, "Google Play"): "Crash logs" and "Diagnostics"
  collected, optional, not shared, not linked to the user, not used for tracking.
- **App Store privacy label** (with the iOS wiring): "Crash Data" and "Other Diagnostic Data", not
  linked to the user, not used for tracking.

## Status

| Part                                                         | State                            |
|--------------------------------------------------------------|----------------------------------|
| This design, the core module (config, scrub, queue, panic)   | Done (first pull request)        |
| Web and desktop page: wasm panics, script errors             | Done (third pull request)        |
| Panic hooks of the desktop shell and the server              | Done (fourth pull request)       |
| Admin setting, served in `GET /api/v1/server-info`, relay    | Done (second pull request)       |
| User opt-out, first-start notice (web and desktop page)      | Done (third pull request)        |
| Android SDK, symbol upload in the release workflow           | Done (fifth pull request)        |
| iOS (sentry-cocoa), settings and notice                      | Written; Mac build: #1515        |
| iOS dSYM upload, device test                                 | Next (#1515)                     |
| GlitchTip in teamcraft-deploy                                | teamcraft-deploy                 |
| Performance data: web, desktop, Android, iOS SDK (#1521)     | Done; iOS untested (#1515)       |
| Core job steps, slow-job warnings, catch-up (#1521 review)   | Done; iOS built in CI only       |
| Named core steps on every platform                           | Done                             |
| Uploads and MLS commits as transactions of their own         | Next                             |
