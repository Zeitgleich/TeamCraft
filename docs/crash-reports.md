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
  (vendor-neutral rule). The app only knows a **DSN** (`https://<public key>@<host>/<project>`).
- **Default backend: GlitchTip** (decided 2026-11-08): Postgres + Valkey, errors plus uptime checks.
  It has a built-in MCP server and a Sentry-style API, so the developers' tools can read and triage
  issues directly. Bugsink (smaller, errors only) and Sentry self-hosted (heavy) take the same DSN.
- **Deployment:** an optional service of [teamcraft-deploy](https://github.com/Zeitgleich/teamcraft-deploy)
  (`glitchtip_enabled`). The operator creates an organization and a project in GlitchTip and pastes
  the project's DSN into Administration → Crash reports.

### Who builds the reports

| Platform          | Builds and sends                                                    | Why                                                   |
|-------------------|---------------------------------------------------------------------|-------------------------------------------------------|
| Desktop, server   | `teamcraft_client_api::crash_reporting` (Rust panics)               | One scrubber for every Rust process; no SDK defaults  |
| Web (wasm + page) | The same module, from the wasm panic hook and `window.onerror`       | No browser SDK: no JS bundle, no download size        |
| Android           | sentry-android (JVM, NDK, ANR) + the core module for Rust panics    | NDK and ANR capture need the platform SDK             |
| iOS               | sentry-cocoa (crashes, hangs, MetricKit): follow-up issue (needs Mac) | Same reason                                           |

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

The Android SDK adds what it needs for its crashes: device model and OS version, ANR thread dumps,
NDK registers and the loaded libraries' debug IDs, and its own scrubbed breadcrumbs (below).

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

sentry-android is configured from the core's config and rules: `sendDefaultPii = false`,
`attachScreenshot = false`, `attachViewHierarchy = false`, no session replay, no automatic HTTP or
UI breadcrumbs, no user. Its `beforeSend` removes the message's quoted text and URL queries, drops
`user` except the installation ID, `request`, `server_name` and `extra`, and its breadcrumbs come
from the core's redacted ring only.

## Installation ID

A random 128-bit ID, made on first use and stored on the device. It is the report's `user.id`, so
the backend counts "installations affected" and crash-free rates; it is never derived from the
account, the device or the instance, and it is rotated when the user turns reports off and on again.

## Settings

- **Instance (admin):** Administration → Crash reports: on/off, DSN, sample rate for non-fatal
  errors (crashes are always sent), environment name. Stored as an instance setting in the
  database (`settings.crash_reports`, a runtime settings group) and served to the apps in
  `GET /api/v1/server-info` (`crash_reports`, absent while off), so store apps
  work for every instance without a rebuild. Off, or no DSN: nothing is captured or sent, and
  queued reports are dropped.
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
  `Retry-After`; a backend's refusal is a `400` and the report is dropped. sentry-android sends its
  JVM, NDK and ANR reports to the DSN directly (no CSP there).
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

## Symbols: readable stack traces

| Platform         | What the report carries                       | Symbol upload (release workflow)                       |
|------------------|-----------------------------------------------|--------------------------------------------------------|
| Desktop, server  | Function names, resolved on the device       | None needed for names; see below for lines              |
| Web (wasm)       | Function index and offset (names are stripped) | The unstripped `.wasm` kept as a release artifact      |
| Android (Kotlin) | Obfuscated names (R8)                         | R8 `mapping.txt` → GlitchTip (ProGuard)                |
| Android (NDK)    | Addresses and debug IDs of `libteamcraft_ffi.so` | The unstripped `.so` (ELF debug files) → GlitchTip    |
| iOS              | Addresses and debug IDs                       | dSYMs → GlitchTip (follow-up issue)                     |

- **Rust on desktop and server** resolves frames in the process (`std::backtrace`): release builds
  keep their symbol table, so function names are there; file and line need debug info in the
  binary (`debug = "line-tables-only"`, a few MB more per binary) and are left out for now. Sending
  addresses plus debug images for server-side symbolication would need the `sentry` crate's
  debug-image code; not worth it while function names suffice.
- **wasm** (`profile.web-release` strips the name section to save download size): a browser frame
  is `wasm-function[1234]:0x5678`. GlitchTip doesn't symbolicate wasm; the release workflow keeps
  the unstripped module as an artifact and `wasm-tools addr2line` (or `llvm-symbolizer`) maps a
  frame by hand. Keeping the name section in the shipped module would cost download size against
  the budget.
- **Android:** GlitchTip accepts R8/ProGuard mappings and ELF debug files through its debug file
  API (`glitchtip-cli debug-files upload`, compatible with `sentry-cli`). Older GlitchTip versions
  failed to apply mappings uploaded without a `kind` (GlitchTip backend issue 504); check the
  deobfuscation on the first deployed instance.
- **Upload is optional.** The release workflow uploads when the secrets `CRASH_REPORTS_URL`,
  `CRASH_REPORTS_ORG`, `CRASH_REPORTS_PROJECT` and `CRASH_REPORTS_AUTH_TOKEN` exist and skips the
  step otherwise, so forks and other operators build without them.

## Store declarations

- **Play data safety** (apps/android/README.md, "Google Play"): "Crash logs" and "Diagnostics"
  collected, optional, not shared, not linked to the user, not used for tracking.
- **App Store privacy label** (with the iOS wiring): "Crash Data" and "Other Diagnostic Data", not
  linked to the user, not used for tracking.

## Status

| Part                                                         | State                            |
|--------------------------------------------------------------|----------------------------------|
| This design, the core module (config, scrub, queue, panic)   | Done (first pull request)        |
| Panic hooks (desktop, server, web, mobile core), JS errors   | Next                             |
| Admin setting, served in `GET /api/v1/server-info`, relay    | Done (second pull request)       |
| User opt-out, first notice                                   | Next                             |
| Android SDK, symbol upload in the release workflow           | Next                             |
| iOS (sentry-cocoa, dSYMs)                                    | Follow-up issue (needs a Mac)    |
| GlitchTip in teamcraft-deploy                                | teamcraft-deploy                 |
