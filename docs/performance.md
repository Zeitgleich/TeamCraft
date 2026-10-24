# Performance and scale

What was measured, how to repeat it, and what the numbers mean for the targets in
ARCHITECTURE.md §1.1 ("huge channels stay cheap"). Both tests are `#[ignore]`d integration tests of
`teamcraft-server`; they use the real router, a real Postgres and real WebSocket connections, so
everything includes authentication, JSON and the database round trip.

```sh
export DATABASE_URL=postgres://teamcraft:teamcraft-dev-only@127.0.0.1:5432/teamcraft   # dev stack
cargo test -p teamcraft-server --features integration --test suite perf_history -- --ignored --nocapture
cargo test -p teamcraft-server --features integration --test suite perf_gateway -- --ignored --nocapture
```

Environment of the numbers below: one machine (24 cores, 62 GB RAM, NVMe) running the test (client
and server in one process) and PostgreSQL 18 in a container on the same host, so there is no network
latency in them. Treat them as the cost of the software, not as a promise for your hardware. The
tests assert generous bounds, so a regression fails them.

## A channel with 10 million messages

`perf_history`: 10,000,000 message rows (200 bytes of ciphertext each), spread over nine months
of monthly partitions, in one channel (`TEAMCRAFT_PERF_ROWS` changes the size). The table with its
indexes takes 3.6 GB; seeding takes about nine minutes with plain SQL.

| Operation (through the API)                                    | p50     | p95     | p99     |
|----------------------------------------------------------------|---------|---------|---------|
| Newest page of 50 messages                                     | 6.8 ms  | 7.1 ms  | 7.7 ms  |
| Page 100 pages (5,000 messages) back                           | 7.0 ms  | 7.4 ms  | 8.8 ms  |
| Forward page after a deep cursor                               | 6.2 ms  | 6.6 ms  | 6.9 ms  |
| Newest page for a member who just joined (`from_join` history) | 4.8 ms  | 5.7 ms  | 11.9 ms |
| Unread counts, user never read the channel (10M unread)        | 10 ms   | -       | -       |
| Unread counts, read position at the newest message             | 4.5 ms  | 7.5 ms  | 8.9 ms  |
| Send one message                                               | 10.7 ms | 16.4 ms | 18.7 ms |

All of these are dominated by the request overhead (about 4 ms for authentication and the user
lookup); the queries themselves take 0.3-0.5 ms (`EXPLAIN ANALYZE` is printed by the test). Paging
cost does not grow with the age of the cursor or the size of the table, because every query is an
index range scan on `(conversation_id, id)` that Postgres merges across the monthly partitions.

**Retention:** keeping the newer half of the data (`max_age_days` on the channel) deletes 5.0 million
rows and drops the five emptied partitions in 13.7 s; history paging is as fast afterwards. The job
deletes by ID range per conversation, so it is bounded by the number of rows to delete, and runs in
the background (`[retention]`).

### What the test found and fixed

- **JIT compilation.** The first runs of the unread-count query on a prepared connection took
  0.5 s at 10 million rows (and 50 ms at 3 million). The planner's cost estimate for a query over a
  table with a dozen partitions exceeds Postgres' JIT threshold, so the query was compiled instead
  of executed. The server now connects with `jit = off` (right for an OLTP workload); the same query
  takes 4 ms. If you connect through a pooler that ignores startup options, set `jit = off` on the
  database or the role.
- **Unbounded unread counts.** Counting unread messages for a user who never read a huge channel
  scanned all of it (7.4 s for 10 million). Counts now look at the newest `limits.max_unread_counted`
  (1000) unread messages per conversation; a count equal to the limit means "that many or more".
  Mentions are counted among those messages.

## Gateway fan-out

`perf_gateway`: 500 users with a device and a gateway connection each, all members of one channel;
a bot posts to it at 300 messages per minute for 30 seconds while a second, small channel
(two members) carries a message every half second. Settings: `TEAMCRAFT_PERF_CONNECTIONS`,
`TEAMCRAFT_PERF_BOT_PER_MINUTE`, `TEAMCRAFT_PERF_SECONDS`.

| What                                                                  | p50   | p95   | p99    |
|-----------------------------------------------------------------------|-------|-------|--------|
| Bot message to every one of the 500 connections (75,000 deliveries)   | 52 ms | 61 ms | 126 ms |
| `POST /messages` round trip of the bot (includes publishing to 500)   | 58 ms | 64 ms | 134 ms |
| Message in the small channel to its other member, without bot traffic | 17 ms | 21 ms | -      |
| The same while the bot posts to the 500-member channel                | 12 ms | 62 ms | 64 ms  |

All 75,000 deliveries arrived, none were missing or duplicated. The small channel did not slow down
under the bot's traffic (its p50 stayed where it was; the p95 rise to 62 ms is the same order as one
bot message's fan-out and comes from sharing the process with 500 sockets). Publishing is done on the
request path and costs about 0.1 ms per connection, so a 500-member channel adds about 50 ms to a
send. Delivery happens through in-memory queues per session; a slow client cannot hold up the others
(its buffer is bounded and it resumes or resyncs when it reconnects).

For larger channels the cost grows linearly: 5,000 connected members mean about half a second for
a send on the request path. If that becomes the limit, move publishing off the request path (the
event is already stored before it is published) before adding instances.

## Server process: restarts, outages, stalled clients

`tests/e2e` `server_robustness.rs` runs the real `teamcraft-server` binary against the dev stack and
drives it with `teamcraft-core` clients and raw gateway connections (tests/e2e/README.md has the
setup). It covers the "Server side" conditions of robustness.md; every scenario except the
measurement is a regular test of the e2e suite:

```sh
cargo test -p teamcraft-e2e --features e2e --test suite server_robustness::
E2E_BIN_DIR=target/release cargo test -p teamcraft-e2e --features e2e --test suite \
  server_robustness::resources -- --ignored --nocapture        # memory and CPU
```

Measured in a cloud VM (4 vCPUs, 15 GiB, a kernel without IPv6), release server, Postgres 18 in a
container on the same VM; October 2026, `main` dd957c6 plus the fixes of the same change.

| Scenario                                                      | Result                                                                                       |
|---------------------------------------------------------------|----------------------------------------------------------------------------------------------|
| SIGTERM with gateway connections open                         | Exits in about 10 ms, without waiting for WebSockets; they end without a close frame         |
| After the restart                                             | Clients get `session_invalid`, identify and sync over REST; a message sent meanwhile arrives |
| Resume within the window                                      | Exactly the missed events are replayed                                                       |
| Resume after more events than the buffer holds, or too late   | `session_invalid`, then a full sync                                                          |
| Ten concurrent retries of one send (same `client_message_id`) | One 201 and nine 200 with the same message ID; one stored row                                |
| A late or repeated read position                              | Never moves it back                                                                          |
| Postgres unreachable for a few seconds                        | 503 after `acquire_timeout_secs` (5 s), no restart needed (was 500)                          |
| Server started while Postgres is down                         | Waits up to `startup_wait_secs` (300 s) and comes up with Postgres (exited before)           |
| A client stops reading during a flood (3,000 × 16 KiB)        | Dropped after 1024 queued events; the others get every event; RSS 51 → 106 MiB               |
| A client never reads again                                    | Connection released 23 s after the flood (30 s write deadline; was held indefinitely)        |

During the database outage `/healthz` stays 200 and `/readyz` answers 503; open gateway connections
stay up and keep answering heartbeats, so clients don't reconnect for a database blip.

| Server process (release)                                | Value                                                       |
|---------------------------------------------------------|-------------------------------------------------------------|
| Resident memory after start                             | 31 MiB                                                      |
| With 200 gateway connections                            | 60 MiB, about 150 KiB per connection                        |
| Idle with 200 connections (heartbeats every 25 s), CPU  | 0.17 % of one core                                          |
| 2,000 sends of 1 KiB, each delivered to 200 connections | 6.1 s (326 sends/s, 65,000 deliveries/s), 144 % CPU, 73 MiB |

The memory a slow or stalled connection holds is bounded by count, not by size: 1024 queued events
per connection plus 1000 per session in the resume buffer, each a copy of the event. With 16 KiB
messages that is about 32 MiB per stalled session; at the 1 MiB `max_ciphertext_bytes` it would be
about 2 GiB (#156).

## Video send path

Per-frame cost of sending a 720p camera at 30 fps (33 ms per frame), from
`crates/teamcraft-media/examples/video_bench.rs`:

```sh
crates/teamcraft-media/models/fetch.sh
TEAMCRAFT_SEGMENTATION_MODEL=crates/teamcraft-media/models/selfie_segmentation_landscape.onnx \
  cargo run --release -p teamcraft-media --example video_bench \
  --features openh264-source,segmentation,gstreamer
```

Measured on an AMD Ryzen 9 5900X with a Radeon RX 7800 XT (Mesa radeonsi, VA-API), 120 frames of
a moving test pattern, while other builds kept the machine busy (load average about 29), so treat
the worst cases as pessimistic.

| Stage                                                        | Average  | Worst    |
|--------------------------------------------------------------|----------|----------|
| H.264 simulcast, 3 layers (720p/360p/180p), OpenH264 (CPU)   | 9.5 ms   | 22.2 ms  |
| H.264 simulcast, 3 layers, VA-API through GStreamer          | 15.0 ms  | 45.8 ms  |
| Background blur, light (WGSL compute on the GPU)             | 0.53 ms  | 1.5 ms   |
| Background blur, strong                                      | 0.49 ms  | 0.92 ms  |
| Background image                                             | 0.52 ms  | 1.1 ms   |
| Person segmentation (MediaPipe landscape, tract on the CPU)  | 6.7 ms   | 16.0 ms  |

The hardware row is latency, not CPU time: each layer waits for its picture from the GPU before
the next layer starts, and the CPU is mostly idle meanwhile. What it saves is CPU (and battery);
encoding the three layers in parallel would bring the latency down. The software encoder stays
the fallback when no hardware encoder is usable. Segmentation plus blur and the software encoder
together stay below the 33 ms frame budget on average.

## Clients

How fast and how lean the apps are for the person using them; the budgets they must stay within are
in [.claude/rules/robustness.md](../.claude/rules/robustness.md) ("Performance and resources").

### Web client (Chrome)

`tests/e2e` `perf.rs` drives the release web build in headless Chrome against the release server
(tests/e2e/README.md has the setup):

```sh
cargo test -p teamcraft-e2e --features browser --test suite -- --nocapture perf::   # PERF_SCALE=small
```

Carol signs in, leaves, and comes back to a workspace that grew while she was away. "Small" is 3
spaces, 24 channels, 3,000 messages (1,500 in one channel) and 12 photos; it runs in the nightly e2e
suite and fails on a regression. Measured on the machine above while it was otherwise idle (one
Android emulator running), web bundle 9.5 MB of wasm:

| What                                                          | Small workspace       |
|---------------------------------------------------------------|-----------------------|
| Sign in and unlock the keys on a new browser                  | 1.1 s                 |
| Coming back: catch-up until the channel list shows            | 2.1 s                 |
| Coming back: until every channel of the space is read         | 2.4 s                 |
| Reload of a signed-in tab until the channel list shows        | 0.4 s                 |
| Main-thread tasks over 50 ms while catching up                | none                  |
| Channel switch, p50 / p95 / max                               | 31 / 35 / 41 ms       |
| Opening the 1,500-message channel                             | 35 ms                 |
| Message rows in the DOM, after opening / after scrolling back | 24 / 23 (virtualised) |
| Frames over 50 ms while scrolling back 120 steps              | 0 of 121              |
| Memory of the browser's processes (PSS): empty / app started  | 335 / 548 MiB         |
| JS heap after start / after 40 / after 80 channel switches    | 4.8 / 4.9 / 4.9 MiB   |
| PSS after 80 channel switches                                 | 560 MiB               |
| Idle in the foreground: CPU / wakeups (whole browser)         | 1.4 % / 163 per s     |

What the numbers say:

- The app adds about 210 MiB to the browser; most of it is the compiled wasm and its linear memory
  (the JS heap is under 5 MiB). Channel switches don't grow it.
- The message list is virtualised: the DOM holds about 25 rows whatever the channel's length.
- While idle the page still runs about 0.4 s of tasks per minute: the sync loop's 3 s tick and a
  30 s REST poll of unread counts, threads, presence and read receipts (#183). A one-second banner
  timer that ran even while connected is fixed.
- Sending used to read the whole conversation for every message (fixed); it showed when filling a
  20,000-message channel.
- Against the budgets in robustness.md: switching, scrolling, start and memory are well within them;
  idle network is not (the 30 s poll, #183, and the once-a-minute account data sync).
- Since #183 the counters come over the gateway (notifications.md, "Unread and mention counts"); the
  30 s poll is gone. The numbers above were measured before that change and are not re-run yet.

#### Large workspace

`PERF_SCALE=large`: 20 spaces, 300 channels, 100k messages (20,000 in one channel), 200 photos,
seeded through the real APIs in 12 minutes. Measured in a cloud VM (4 vCPUs, 15 GiB, headless
Chromium 1243, release server, Postgres in a container on the same VM), October 2026, `main` dd957c6
plus the fixes of the same change. A VM is noisy; read the numbers for leaks, outliers and growth
with the workspace, not to the millisecond. The small workspace on the same VM is given for
comparison.

```sh
PERF_SCALE=large E2E_BIN_DIR=target/release cargo test -p teamcraft-e2e --features browser \
  --test suite -- --nocapture perf::
```

| What                                                    | Large            | Small (same VM)  | Budget       |
|---------------------------------------------------------|------------------|------------------|--------------|
| Reload of a signed-in tab until the channel list shows  | 0.7 s            | 0.4 s            | ≤ 1.5 s      |
| Coming back after the workspace grew: channel list      | 191 s            | 2.1 s            | -            |
| Channel switch, p50 / p95 / max                         | 34 / 43 / 43 ms  | 34 / 40 / 42 ms  | p95 ≤ 100 ms |
| Opening the 20,000-message channel                      | 36 ms            | -                | -            |
| Message rows in the DOM, after opening / scrolling back | 14 / 23          | 24 / 23          | -            |
| Frames over 50 ms while scrolling back 120 steps        | 1 of 121         | 0 of 121         | -            |
| PSS of the browser: empty / app started                 | 426 / 664 MiB    | 428 / 609 MiB    | +350 MiB     |
| PSS after 100 more channel switches                     | 686 MiB (+3.4 %) | 650 MiB (+6.7 %) | ≤ 10 %       |
| JS heap after start / after the switches                | 5.0 / 5.1 MiB    | 5.0 / 5.0 MiB    | -            |
| Idle in the foreground: CPU, whole browser              | 7.3 %            | 1.4 %            | ≤ 1 %        |
| Idle: main-thread tasks per minute                      | 3,773 ms         | 471 ms           | -            |
| Idle: HTTP requests / gateway frames per minute         | 63 / 5           | 30 / 5           | none / -     |
| wasm, app / with the service worker                     | 9.41 / 10.19 MB  | same             | ≤ 10 MB      |

The empty browser idles at 0.6 % CPU and 25 wakeups per second.

What the numbers say:

- **Within budget:** start, channel switches, scrolling and memory. The app adds 238 MiB to the
  browser on the large workspace (181 MiB on the small one), and neither the JS heap nor the PSS
  grows with channel switches beyond the noise. The message list stays virtualised at 20,000
  messages.
- **Idle is not** (#246, #183): every change notification re-reads work that grows with the
  workspace (about 0.6 s of main-thread time per round on the large workspace, 70 ms on the small
  one), and refresh rounds trigger further rounds. Before this change each heartbeat answer (every
  25 s) counted as a change and started a round too; the 30 s poll and the loop remain.
- **Coming back** to a workspace that grew by 300 channels and 100k messages takes three minutes
  before the channel list shows (#247); the main thread is mostly free meanwhile (14 tasks over
  50 ms, 1 s in total), the time goes to the sequential catch-up (#155).
- **wasm:** the app is 0.6 MB under the budget, with the service worker 0.2 MB over (#248).

The desktop and Android apps are measured against the same harness (`PERF_MOBILE_USER`) in a local
session. Slack, Teams and Element were not measured: that needs their apps installed and accounts
on their services.

### Android (first numbers)

Release Rust core, debug Kotlin build (`assembleLibreDebug`), API 35 emulator (the Pixel 10 / API 37
image does not boot on this AMD host), signed out:

| What                                          | Value     |
|-----------------------------------------------|-----------|
| Cold start to the first frame (`am start -W`) | 1.0–1.2 s |
| Memory after start (PSS)                      | 95 MiB    |

## Limits to keep in mind

- All of this is for a single server instance and one database; rate limits and the call roster are
  per instance (`docs/operations.md`).
- Index segments, attachments and search are client-side and not covered by the server tests.
- The load tests do not use TLS, a reverse proxy or real clients; WebSocket frames are small.
