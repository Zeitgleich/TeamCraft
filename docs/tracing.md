# Tracing

Issue #1640. Every action that runs a store query, a server request, a gateway or push event, an
FFI call or a background job is recorded as a span, and a slow or failed one reaches the crash
report backend (GlitchTip) with the spans below it, so a hang is explained from the data: which
step, how long it **waited** versus **ran**, and what it waited on.

This note builds on [crash-reports.md](crash-reports.md) (format, backend, scrubbing, settings,
"Performance data"). It replaces hand-written timing at each site with one framework: spans of the
`tracing` crate, written at a handful of chokepoints, collected by one layer.

## Why `tracing` spans

- The code already logs through `tracing`; every client and the server install a subscriber.
- A span's name and its field names are `&'static str` in the callsite's metadata: names come from
  the code by construction.
- A span knows its parent, and the registry tells when a future was polled: entered (**busy**) and
  not entered (**idle**). For a queued job the span is made when the job is queued and entered when
  it runs, so one span answers "waited in the queue" (creation → first entry), "worked" (busy) and
  "waited on something else" (idle after the first entry: network, a lock, a channel).
- A callsite nobody listens to costs one atomic load; a span that is created but not kept costs no
  allocation (the registry pools its slots).

## Where spans are written (chokepoints)

Spans are written at `trace` level, so log layers (which stop at `info`) don't pay for them; the
trace layer's own filter takes them at any level.

| Chokepoint                                         | Span (`op`)                            | Description                               | Fields            |
|----------------------------------------------------|----------------------------------------|-------------------------------------------|-------------------|
| Every HTTP request of the core (`send_traced`)     | `http.client`                          | `GET /api/v1/users/{id}`                  | `status`, `error` |
| Reading an API answer's body                       | `http.body`                            | `http.body`                               | `body_bytes`      |
| Each gateway event received and applied            | `gateway.event`                        | the event's kind (`message_create`)       | `error`           |
| Mobile FFI engine queue (every engine call)        | `queue.engine`                         | the job's function (`send_message`)       | `depth`           |
| Mobile FFI read queue, open chat's read thread     | `queue.read`, `queue.query`            | the job's function (`messages`)           | `depth`           |
| Mobile FFI query of the app                        | `db.query`                             | `query_<kind>` (`query_feed_extras`)      | –                 |
| Desktop shell: a request of the page               | `queue.shell`                          | the request's function                    | –                 |
| Store: wait for a group lock, a write group        | `store.lock_wait`, `store.write_group` | `store_lock`, `write_group`               | –                 |
| Named steps (`timing::step`, about 70 places)      | `core.step`                            | the step's name (`catch_up_conversation`) | –                 |
| Server: every request (axum middleware)            | `http.server`                          | `GET /api/v1/users/{id}` (matched route)  | `status`          |
| Server: every SQL statement (sqlx's statement log) | `db.sql`                               | the statement, literals removed           | `rows`            |

Gateway connect, resume and catch-up, push handling (`push_notification`, `push_preview`,
`push_catch_up`) and the other named stretches are `core.step` spans. Every FFI call reaches the
core through the engine or a read queue, so the queue spans are the FFI's span per call.

`timing::step("name")` stays the one-line way to name a stretch of work; it now makes a span.
New instrumentation adds a span at a chokepoint, not a call at each caller.

## The layer (`teamcraft_client_api::crash_reporting::trace`)

One `tracing_subscriber::Layer`, installed by every process next to its log layers (web page,
desktop shell, mobile FFI, server). It listens to spans of TeamCraft's crates (and `sqlx::query`
events on the server) and does four things:

1. **Times** each span: start, queued (creation → first entry), busy and idle, and the CPU time of
   the thread while entered where the platform tells it (`cpu_ms`, Android and Linux).
2. **Keeps** a ring of the last 512 closed spans: the measures of crash-reports.md take their
   steps from it (`trace::recent`), and a crash report takes the last 30 as breadcrumbs.
3. **Tracks open spans** (at most 256): a crash, an ANR or an app hang report carries the spans that
   were still open and how long, which is where it hung.
4. **Decides at the end (tail-based)** whether a root span (one without a parent) becomes a
   transaction of its own:
   - **failed or slow** (longer than `slow_job_ms`): always, at any trace rate above 0, with all
     its descendants. The only limit is against a storm of the same one (every request failing
     while offline): at most 10 per description a minute.
   - **anything else** of at least 5 ms: sampled at the instance's trace rate, the same
     `trace_percent` the measures use (100 %: every one), at most 20 a minute.

   The transaction holds its descendants (at most 31, the longest) and, when it waited, the spans
   that overlapped its wait on the same queue (`blocked_by`, at most 8): so #1466 reads as
   "`queue.read messages` queued 6.2 s, blocked by 40 × `queue.read conversations`".

### Sampling: one admin setting

The instance traces at 100 % on purpose while the team develops (full detail is the point) and is
meant to go down to 1–5 % later. That is only Administration → Crash reports → `trace_percent`;
no build or code change. Every process follows a change without a restart: the web page and
desktop with their next settings refresh, the mobile apps when they start the crash SDK with the
new setting, the server within a minute. Slow and failed work stays fully captured at any rate
above 0; 0 turns performance data off.

### Data volume

Sizes measured (`a_kept_trace_is_a_few_kilobytes`): a typical kept trace (a queue job with its
request, body and a few steps) is about 2.5 KB, a full one (31 descendants, 8 that blocked it)
about 11.5 KB, a measure with its steps about 4 KB. One envelope per request, uncompressed.

Per active user and day (the app in use for a working day), **estimated** from the counts the
backend saw so far (about 170 measures per user and day) and an assumed 1,500 roots of 5 ms or
more (queue jobs, gateway events, shell requests; requests inside a job are its children, not
roots). GlitchTip's own counts per release replace the estimate after a week of data.

| Rate  | Sampled roots  | Slow or failed | Measures   | Requests to the backend | Data per user and day |
|-------|----------------|----------------|------------|-------------------------|-----------------------|
| 100 % | 1,500 × 2.5 KB | ~30 × 6 KB     | 170 × 4 KB | ~1,700                  | ~4.6 MB               |
| 5 %   | 75 × 2.5 KB    | ~30 × 6 KB     | 9 × 4 KB   | ~115                    | ~0.4 MB               |

The caps bound the worst case whatever the estimate: 20 sampled roots a minute and 10 slow or
failed per description a minute, so a client that is busy for 8 hours sends at most about 9,600
sampled traces (~24 MB) a day; the relay's per-address limit (30 requests a minute) and its `429`
pause both queues before that. The server's own traces (`http.server` with SQL, about 3 KB) share
the same caps per instance: at 100 % a busy instance sends up to 20 a minute (~85 MB a day at
the cap), at 5 % about one a minute plus its slow and failed requests.

### What a span may carry (privacy)

The layer is the scrubber: it keeps only what can't be content, whatever a caller writes.

- **Names:** the span's name (the `op`) is static. Its description is a static name or a field
  that passes a shape check: a code name (`[a-z0-9_.]`, at most 48 characters) or a route
  template (below). Anything else becomes `other`.
- **Route templates:** method plus path with every segment that is not a lowercase word replaced
  by `{id}` (numbers, base64, hex, UUIDs, tokens, anything with upper case or digits), the query and
  fragment dropped, and the meeting-link and share-link segments always `{token}`. A server span
  uses axum's matched route instead.
- **Numbers:** kept only for field names that say they are a measure (`*_ms`, `*_bytes`, `status`,
  `attempt`, `depth`, `rows`, `count`) and never for `*_id`; booleans for `is_*` and `error`.
- **Strings, `Debug` values and errors:** dropped, except the two shapes above. An error becomes
  `status = internal_error` and the error's class name from the code (`error = "timeout"` passes
  the code-name check), never its message.
- **SQL (server):** the statement's summary as sqlx gives it, with string and number literals
  replaced by `?`. Statements come from the code; bind values are never part of it.
- Interned descriptions are bounded (1024 distinct per process; later ones become `other`).

A test feeds spans with message text, names, file names, e-mail addresses, IDs, tokens and paths in
every field kind and asserts that the transaction's JSON contains none of them
(`trace.rs`, `no_span_field_can_carry_content`).

### Following a request from the client to the server

The client sends `sentry-trace: <trace id>-<span id>` (no sampling flag: each side decides at its
own end) with each API request; the server's request span continues that trace. A slow request
then shows in GlitchTip as the client's `http.client` span and, with the same trace ID, the
server's `http.server` transaction with its SQL statements. Trace and span IDs are random, not
derived from anything the user did.

### Settings and cost

- **Off means off:** while crash reports are off for the instance or the user opted out, the layer
  says it is not interested and spans cost one atomic load. Turning reports on starts collection
  without a restart.
- **Not kept:** a span that is neither slow nor failed nor sampled costs its timing and a ring
  slot; no allocation per field, no formatting.
- **Bounded:** ring 512, open spans 256, descriptions 1024, transaction 32 spans; the queue and the
  flushes are those of crash-reports.md ("Sampling and sending": 50 transactions, 256 KiB, sent
  with the crash reports' flushes, never by a timer; a flush goes on while the backend takes what
  it sends, so a full rate doesn't pile up).
- **No wakeups:** the layer has no thread and no timer; the mobile shells send with their flushes.
- **Measured** (`a_span_costs_little`, release build, x86-64): a span costs about 0.1 µs with
  collection off and 0.85 µs with it on. A channel switch or a send makes about ten spans (queue
  job, steps, store waits, request and body), so about 10 µs against budgets of 100 ms: no change
  to robustness.md's budgets.

## Platforms

| Process      | Installs the layer       | Follows the settings                         | Sends kept traces                                                                |
|--------------|--------------------------|----------------------------------------------|----------------------------------------------------------------------------------|
| Web page     | `teamcraft-ui` `main.rs` | The page's `CrashReporter`                   | The page, with its flushes (`take_kept`)                                         |
| Desktop      | Shell `main.rs`          | The page tells the shell (`perf_configure`)  | The page takes the shell's trees (`perf_traces`), checked again on arrival       |
| Android, iOS | FFI `logging.rs`         | `trace_start` / `trace_stop` with the SDK    | `PerfListener::on_traces_kept` → `take_trace_envelopes` → the SDK's envelope API |
| Server       | `logging.rs`             | The instance setting, looked at every minute | Its own sender task (`spawn_trace_sender`)                                       |

On mobile the core builds the whole envelope (scrubbed here, like on every platform) and the SDK
sends it as it is: sentry-android `InternalSentrySdk.captureEnvelope`, sentry-cocoa
`SentrySDK.internal.envelope` (the hybrid-SDK entry points), with the SDK's own queue, retries and
rate limits. The measures' steps (`perfJobSpans`, `perf_spans`, the web's ring) come from the same
spans, so a measure shows `http.client` and `store.lock_wait` steps next to the queue jobs.

## Status

| Part                                                                   | State                      |
|------------------------------------------------------------------------|----------------------------|
| This design                                                            | Done                       |
| The layer: timing, ring, open spans, scrub, tail sampling, breadcrumbs | Done                       |
| HTTP client, gateway events, store waits, named steps                  | Done                       |
| FFI queues and queries, Android and iOS shells, web and desktop        | Done; iOS: device check    |
| Server request and SQL spans, trace propagation                        | Done                       |
| Push as spans of their own (`push.receive`, `push.show`) in the shells | Later: named steps for now |
