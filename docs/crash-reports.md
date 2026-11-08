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
  screenshot, view hierarchy, threads, session replay, tracing, profiling or sessions, and no
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
`enableAutoPerformanceTracing` and `enableAutoSessionTracking` off; no tracing, profiling or
replay (their defaults); `enableAppHangTracking` and `enableMetricKit` on. `beforeSend`
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
