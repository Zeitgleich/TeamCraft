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
- While idle the page ran about 0.4 s of tasks per minute at the time of this table: the sync
  loop's 3 s tick and a 30 s REST poll of unread counts, threads, presence and read receipts. The
  poll is gone since (#183, #252; see "Idle traffic and updates per conversation" below). A
  one-second banner timer that ran even while connected is fixed.
- Sending used to read the whole conversation for every message (fixed); it showed when filling a
  20,000-message channel.
- Against the budgets in robustness.md: switching, scrolling, start and memory are well within them.
  Idle network was not at the time (the 30 s poll and the once-a-minute account data sync); both are
  gone since (#183, #252), re-measured below.

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
| wasm, app / with the service worker                     | 9.41 / 10.19 MB  | same             | -            |

The empty browser idles at 0.6 % CPU and 25 wakeups per second.

What the numbers say:

- **Within budget:** start, channel switches, scrolling and memory. The app adds 238 MiB to the
  browser on the large workspace (181 MiB on the small one), and neither the JS heap nor the PSS
  grows with channel switches beyond the noise. The message list stays virtualised at 20,000
  messages.
- **Idle was not** (#246, #183): every change notification re-read work that grows with the
  workspace (about 0.6 s of main-thread time per round on the large workspace, 70 ms on the small
  one), and refresh rounds triggered further rounds. Each heartbeat answer (every 25 s) counted as a
  change too (fixed with this measurement), the 30 s poll and the loop went with #183 and #253, the
  rest with #246 and #252: see "Idle traffic and updates per conversation" below.
- **Coming back** to a workspace that grew by 300 channels and 100k messages takes three minutes
  before the channel list shows (#247); the main thread is mostly free meanwhile (14 tasks over
  50 ms, 1 s in total), the time goes to the sequential catch-up (#155).
- **wasm:** uncompressed sizes at the time (#248); the budget now counts the compressed transfer,
  see "Web bundle size" below for the current numbers.

#### Idle traffic and updates per conversation

Re-measured for #246, #247, #252, #259 and #270 in the same kind of cloud VM (4 vCPUs, 15 GiB,
headless Chromium 1243, Postgres in a container on the VM), with the **debug** server binary (the
disk of the VM had no room for two release builds; the client is the release web build either
way). "Before" is `main` e569848, "after" the change. Same harness as above, with three additions:
API requests while idle apart from token renewal (`idle_api_requests_per_minute`, the regression
check asserts 0), when the first channel opened after coming back shows its newest message, and
when the background catch-up is done (no connection banner and no API request for 10 s). The idle
period starts only after that.

| What                                                      | Small before | Small after | Large before | Large after |
|-----------------------------------------------------------|--------------|-------------|--------------|-------------|
| Coming back: channel list shows                           | 4.4 s        | 1.6 s       | 247 s        | 1.6 s       |
| Coming back: first channel read                           | -            | 2.8 s       | 247 s        | 30.4 s      |
| Coming back: every channel of the first space read        | 5.0 s        | 3.4 s       | 250 s        | 32.9 s      |
| Coming back: whole workspace caught up                    | 5.0 s        | -           | 250 s        | 39.1 s      |
| Reload of a signed-in tab until the channel list shows    | 0.5 s        | 0.5 s       | 1.0 s        | 0.8 s       |
| Channel switch, p50 / p95                                 | 40 / 54 ms   | 41 / 52 ms  | 48 / 72 ms   | 66 / 155 ms |
| Idle: CPU, whole browser (first 5 min, see below)         | 0.8 %        | 0.7 %       | 8.0 %        | 33.2 %      |
| Idle: CPU, whole browser, the 5 min after that            | -            | -           | -            | 0.2 %       |
| Idle: main-thread tasks per minute                        | 115 ms       | 113 ms      | 3,731 ms     | 771 ms      |
| Idle: HTTP requests per minute (API)                      | 2.0          | 0 (0)       | 99           | 0.2 (0)     |
| Idle: gateway frames per minute                           | 5.0          | 5.0         | 33           | 5.0         |

Small: 3 spaces, 24 channels, 3,000 messages; idle 60 s. Large: 20 spaces, 300 channels, 100k
messages; idle 300 s. "Before" shows the channel list only once the whole catch-up is done, so its
first-channel and caught-up times are the same moment.

What the numbers say:

- **No idle traffic** besides the gateway heartbeat (#252): account data, presence, receipts and
  last seen are read once per connection or when shown. The server closes every connection after
  an hour (`connection_max_age_secs`, to check a fresh token); the resume then costs the
  once-per-connection reads: the unread counts and threads, presence in batches of 200, the
  announcement, the account data.
- **Change notifications cost what changed** (#246): on the large workspace main-thread work while
  idle went from 3.7 s to 0.8 s per minute; a new message re-reads one conversation and
  sends one entry, a presence event one person's entry. A write that leaves the entry as it was
  (a reply that stays in its thread, an edit, a reaction) sends only the conversation's ID, so the
  UI re-reads the feed and threads it shows of it.
- **Coming back** (#247): the list shows as soon as the conversation list arrived (1.6 s instead of
  four minutes), the channel the user opens is caught up first, the rest in the background with up
  to six conversations' round trips at once. Before any conversation's messages, the sync still
  reads every member list and joins the 300 groups from their welcomes, one after the other in the
  server's order; that is where the 30 s until the first channel is readable go.
- **After a large catch-up the browser was busy for minutes** (first idle window: Chrome's
  browser process at 31 %, the renderer at 2.4 %; the five minutes after that 0.2 %), and channel
  switches measured 155 ms at p95 meanwhile. The cause was the IndexedDB store: one read-write
  transaction per record written, and the whole MLS state (one record for all groups, rewritten
  after every group the catch-up processed) written again each time. Chrome worked through that
  backlog for minutes after the catch-up, and every committed transaction woke the page. Writes now
  go out in batches (one transaction after 50 ms, or at once beyond 8 MiB or on a flush), and of
  several writes of one record in a batch only the last; re-measured below.

#### Release server, after the catch-up settled

The large workspace again, after the IndexedDB batching and the review fixes of the same change
(presence log and snapshot indexes, per-conversation lookups in the UI mirror). Local machine
(24 threads, 62 GiB, headless Google Chrome 156, dev stack in containers on the same machine, other
work running beside it), **release** server and release test harness, October 2026. Two additions
to the harness: after the catch-up and again after the reload it waits until the whole browser
stays under 3 % CPU for 10 s (`seconds_*_until_browser_settled`) before it switches channels and
idles, and a second idle window of the same length (`PERF_SECOND_IDLE=1`).

```sh
PERF_SCALE=large PERF_SECOND_IDLE=1 cargo test --release -p teamcraft-e2e --features browser \
  --test suite -- --nocapture perf::
```

| What                                                     | Large                | Budget       |
|----------------------------------------------------------|----------------------|--------------|
| Coming back: channel list shows                          | 1.7 s                | -            |
| Coming back: first channel read / whole space read       | 26.1 / 27.2 s        | -            |
| Coming back: whole workspace caught up                   | 30.8 s               | -            |
| Caught up until the browser is quiet (after the reload)  | 0.1 s (0.1 s)        | -            |
| Reload of a signed-in tab until the channel list shows   | 1.0 s                | ≤ 1.5 s      |
| Channel switch, p50 / p95 / max                          | 40 / 58 / 64 ms      | p95 ≤ 100 ms |
| Opening the 20,000-message channel                       | 68 ms                | -            |
| PSS of the browser: empty / app started / after switches | 346 / 274 / 308 MiB  | -            |
| JS heap after the first / 100 more switches              | 2.7 / 2.8 MiB        | -            |
| Idle: CPU, whole browser, first 5 min / the 5 min after  | 0.1 % / 0.1 %        | ≤ 1 %        |
| Idle: main-thread tasks per minute                       | 40 ms                | -            |
| Idle: HTTP requests per minute (API)                     | 0.2 (0)              | none         |
| Idle: gateway frames per minute                          | 5.0                  | -            |

Seeding took 24.5 minutes. The empty browser idled at 4.1 % CPU here (the shared machine and a
newer Chrome; the cloud VM above measured 0.6 %); the app's idle numbers are the difference over
the same profile, so they don't include it. The empty-browser PSS is higher than the app's because
the empty page was measured while the machine was busier; read PSS for growth, not as absolute.

What the numbers say:

- **Within budget once settled:** channel switches at 58 ms p95 and the whole browser at 0.1 % CPU
  idle, in both five-minute windows. With the batched writes the browser is quiet 0.1 s after the
  catch-up is done; there is no backlog left to settle.
- The first channel is still readable only after 26 s: member lists and welcomes of all 300
  conversations come first (#247, "Left open" in the change).
- 63 tasks over 50 ms (3.9 s in total, the longest 130 ms) while coming back: the catch-up's MLS
  work and decryption on the page's thread.

#### Coming back: the open channel first, heartbeats during the sync

The large workspace again for #247, #325 and #326, same machine and harness, release server,
October 2026. Two additions to the harness: the client's catch-up phase timings and lost gateway
connections are printed from its console, and 20 switches each are measured while the background
catch-up runs and right after it, while the browser still writes it. "Before" ran while other
builds loaded the machine (load about 12); "after" is the final change on a quiet machine (load
1.5). An intermediate run of the change under load 28 measured 84 ms during and 125 ms right after
the catch-up, and 47 / 87 ms settled: switch times move with the machine's load; the catch-up
times did not.

| What                                                 | Before             | After              | Budget       |
|------------------------------------------------------|--------------------|--------------------|--------------|
| Coming back: channel list shows                      | 1.5 s              | 1.5 s              | ≤ 2 s        |
| Coming back: first channel read                      | 22.4 s             | 1.7 s              | ≤ 5 s        |
| Coming back: all 15 channels of the first space read | 23.8 s             | 4.9 s              | -            |
| Coming back: whole workspace caught up               | 28.7 s             | 16.1 s             | -            |
| Catch-up phases: member lists / welcomes / content   | 0.3 / 20.7 / 6.3 s | 0.3 / 11.5 / 3.6 s | -            |
| Switch p95 while the catch-up runs                   | 133 ms             | 73 ms              | ≤ 100 ms     |
| Switch p95 right after the catch-up                  | 98 ms              | 92 ms              | ≤ 100 ms     |
| Switch p50 / p95, settled                            | 38 / 72 ms         | 39 / 65 ms         | p95 ≤ 100 ms |
| Tasks over 50 ms while coming back (count / total)   | 60 / 3.6 s         | 57 / 3.2 s         | -            |
| Gateway connections lost while coming back           | 0                  | 0                  | -            |
| Idle: CPU, whole browser / API requests per minute   | 0.1 % / 0          | 0.1 % / 0          | ≤ 1 % / none |

What changed and what the numbers say:

- **The open channel first** (#247): between two member-list batches and between two welcomes the
  catch-up takes the conversation the user opened completely (members, its welcomes, MLS queue,
  messages). The UI tells the core when a conversation opens (`core_focus`): a channel with nothing
  stored asked the core for no messages, so the core never learned the focus before. The web core
  shows a conversation caught up ahead at once instead of with the next one-second progress round.
  Each of the 15 channels the harness opens in turn is readable 0.3 to 1 s after the click.
- **Welcomes are the bulk of a catch-up**: 300 joins in the server's order, each reading the
  conversation, its members and its MLS queue, and each rewriting the MLS state (3.6 MB here, one
  record for all groups). Those rewrites are what kept the page busy: every write was sealed at
  once (base64 in JSON, then AES-GCM). IndexedDB now seals a record when its batch goes out, so a
  replaced write costs nothing, and a batch over 1 MiB waits 200 ms more to take the next rewrite
  along (#326). The welcomes phase went from 20.7 to 11.5 s.
- **Heartbeats during the sync** (#325): the full sync, long commands and the upkeep run inside
  `keep_alive`; the catch-up kept its one connection (before: a sync over 90 s lost it, here the
  catch-up took less, so the test `runner::a_sync_longer_than_the_heartbeat_timeout_keeps_its_connection`
  covers the long case).
- **Left open (#424):** the MLS state is still one record rewritten per group, quadratic in the
  number of groups; splitting it per group is a change to the store format and needs its own
  review. Done since: see "The MLS state per entry (issue #424)" below.

#### Coming back: requests and round trips (issue #714)

Product requirement: after the app opens, the sync is done in **under 300 ms**, whatever the size of
the workspace and its chats. `tests/e2e` `sync_perf.rs` measures the client core (the code every app
runs) against the release harness's real server, Keycloak and MLS: Carol is in 3 spaces with 24
channels, one message each. It measures from "a new client opens the store" to "the gateway
connection is caught up" for a fresh session after the resume window (`session_invalid`) on an
unchanged workspace, the same with three channels that got a message meanwhile, and a resume within
the window. Every scenario runs once directly against the local server and once through a relay that
adds 30 ms to every round trip, like a client in another region or on a good mobile network. Debug
build of the server and the test, local machine (24 threads), October 2026; the core's own work (MLS,
store) is slower in a debug build than in the apps.

```sh
E2E_REQUIRED=1 cargo test -p teamcraft-e2e --features e2e --test suite -- --nocapture sync_perf
```

| What                                       | Before: requests | Before: local | Before: 30 ms RTT | After: requests | After: local | After: 30 ms RTT | Budget   |
|--------------------------------------------|------------------|---------------|-------------------|-----------------|--------------|------------------|----------|
| Fresh session, unchanged workspace         | 91               | 249 ms        | 1,333 ms          | 16              | 87 ms        | 187 ms           | ≤ 300 ms |
| Fresh session, three channels with news    | 91               | 258 ms        | 1,369 ms          | 22              | 119 ms       | 248 ms           | -        |
| Resume within the window                   | 6                | 27 ms         | 264 ms            | 6               | 9 ms         | 74 ms            | ≤ 300 ms |

"Before" is `main` f1a0f6e with the same test. The client prints the phases of each connection
(`gateway connection caught up`, `Client::last_connection_phases`); after the change, 30 ms away, an
unchanged workspace spends them like this: socket and `hello` 33 ms, `ready` 35 ms, the first reads
(all at once) 56 ms, then local work: deciding what is due 12 ms, the device list check 15 ms, the
rest a few ms.

What changed and what the numbers say:

- **One request per conversation was the cost.** Every member list, MLS queue and history page was read
  for every conversation (24 × 3 requests here), so the time grew with the workspace: about 300
  conversations took tens of seconds (see "Coming back: the open channel first" above). Now
  `GET /sync/heads` names where each conversation stands, and only conversations whose newest message,
  MLS entry or member list moved are read: 16 requests for 24 channels here, and in the core suite
  as many for 8 channels as for 2
  (`sync_heads::an_unchanged_workspace_costs_no_request_per_conversation`). The
  server answers it with three queries and one per space; the newest IDs are one backward index probe
  each (`mls_messages_conversation_id_idx`, migration 0063).
- **Round trips, not requests, are what a user far away waits for.** The full sync asked one thing
  after the other (about 30 round trips: 1.3 s at 30 ms, several seconds on a mobile network). The
  first reads now go out together after `ready` and each step takes its answer: three round trips for an
  unchanged workspace: socket and `hello`, `ready`, the first reads. The member lists and first
  pages of changed conversations are read at once too, and each conversation's MLS queue and history
  side by side.
- **A resume** read its counts, presence and link requests one after the other after the answer; now
  they are read while the server answers.
- The budget test fails above 18 requests or 300 ms for an unchanged workspace, also at 30 ms RTT.

Left open (see the PR of #714/#717):

- The web and desktop shells still restore the sign-in (token refresh at the identity provider) and read
  `GET /me` before they start the core; offline the web app shows the sign-in page instead of the
  stored chats. The cached account should start the core at once and the sign-in be checked beside it.
- The web core decrypts the whole IndexedDB store into memory before it answers the first read.
- The MLS state was still one record (#424); a catch-up of many changed groups rewrote it per group.
  Fixed since, see the next section.

The desktop and Android apps are measured against the same harness (`PERF_MOBILE_USER`) in a local
session. Slack, Teams and Element were not measured: that needs their apps installed and accounts
on their services.

#### The MLS state per entry (issue #424)

The MLS state of all groups was one record, rewritten after every change to any group: a catch-up
that joins 300 groups wrote it two times per group. It is now one record per openmls storage entry,
and a save writes the entries that changed (docs/e2ee.md §9.5). Measured in the client core against
the fake server: Carol comes back to channels Alice made and added her to meanwhile, and catches up
(`tests/suite/mls_storage.rs`, a store that counts the bytes written to the MLS table; debug build,
local machine, October 2026; "before" is the same test on `integration/v1.0.9` ecdc727):

```sh
# MLS_STORAGE_SQLCIPHER=1: on a SQLCipher file instead of the memory store
MLS_STORAGE_CHANNELS=300 cargo test -p teamcraft-core --all-features --test suite -- \
  --ignored --nocapture mls_storage
```

| Catch-up of           | Before: MLS writes | Before: bytes written | After: MLS writes | After: bytes written | State in the store, before / after |
|-----------------------|--------------------|-----------------------|-------------------|----------------------|------------------------------------|
| 100 groups            | 201                | 92.0 MB               | 1,203             | 1.1 MB               | 0.91 / 0.96 MB                     |
| 300 groups            | 601                | 816.2 MB              | 3,603             | 3.3 MB               | 2.71 / 2.86 MB                     |
| Per group, 300 groups | 2                  | 2.7 MB                | 12                | 11 KB                | 9.0 / 9.5 KB                       |

| Time of Carol's catch-up (debug build) | Before  | After   |
|----------------------------------------|---------|---------|
| 100 groups, store in memory            | 2.14 s  | 1.32 s  |
| 300 groups, store in memory            | 15.62 s | 7.96 s  |
| 100 groups, SQLCipher file             | 3.32 s  | 2.09 s  |
| 300 groups, SQLCipher file             | 22.91 s | 10.30 s |

- **Bytes written per join no longer grow with the number of groups:** about 11 KB per join, the
  joined group's entries and the spent key package. Before, the n-th join wrote the whole state of
  n groups twice. The test `writes_per_joined_group_do_not_grow_with_the_number_of_groups` fails if
  they grow again.
- More, smaller records: a group's state is about a dozen entries (tree, context, secrets, leaf
  keys). Opening a store reads them all, as it read the one record; the bytes are the same.
- What a save costs besides the writes: the client compares the provider storage with what it
  wrote last (a copy of the state in memory, compared byte for byte: no hashing, and no read of the
  store). With the state of 300 groups (2.7 MB in 3,600 entries) a save of one changed entry takes
  0.3 ms in a release build (`mls_storage::tests::measure_save_of_one_change`). The copy costs as
  much memory as the state.
- The catch-up time still grows faster than the number of groups (memory store, debug build: 1.3 s
  for 100, 8 s for 300). The comparison is one part of it (unoptimised in a debug build); the rest
  was not taken apart here.
- The browser's batches stay small now, so the extra wait for a batch over 1 MiB (#326) rarely
  applies; the large web workspace (`perf.rs`, `PERF_SCALE=large`) is still to be measured with it.

### Desktop on Linux: idle (issue #399)

The 1.0.2 desktop app on Linux (WebKitGTK 2.54) grew while nobody touched it. Measured on the
released binary and on the fix, signed in to a real workspace, window visible, untouched; memory
and CPU of the `WebKitWebProcess` from `/proc` every 30 s:

| What                                        | 1.0.2                       | Fixed            | Budget   |
|---------------------------------------------|-----------------------------|------------------|----------|
| Webview RSS at start / after 8 min          | 552 MiB / 3.76 GiB          | 302 / 303 MiB    | 400 MiB  |
| Webview anonymous memory after 8 min        | 3.59 GiB                    | 136 MiB          | -        |
| Webview CPU while idle                      | about 100 % of one core     | 0.1 %            | ≤ 0.5 %  |
| Shell calls from the webview while idle     | about 770 per second        | none             | -        |

The cause was a loop between the call state and the backend: effects in the desktop call setup emit
call events, the handler read the backend, which subscribed the effect to every backend change, and
each call action ends with a change notification. `CallState::emit` now runs the handler untracked
and the call mirror only notifies when its snapshot changed (unit tests in `calls.rs` and
`remote_calls.rs`). The web client does not wire these effects and idled cleanly before and after:
a phone (Android, Chromium 152) over 10 minutes stayed at about 300 MiB renderer RSS, a 4 to 5 MiB
JS heap and under 1 % CPU. `perf.rs` now also fails when the PSS or the JS heap grows over the idle
period.

### Desktop on Linux: memory

A user's 1.0.3 on KDE/Wayland (AMD GPU, window maximized, a real workspace in use)
showed 512 MiB RAM and 766 MiB GPU memory in the system monitor, 341 MiB and 711 MiB of it in the
`WebKitWebProcess`. Measured in October 2026 on WebKitGTK 2.54, Radeon RX 7800 XT, with the demo
workspace (`TEAMCRAFT_BACKEND=mock`) and a UI built with the release profile, the window maximized
on a 1920×1080 screen. Driven over the WebKit inspector (`WEBKIT_INSPECTOR_HTTP_SERVER`); GPU memory
from `/proc/<pid>/fdinfo` (`drm-memory-vram`), RAM as PSS from `smaps_rollup`, WebKit's own split
from the inspector's `Memory` domain.

Where the GPU memory goes: WebKitGTK 2.54 paints tiles on the GPU with Skia, in two painting
threads plus the main thread. Each has a Skia resource cache that WebKit sizes by the machine's RAM:
256 MiB per context on machines with 4 GiB or more. The caches fill with whatever was painted
(scratch textures, pictures) and are only emptied on memory pressure, which WebKit's monitor
reports when the system runs low. GPU memory therefore rises to a ceiling and stays there; it does
not grow with use and does not get the app killed:

| GPU memory of the webview                                     | Value    |
|---------------------------------------------------------------|----------|
| Signed in, idle                                               | 176 MiB  |
| After 1,500 channel switches                                  | 219 MiB  |
| After channels, a few lightbox pictures, 15 rounds            | 465 MiB  |
| 20 pictures of 4000×3000 shown as thumbnails (synthetic)      | 816 MiB  |
| The same after they went                                      | 352 MiB  |

Painting on the CPU instead (`WEBKIT_SKIA_ENABLE_CPU_RENDERING=1`) keeps it at 150–190 MiB but
made a channel switch three times slower and doubled the webview's CPU time (debug build: 300
instead of 105 ms at p50), which costs battery on a laptop; one GPU painting thread
(`WEBKIT_SKIA_GPU_PAINTING_THREADS=1`) saved nothing measurable. Both stay off. The cache sizes
can only be lowered through WebKit's memory pressure settings, which are fixed when the
`WebKitWebContext` is created; Tauri creates it without a hook for that.

What was fixed:

- **Window listeners per conversation.** The message list dropped the handles of three window
  listeners (focus, blur, visibility), and Leptos only removes a listener on `handle.remove()`:
  every conversation opened left three listeners and their closures for the life of the page.
  `scripts/check-async-signals.py` now fails on a discarded listener handle.
- **Browser caches.** WebKitGTK starts with the web browser cache model: up to 64 MiB of
  resources no page shows any more stay in memory, and pages are kept for back and forward. The
  app never navigates and its pictures are revoked `blob:` URLs, so neither cache is ever hit. The
  shell sets the document viewer model (`webview_memory.rs`); the inspector's `images` category
  stays at 0 instead of 17–27 MiB.
- **The Linux window edge** of the custom title bar was a fixed `::after` over the whole window,
  a compositing layer of the window's size (7.4 MiB at 1774×1074, 14 MiB at 2560×1440) blended on
  every frame. Four one-pixel lines now, with no backing store. `scripts/check-css.py` fails on
  `will-change` outside an allow-list.

Webview memory over channel switches between two busy channels (PSS, same build otherwise):

| Channel switches        | Before    | After     | Budget   |
|-------------------------|-----------|-----------|----------|
| 0 (signed in, idle)     | 176 MiB   | 176 MiB   | -        |
| 600                     | 311 MiB   | 299 MiB   | -        |
| 1,200                   | 383 MiB   | 371 MiB   | -        |
| 1,500, then GC and idle | 405 MiB   | 382 MiB   | 400 MiB  |

After the fix the app holds no detached DOM: the wasm-bindgen table of JS values stays at the same
size (2,408 entries after 20 and after 60 switches), no closure outlives its view once the JS heap
is collected, and the wasm memory grows by about 28 KiB per switch. The remaining growth is
WebKit's: its native allocations ("other" in the inspector) level off at about 250 MiB after about
600 switches, and the JS heap's capacity grows slowly between collections. Growth per 300 switches
fell from 78 to 14 MiB over the run, so it levels off rather than leaks, but it ends near the
desktop budget. Idle CPU was unchanged; no animation runs while the app is idle.

### Android (first numbers)

Release Rust core, debug Kotlin build (`assembleLibreDebug`), API 35 emulator (the Pixel 10 / API 37
image does not boot on this AMD host), signed out:

| What                                          | Value     |
|-----------------------------------------------|-----------|
| Cold start to the first frame (`am start -W`) | 1.0–1.2 s |
| Memory after start (PSS)                      | 95 MiB    |

### Web bundle size

The budget counts the **compressed** wasm a signed-in user's first visit transfers: the app, the
signed-in app's chunks that `init.js` fetches next to it, and the service worker, which is installed
on the first visit too. That is what a user waits for: the server sends the wasm
with gzip or brotli, the browser compiles it while it streams (`instantiateStreaming`), and the
service worker serves it from its cache on every later visit. The uncompressed size of a Leptos app
of this scope is not a useful budget on its own; 10 MB uncompressed was reached with the features of
v1, and nothing cheap is left to cut (below).

`.github/workflows/web-size.yml` builds the release bundle on every change to the crates in it and
runs `scripts/check-web-size.py`. The script prints uncompressed, gzip -9 and (when the `brotli`
module is installed) brotli -11 sizes, the gzip delta to `main` and a guest's first visit, and lists
the chunks that load later. The limit:

| Limit                 | Value       | Above it                         |
|-----------------------|-------------|----------------------------------|
| Budget: gzip -9 total | 4,700,000 B | fails; warns from 95 % (4.47 MB) |

The budget is the gzip total measured on `main` c55b2e58 plus about 10 %. gzip -9 is the measure
because every server and browser supports it and it is reproducible; brotli is printed as
information. The uncompressed size is printed but not limited: what a user waits for is the
transfer. The earlier growth ratchet on it (11.85 MB) was dropped with the split (#334): main had
outgrown it anyway, and the split moves code between files without changing what matters. Raising
the budget is a decision, not a fix.

#### The split (#334)

The app is split into wasm chunks (`crates/teamcraft-ui/src/areas.rs`, built by
`crates/teamcraft-web-bundle` from the Trunk hook `scripts/build-app.sh`; Trunk 0.22 can't split):

- **The signed-in app** (`session`: sign-in, the shell, everything a member uses) is a chunk of its
  own. `init.js` fetches its files next to the app's for every page except a guest's meeting link,
  so signed-in users load both at once and a guest only loads the guest pages. As the app grows,
  what a guest downloads doesn't.
- **Areas loaded when first used:** the administration console, settings, the lightbox, the
  workspace dialogs (spaces, channels, invitations), the call screens, meetings, the directory and
  search; since 1.0.8 also the account menu, the dialogs a first visit doesn't open, the Saved,
  Files and Calls pages, Activity, a conversation's members and pins, an integration channel's log
  view and the image codecs that make thumbnails (below); since 1.0.9 the synthesis of the message
  sounds (`sounds`, #800), with the first sound, the administration console's data layer (with the
  console, `admin`), a conversation's huddle bar (`calls`), and the formatting toolbar, the emoji
  picker, the date pills' menu, the prompt of a device asking to be linked, the desktop app's update
  banner and the "open in another tab" page (`dialogs`; below). `lazy_area` shows a spinner when a
  chunk takes a moment and an error with a retry when it can't load.
- Code several modules share goes into shared chunks (one per combination of modules), so each file
  is downloaded once.
- The call engine (`teamcraft-web-calls`, #330) stays a module of its own that loads in the
  background after sign-in; the budget doesn't count it.
- The background effects (#439) are part of it: tract's person segmentation (about 2 MB gzip) and
  the WebGPU compositor. The segmentation model (0.46 MB, 0.26 MB gzip) is a file of its own,
  downloaded when the first effect starts.
- A signed release's service worker installs without the chunks (the signed-in app's included) and
  fetches each one, checked against its signed hash, when it is first used or in the background
  about 15 s after a page loaded; not for a guest's meeting page (e2ee.md §9.3).

Release `trunk build` in the CI image (Rust 1.99.1, trunk 0.22.0, wasm-opt 131), gzip -9:

| Build                            | First visit, signed in | First visit, guest | Loaded later |
|----------------------------------|------------------------|--------------------|--------------|
| `main` b301a0d2 (one module)     | 4.93 MB                | 4.93 MB            | –            |
| Split (#334)                     | 4.15 MB                | 2.47 MB            | 1.12 MB      |

First visit = app + service worker (+ the signed-in app's chunks when signed in); the call module is
not counted in either. The split costs a signed-in first visit about 0.2 MB of overhead over one
module (each chunk imports what it uses from the app, which keeps those functions exported) and
156 requests instead of one; in exchange a guest loads half and everything an area alone uses
waits until it opens.

Back under the budget (#695): the quick switcher, the keyboard shortcuts, the inline video player and
the dialogs a first visit doesn't open (forward, edit history, bookmarks, share link, verifying a key
or a contact) moved into areas. Release `trunk build` in the local CI image, gzip -9:

| Web wasm       | Before (`a8778a26`) | After       | Delta     |
|----------------|---------------------|-------------|-----------|
| App            | 2,356,306 B         | 2,358,852 B | +2,546 B  |
| Signed-in app  | 1,999,066 B         | 1,936,659 B | −62,407 B |
| Service worker | 375,193 B           | 375,193 B   | 0 B       |
| Total          | 4,730,565 B         | 4,670,704 B | −59,861 B |

Over the budget again after 1.0.8 (CI: 4,809,730 B). The growth had no single cause: the 1.0.8 features (sync
heads, the persistent media cache, one active tab, permission gating, desktop file menus and drops)
added about 0.4 MB of first-visit code before wasm-opt, spread over the core and the shell. No
debug code or new dependency came with them. Three kinds of code a first visit doesn't need were in
it, and moved out:

- **The image codecs** (`image`, zune-jpeg, png, image-webp, gif and the encoders, about 0.5 MB
  before wasm-opt): `thumbnail.rs` makes thumbnails of sent pictures, and `image_dimensions` read the
  header through `ImageReader::into_decoder`, a `dyn ImageDecoder` whose vtables link every
  decoder's whole decoding. Now `image_dimensions` calls the format's decoder directly (headers
  only; a test checks it reads the same sizes as before), and in the browser the engine makes
  thumbnails through `make_thumbnail`, which the web app answers from the `images` chunk the first
  time a picture needs one (`web_core.rs`). Natively nothing changed.
- **`lazy_area`'s own views** were generic over each caller's closure, so every split point copied
  the placeholder and the error view into the signed-in app (about 0.25 MB before wasm-opt for the
  34 callers on `a6323ac5`, more with every new area). They are built once now; a caller only boxes
  its loader.
- **More areas:** the account menu and the custom status (`account`), a person's profile, a new
  direct message, linking and approving a device, "calls unavailable" (`dialogs`), the Saved, Files
  and Calls pages and a conversation's members and pins (`places`), Activity (`activity`), an
  integration channel's log view (`log`).

Release `trunk build` on macOS (trunk 0.22.0, Rust 1.99.1, wasm-opt 131, the CI job's commands; the
before is 128 B from CI's), gzip -9:

| Web wasm       | Before (`a6323ac5`) | After       | Delta      |
|----------------|---------------------|-------------|------------|
| App            | 2,416,993 B         | 2,287,481 B | −129,512 B |
| Signed-in app  | 2,016,805 B         | 1,900,122 B | −116,683 B |
| Service worker | 375,804 B           | 375,804 B   | 0 B        |
| Total          | 4,809,602 B         | 4,563,407 B | −246,195 B |

By step, measured one after the other: the areas −59 kB and −33 kB, `lazy_area` −26 kB, the image
codecs −128 kB. A guest's first visit went from 2.79 MB to 2.66 MB. Still in the first visit and the
next candidates: the admin console's data layer (`admin_live.rs`, about 0.1 MB before wasm-opt),
which the shell reaches through the backend's vtable and uses for an admin's permission scheme
outside the console, and the image headers `image_dimensions` reads (about 0.15 MB before
wasm-opt).

Over the budget again on the way to 1.0.9 (CI on `679c2dee`: 4,732,887 B; with #800, #830 and #838
4,751,073 B). The growth came with the settings' device checks and key backup (#409, #410), #818,
the abandoned device setups (#827) and the message sounds (#800); the settings themselves are an area
already. Measured on the signed-in app's files (`twiggy top` on the split before wasm-opt), parts a
first visit doesn't need were still in it, and moved out:

- **The administration console's data layer** (`admin_live.rs`, about 0.2 MB before wasm-opt with
  the admin API's wire types): the shell reaches it through the backend's vtable, so it stayed in the
  signed-in app although only an admin's console uses it. Its refresh and its requests (changes,
  on-demand data, connection tests, brand images) now run through `areas::admin_refresh` and
  `areas::admin_task`, in the console's chunk; what stays is the cached data the synchronous
  backend answers from. Nothing changes for an admin: the console loads that chunk anyway.
- **Parts shown only in some situations**, each where it shows (`lazy_area`, new places `Inline`
  for a part of a view and `Popover` for a popover, whose error leaves the app usable): the huddle
  bar with the first call it shows (`calls`); the formatting toolbar (off by default), the emoji
  picker and the date pills' menu the first time they open, the prompt of a device asking to be
  linked with the first request, the update banner only in the desktop app, and the page a tab
  shows while another tab runs the app (`dialogs`).

Every new module multiplies the shared chunks (one per combination of modules that use a function),
and each file costs a few hundred bytes of headers and types: a first try with four new modules
(one per part) made the signed-in app 745 files instead of 627; in existing modules it is 629. New
parts go into an existing module where one fits.

Release `trunk build` on macOS (trunk 0.22.0, Rust 1.99.1, wasm-opt 131, the CI job's commands),
gzip -9:

| Web wasm       | Before (`4e9bfd38`) | After       | Delta      |
|----------------|---------------------|-------------|------------|
| App            | 2,397,220 B         | 2,399,068 B | +1,848 B   |
| Signed-in app  | 1,959,081 B         | 1,798,771 B | −160,310 B |
| Service worker | 394,772 B           | 394,772 B   | 0 B        |
| Total          | 4,751,073 B         | 4,592,611 B | −158,462 B |

A guest's first visit stays at 2.79 MB. Loaded later grew accordingly: the console's chunk from
1.08 to 1.28 MB uncompressed, `dialogs` from 0.24 to 0.38 MB, `calls` from 0.29 to 0.32 MB.

Release `trunk build` in the CI image (Rust 1.99.1, trunk 0.22.0, wasm-opt 131), `main` c55b2e58:

| Web wasm       | Uncompressed | gzip -9     | brotli -11  |
|----------------|--------------|-------------|-------------|
| App            | 10,485,233 B | 3,906,338 B | 2,737,666 B |
| Service worker | 789,797 B    | 339,884 B   | 275,586 B   |
| Total          | 11,275,030 B | 4,246,222 B | 3,013,252 B |

Earlier measurement, when wasm-opt `--converge` was added (#311):

Release `trunk build`, Rust 1.99, wasm-opt 131, `main` e569848:

| Web wasm       | Before (`-Oz`) | After (`-Oz --converge`) | gzip -9 (after) |
|----------------|----------------|--------------------------|-----------------|
| App            | 10,282,671 B   | 10,272,725 B             | 3.83 MB         |
| Service worker | 790,289 B      | 789,496 B                | 0.34 MB         |
| Total          | 11,072,960 B   | 11,062,221 B             | 4.17 MB         |

The app grew from 9.41 MB (#248) to 10.27 MB by then and 10.49 MB at c55b2e58.

Where the app's code goes (`twiggy top` on an unstripped build, before wasm-opt, by crate):

| Crate(s)                                     | Share | What it is                                        |
|----------------------------------------------|-------|---------------------------------------------------|
| teamcraft-ui                                 | 19 %  | the views; the admin console's closures are large |
| tachys, reactive_graph, leptos               | 21 %  | Leptos (rendering and reactivity)                 |
| core, alloc                                  | 13 %  | the standard library                              |
| serde, serde_json and the derived impls      | ~18 % | 2.0 MB of deserializers (overlaps the other rows) |
| teamcraft-core, -client-api, -proto, -crypto | 18 %  | the client engine and the wire types              |
| data (`.rodata`)                             | 8.5 % | strings, the two Fluent files (210 kB)            |
| openmls, hpke-rs, p384, k256, curve25519     | 5 %   | MLS with every ciphersuite of openmls_rust_crypto |
| image, zune-jpeg, image-webp, png, fdeflate  | 3 %   | attachment thumbnails (`thumbnail.rs`)            |

What was tried, app wasm after wasm-opt:

| Change                                                    | Size         | Taken                          |
|-----------------------------------------------------------|--------------|--------------------------------|
| `-Oz` (as shipped)                                        | 10,282,671 B | -                              |
| wasm-opt `--converge`                                     | −9.9 kB      | yes                            |
| wasm-opt `--low-memory-unused`                            | −40 kB       | no: Rust's stack is down there |
| wasm-opt `-Oz -Oz`, `--gufa`, `--merge-similar-functions` | ≤ −10 kB     | no (same as `--converge`)      |
| `opt-level = "s"` instead of `"z"`                        | +2.15 MB     | no                             |
| `log` `release_max_level_off` (openmls' logging)          | −0.1 kB      | no (LTO drops it already)      |
| `tracing` `release_max_level_info` (no debug logs on web) | −33 kB       | no: loses the console's debug  |

Debug sections are already stripped (`strip = true`; the only custom sections left are 85 bytes),
and the profile is already `opt-level = "z"`, fat LTO, one codegen unit and `panic = "abort"`. So
there is no cheap megabyte left. What would make the first visit smaller:

- **Split the bundle:** done (#334, above).
- **Thumbnails through the browser** (`createImageBitmap` and `OffscreenCanvas`) instead of the
  `image` decoders on wasm: about 0.35 MB. Since 1.0.8 the codecs load only with the first picture
  (above), so this would now save a lazily loaded chunk, not first-visit bytes.
- **Leaner deserializers** for the largest wire enums: `ChatAction` alone is 129 kB, decoded from a
  `serde_json::Value`. Internally tagged enums go through serde's buffering `Content`, which adds
  0.25 MB.

## Limits to keep in mind

- All of this is for a single server instance and one database; rate limits and the call roster are
  per instance (`docs/operations.md`).
- Index segments, attachments and search are client-side and not covered by the server tests.
- The load tests do not use TLS, a reverse proxy or real clients; WebSocket frames are small.
