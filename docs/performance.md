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

### Cameras larger than the send size (issue #537)

The desktop sender shrinks a camera's frames to the send size on the CPU before the effects and
encoders (`VideoFrame::scaled`), and each encoder shrinks the lower simulcast layers from that
the same way. Since #967 the send size is 1080p in Auto quality and 720p when the user caps the
camera there (`camera_limit`): never more than the camera preset, whatever the camera delivers
(screen shares have their own limits). `capture_negotiate` picks the smallest mode at or above
the send size, so only cameras without one pay for the shrink: a 4K-only camera always, a 1080p
camera when capped at 720p. What that costs, and whether the GPU would do it for less, from
`crates/teamcraft-media/examples/camera_scaling_bench.rs` (synthetic frames: the moving test
pattern with sensor noise, as packed YUV like AVFoundation delivers it; no camera needed):

```sh
crates/teamcraft-media/models/fetch.sh
TEAMCRAFT_SEGMENTATION_MODEL=crates/teamcraft-media/models/selfie_segmentation_landscape.onnx \
  cargo run --release -p teamcraft-media --example camera_scaling_bench \
  --features openh264-source,segmentation,gstreamer
```

macOS only: an Apple M4 Max (10 performance + 4 efficiency cores), macOS 26.6, GStreamer 1.28,
with other work on the machine (load average 3.5 to 5.5). 300 frames per row. "CPU" is the
process's CPU time per frame over all threads (encoder threads, GPU driver), "% core" that time
against the 33 ms a frame has at 30 fps. **Linux and Windows were not measured**: the scaler and
the decision below are the same code there, the encoders and GPU drivers are not.

The GPU column is a wgpu prototype in the bench, what scaling in the effects pipeline would cost:
the camera's I420 picture uploaded, one compute pass (an area filter, so 4K → 720p wouldn't alias
like bilinear's one tap in nine; 41 to 57 dB luma PSNR against the CPU's picture) and the
send-size picture read back for the encoders.

| Stage                                                      | CPU per frame  | GPU: CPU per frame (latency) |
|------------------------------------------------------------|----------------|------------------------------|
| 4K: copy out of the camera's buffer                        | 0.23 ms        |                              |
| 4K: YUYV → I420                                            | 1.26 ms        |                              |
| 1080p: YUYV → I420                                         | 0.29 ms        |                              |
| 720p: YUYV → I420                                          | 0.14 ms        |                              |
| 4K → 1080p (Auto)                                          | 0.19 ms        | 1.38 ms (2.7 ms)             |
| 4K → 720p (capped at 720p)                                 | 0.86 ms        | 1.23 ms (1.8 ms)             |
| 1080p → 720p (capped at 720p), before → after this change  | 1.21 → 0.31 ms | 0.38 ms (0.6 ms)             |
| 1080p: the encoders' layers (720p, 360p, 180p), before → after | 1.23 → 0.35 ms |                          |
| 720p: the encoders' layers (360p, 180p)                    | 0.02 ms        |                              |
| Light blur, compositor only (GPU), 1080p                   | 0.44–0.57 ms   |                              |
| Light blur, compositor only (GPU), 720p                    | 0.25–0.32 ms   |                              |
| Segmentation (tract, CPU)                                  | 3.1–3.2 ms     |                              |

Everything before the encoders (copy, YUYV → I420, the shrink) back to back and paced at 30 fps
like a camera, where idle cores clock down between frames: 4K in Auto 1.69 ms, 4K capped at 720p
2.32 ms, 1080p in Auto 0.39 ms, 1080p capped at 720p 0.72 ms, 720p 0.20 ms.

The whole send path at 30 fps (copy, YUYV → I420, the shrink, the effect with segmentation, the
simulcast layers of `camera_layers`: four from 1080p, three from 720p); latency is the wall time
from the camera's buffer to the last layer's picture. Where the camera is shrunk: CPU scaling,
then the GPU prototype instead.

| Camera | Quality | Effect | Encoder      | CPU per frame (CPU / GPU scaling) | % core    | Latency, average (CPU / GPU) |
|--------|---------|--------|--------------|-----------------------------------|-----------|------------------------------|
| 4K     | Auto    | none   | OpenH264     | 11.9 / 14.0 ms                    | 36 / 42 % | 11.9 / 15.5 ms               |
| 4K     | Auto    | blur   | OpenH264     | 14.6 / 15.6 ms                    | 44 / 47 % | 16.4 / 18.9 ms               |
| 4K     | Auto    | none   | VideoToolbox | 6.6 / 7.8 ms                      | 20 / 23 % | 16.4 / 17.3 ms               |
| 4K     | Auto    | blur   | VideoToolbox | 7.5 / 7.7 ms                      | 22 / 23 % | 13.9 / 15.2 ms               |
| 4K     | 720p    | none   | OpenH264     | 5.3 / 9.2 ms                      | 16 / 28 % | 5.3 / 10.7 ms                |
| 4K     | 720p    | blur   | OpenH264     | 11.4 / 12.7 ms                    | 34 / 38 % | 12.4 / 15.2 ms               |
| 4K     | 720p    | none   | VideoToolbox | 6.1 / 7.0 ms                      | 18 / 21 % | 10.8 / 13.6 ms               |
| 4K     | 720p    | blur   | VideoToolbox | 9.5 / 10.6 ms                     | 29 / 32 % | 15.1 / 17.1 ms               |
| 1080p  | Auto    | none   | OpenH264     | 11.1 ms                           | 33 %      | 11.1 ms                      |
| 1080p  | Auto    | blur   | OpenH264     | 12.5 ms                           | 38 %      | 14.4 ms                      |
| 1080p  | Auto    | none   | VideoToolbox | 1.6 ms                            | 5 %       | 5.6 ms                       |
| 1080p  | Auto    | blur   | VideoToolbox | 5.7 ms                            | 17 %      | 11.3 ms                      |
| 1080p  | 720p    | none   | OpenH264     | 3.9 / 4.7 ms                      | 12 / 14 % | 3.8 / 5.4 ms                 |
| 1080p  | 720p    | blur   | OpenH264     | 7.7 / 8.1 ms                      | 23 / 24 % | 8.7 / 9.9 ms                 |
| 1080p  | 720p    | none   | VideoToolbox | 1.4 / 1.6 ms                      | 4 / 5 %   | 3.4 / 5.0 ms                 |
| 1080p  | 720p    | blur   | VideoToolbox | 5.2 / 5.6 ms                      | 16 / 17 % | 8.9 / 9.5 ms                 |
| 720p   | Auto    | none   | OpenH264     | 3.3 ms                            | 10 %      | 3.3 ms                       |
| 720p   | Auto    | blur   | OpenH264     | 7.5 ms                            | 23 %      | 8.6 ms                       |
| 720p   | Auto    | none   | VideoToolbox | 0.8 ms                            | 3 %       | 3.5 ms                       |
| 720p   | Auto    | blur   | VideoToolbox | 4.6 ms                            | 14 %      | 7.8 ms                       |

The VideoToolbox rows vary by up to a factor of two between runs on this machine (GStreamer's and
VideoToolbox's threads, the other load: two more runs of the no-effect rows gave 1.6 to 4.4 ms for
1080p in Auto and 3.0 to 5.0 ms for 4K in Auto); the OpenH264 rows stay within a few tenths of a
millisecond. GPU scaling cost more CPU than CPU scaling in every chain of all three runs.

What it shows, and what was decided:

- **The shrink is not worth moving to the GPU.** On the CPU it costs 0.2 to 0.9 ms per frame (at
  most 3 % of a core), against 0.4 to 1.4 ms of CPU for the GPU's upload, dispatch and read-back,
  plus 0.6 to 2.7 ms of latency. The upload is the problem: a 4K picture is 12 MB per frame,
  written by the CPU whatever the GPU does with it afterwards, while halving it on the CPU (4K →
  1080p, the Auto case) is a 2x2 average at memory speed. Folding the shrink into the effects
  pipeline would save the read-back only while an effect is on, and add a GPU round trip to every
  frame while none is (the usual case). So the shrink stays on the CPU. The camera's own hardware
  (AVFoundation's `videoSettings` width and height) remains the only cheaper place: nokhwa's
  bindings don't offer it and the workspace forbids `unsafe`.
- **What did matter was 1080p → 720p.** It is not a halving, so it went through the general
  bilinear path at 1.2 ms per frame, and it runs for every 1080p camera: for the encoders' 720p
  layer in Auto (#967, #978), and as the shrink when capped at 720p. It now has fixed weights like
  the halving (every three source pixels give two, weighted 3:1 and 1:3; the same pictures to the
  bit, tested): 1.21 → 0.31 ms, and the 1080p plan's layers 1.23 → 0.35 ms. About 0.9 ms (3 % of a
  core) less for every 1080p camera in every call, on every platform, and less than the GPU
  prototype costs.
- **A 4K camera costs about 1.3 ms of CPU per frame (4 % of a core) more than a 1080p one** before
  the encoders (paced: 1.69 against 0.39 ms), most of it the YUYV → I420 conversion of 8 million
  pixels (1.26 ms), not the shrink. In the whole chain it is about 0.8 ms with OpenH264, within
  the noise with VideoToolbox.
- Scaling adds no noticeable latency on the CPU (at most 0.9 ms); the latency is the encoders'.
- Segmentation (3 ms) is still the largest single cost of an effect; the GPU compositor stays
  below 1 ms of CPU.
- Where a call's CPU (and battery) goes is the encoder: OpenH264 at 1080p takes 11 ms per frame (a
  third of a core), VideoToolbox 1.6 to 4.4 ms. Not the shrink.

The first round of #537 (#963), when the send size was still 720p, made the scaler work out the
source positions once per row and column with 8-bit weights, average 2x2 blocks for halving and
reuse shared source rows, and made the YUYV conversion go through whole rows: 4K → 720p went from
2.13 to 0.85 ms, 1080p → 720p from 2.07 to 1.05 ms, 4K YUYV → I420 from 2.84 to 1.18 ms.

Bilinear shrinking by 3 (4K → 720p: only a 4K-only camera capped at 720p) samples one source pixel
of nine and can alias fine patterns; an area filter (as in the GPU prototype) would look better on
the CPU at about the same cost, if it shows.

### What each camera layer sends (issue #967)

In calls a Mac's 720p camera layer, meant for 1.5 Mbit/s, sent 155 to 306 kbit/s through
VideoToolbox, and the picture looked soft. From
`crates/teamcraft-media/examples/camera_bitrate.rs`: frames encoded as they come, at 30 fps in
real time, as the call's encoder thread does; a calm synthetic camera picture (a room and a
person who barely moves, a denoised camera's faint noise); the first second left out; luma PSNR
of a sample of each layer decoded with OpenH264 against what its encoder was given. Apple M4 Max,
macOS 26.6, GStreamer 1.28; runs vary by about 0.5 dB.

```sh
cargo run --release -p teamcraft-media --example camera_bitrate \
  --features gstreamer,openh264-source -- calm --seconds 10 [--max 720] [--encoder software]
```

A 720p camera (and before #967 every camera, a 1080p one shrunk to 720p):

| Layer | Target     | VideoToolbox, average bitrate (before) | VideoToolbox, constant bitrate (after) | OpenH264            |
|-------|------------|----------------------------------------|----------------------------------------|---------------------|
| 180p  | 94 kbit/s  | 15 kbit/s (16 %), 38.4 dB              | 97 kbit/s (103 %), 45.2 dB             | 19 kbit/s, 45.5 dB  |
| 360p  | 375 kbit/s | 29 kbit/s (8 %), 40.2 dB               | 377 kbit/s (101 %), 46.3 dB            | 73 kbit/s, 47.2 dB  |
| 720p  | 1.5 Mbit/s | 602 kbit/s (40 %), 41.2 dB             | 1504 kbit/s (100 %), 45.4 dB           | 424 kbit/s, 44.9 dB |

A 1080p camera since #967 (the average bitrate measured with these layers before the change):

| Layer | Target     | VideoToolbox, average bitrate | VideoToolbox, constant bitrate | OpenH264             |
|-------|------------|-------------------------------|--------------------------------|----------------------|
| 360p  | 375 kbit/s | 42 kbit/s (11 %), 39.7 dB     | 381 kbit/s (102 %), 46.0 dB    | 71 kbit/s, 47.2 dB   |
| 720p  | 1.5 Mbit/s | 440 kbit/s (29 %), 42.0 dB    | 1504 kbit/s (100 %), 45.5 dB   | 420 kbit/s, 44.9 dB  |
| 1080p | 3 Mbit/s   | 1348 kbit/s (45 %), 41.6 dB   | 3038 kbit/s (101 %), 43.9 dB   | 1146 kbit/s, 46.2 dB |

Encoding a frame (shrinking it to the layers and encoding all three) took 2.0 ms of CPU with
VideoToolbox for both cameras (2.9 and 4.5 ms wall time), 2.6 and 4.9 ms with OpenH264.

- VideoToolbox at an average bitrate (`AverageBitRate`) stops spending bits on a calm picture
  long before it looks good: the 720p layer at 41 dB where its bitrate buys 45. A camera with
  less noise than this picture (a Mac's in daylight) undershoots further, as in the calls. At a
  constant bitrate (`rate-control=cbr`; `kVTCompressionPropertyKey_ConstantBitRate`, Apple
  silicon from macOS 13, which GStreamer emulates with data rate limits elsewhere) every layer
  sends its target, 4 to 7 dB sharper. A quality floor (`quality`) instead made VideoToolbox
  ignore the bitrate: 0.9 sent 9.3 Mbit/s for the 1.5 Mbit/s layer.
- OpenH264 stops below its target on a calm picture too, but only once the picture is as sharp as
  the bitrate would make it (45 dB and more): its rate control bottoms out at its lowest
  quantizer, not at a soft picture. On the busy camera of the SFU's tests it uses its whole
  bitrate.
- Through a real SFU (`video_quality` in its test suite: a busy 720p camera, the viewer picks
  720p) the 720p layer arrived at 143 kbit/s with VideoToolbox at an average bitrate and at
  1496 kbit/s at a constant one (OpenH264: 1365 kbit/s with the rest of the suite running).
- The browser has the same problem in Chrome's hardware encoder on a Mac. A page encoding a calm
  30 fps picture with WebCodecs (H.264 Constrained Baseline, Chrome 156, same Mac): in
  `latencyMode: "realtime"` 36 of 375 kbit/s at 360p, 503 of 1500 at 720p and 1940 of 3000 at
  1080p, and still 533 kbit/s when 720p was given 3 Mbit/s; `bitrateMode: "constant"` changed
  nothing. In `"quality"` mode 424, 1700 and 3053 kbit/s, with the pictures out 5 to 9 ms after
  the frame (6 to 11 ms in `"realtime"`). Chrome's software encoder sent about 1.1 Mbit/s at 720p
  in either mode. Camera encoders in browsers on Apple devices use `"quality"` now
  (`call_web/video.rs`, `camera_latency_mode`); Safari, Firefox and other platforms' hardware
  encoders are not measured.

### Preview of the own screen share (issue #1175)

The person sharing sees a small live preview. It decodes nothing and runs only while it is on
screen, the page is visible and it isn't collapsed (the setting stays for the next share).

| Client  | Source                                                  | Rate        | Size                        |
|---------|---------------------------------------------------------|-------------|-----------------------------|
| Browser | A clone of the capture's track in a muted `<video>`     | At most 5/s | The capture, drawn small    |
| Desktop | The encoder's last picture as JPEG images (`self_view`) | At most 5/s | At most 640x400, quality 70 |

The browser's frame rate limit applies to the clone only, where the browser honours it; elsewhere
the preview plays the capture's rate, at the compositor's cost of a small video. On the desktop,
each image costs one copy of the sent picture, one scale and one JPEG encode on the view's own
thread, and a still screen costs nothing after its first image. Without a viewer the encoder
thread does one lock per loop and no copy. Not measured yet on real hardware.

### Screen share: profiles and codecs (issue #565)

A share is sent per its profile (`video::ScreenProfile`): detail up to 1440p30, motion up to
1080p60, and with the 4K option detail up to 4K30 and motion up to 1440p60 (top layer up to 16
Mbit/s instead of 8). A detail share goes out in AV1 with WebCodecs' screen content tools where
the sender's browser encodes AV1 and every viewer decodes it (ARCHITECTURE.md §6.3, "Video
codecs"); H.264 otherwise. All numbers: Apple M4 Max (10 performance + 4 efficiency cores),
macOS 26.6, on AC power, other work on the machine.

**Which codec, in the browser.** `dev/screen-codec-bench/` draws an editor-like screen (code in
real fonts, a file list, a tab strip; text at 1.25× at 1440p and 2× at 4K, as a HiDPI display
draws it) into a canvas, encodes it with WebCodecs, decodes it again and compares every tenth
picture's luma plane with the source's (BT.709). "Still": the page with one line being typed and
a blinking cursor; "scroll": the page scrolling 6 px per frame at 1× (a fast, steady scroll).
150 frames at 30 fps (60 for motion), the first second left out, `latencyMode: "realtime"`, as
the call configures its screen encoders. Chrome 156, headless.

```sh
cd dev/screen-codec-bench && python3 make_runs.py
python3 server.py chrome q1440.json q1440.jsonl && python3 table.py q1440.jsonl
```

1440p at 30 fps, scrolling text (what is sent, luma PSNR):

| Target     | H.264, hardware      | H.264, software      | AV1, `contentHint: "text"` | AV1, no hint         |
|------------|----------------------|----------------------|----------------------------|----------------------|
| 1 Mbit/s   | 918 kbit/s, 37.1 dB  | 778 kbit/s, 34.8 dB  | 1015 kbit/s, 47.8 dB       | 954 kbit/s, 40.3 dB  |
| 2 Mbit/s   | 1914 kbit/s, 43.4 dB | 1928 kbit/s, 43.2 dB | 1990 kbit/s, 50.4 dB       | 1890 kbit/s, 45.4 dB |
| 4 Mbit/s   | 3905 kbit/s, 50.8 dB | 3855 kbit/s, 49.2 dB | 3489 kbit/s, 53.0 dB       | 3615 kbit/s, 50.4 dB |
| 6.6 Mbit/s | 5982 kbit/s, 55.1 dB | 6338 kbit/s, 51.3 dB | 4576 kbit/s, 53.9 dB       | 5496 kbit/s, 53.0 dB |

1440p at 30 fps, a still page with a line being typed:

| Target     | H.264, hardware      | H.264, software     | AV1, `contentHint: "text"` | AV1, no hint        |
|------------|----------------------|---------------------|----------------------------|---------------------|
| 1 Mbit/s   | 1110 kbit/s, 44.9 dB | 421 kbit/s, 45.8 dB | 314 kbit/s, 51.1 dB        | 532 kbit/s, 47.6 dB |
| 2 Mbit/s   | 673 kbit/s, 51.8 dB  | 110 kbit/s, 45.3 dB | 249 kbit/s, 51.7 dB        | 397 kbit/s, 50.2 dB |
| 4 Mbit/s   | 527 kbit/s, 53.6 dB  | 46 kbit/s, 47.7 dB  | 152 kbit/s, 51.9 dB        | 334 kbit/s, 50.8 dB |
| 6.6 Mbit/s | 237 kbit/s, 53.9 dB  | 46 kbit/s, 45.0 dB  | 102 kbit/s, 52.0 dB        | 210 kbit/s, 51.2 dB |

4K at 30 fps (text at 2×) and 1080p at 60 fps (motion):

| Picture, scene  | Target    | H.264, hardware      | H.264, software      | AV1 (`"text"`; `"motion"` at 60 fps) |
|-----------------|-----------|----------------------|----------------------|--------------------------------------|
| 4K30, scroll    | 4 Mbit/s  | 2283 kbit/s, 51.6 dB | 3452 kbit/s, 49.8 dB | 1241 kbit/s, 53.4 dB                 |
| 4K30, scroll    | 15 Mbit/s | 2007 kbit/s, 55.7 dB | 2392 kbit/s, 50.1 dB | 1011 kbit/s, 54.4 dB                 |
| 4K30, still     | 4 Mbit/s  | 1209 kbit/s, 51.7 dB | 219 kbit/s, 47.5 dB  | 430 kbit/s, 52.8 dB                  |
| 4K30, still     | 15 Mbit/s | 585 kbit/s, 55.4 dB  | 76 kbit/s, 47.5 dB   | 139 kbit/s, 53.5 dB                  |
| 1080p60, scroll | 4 Mbit/s  | 2061 kbit/s, 54.2 dB | 2652 kbit/s, 50.8 dB | 2899 kbit/s, 51.7 dB                 |
| 1080p60, scroll | 8 Mbit/s  | 1842 kbit/s, 54.5 dB | 2406 kbit/s, 50.2 dB | 2738 kbit/s, 52.6 dB                 |

- **AV1 with its screen content tools keeps text sharp at a fraction of the bitrate.** Scrolling
  at 1440p, AV1 at 1 Mbit/s (47.8 dB) is as sharp as H.264 at about 3 Mbit/s, and at 2 Mbit/s
  (50.4 dB) as H.264 at 4: 10.7 and 7.0 dB more than Chrome's hardware H.264 at the same bitrate.
  That is what a viewer on a weak link gets, and what the half-size layer has. At the full
  1440p bitrate both are sharp (53.9 and 55.1 dB), AV1 with a third less. A still page costs AV1
  100 to 300 kbit/s against H.264's 240 to 1100.
- **The hint matters:** without `contentHint: "text"` (libaom without its screen content mode)
  AV1 is 5 to 7 dB less sharp at 1 to 2 Mbit/s. Chrome 156 and Firefox 159 accept the hint.
- **Motion stays H.264:** at 1080p60 the hardware H.264 encoder was sharper (54.2 against 51.7 dB)
  with less bitrate, at a fraction of the CPU (below). Motion shares and cameras keep H.264.
- **4K text is cheap to send:** text drawn at 2× compresses well; the 4K top layer sent 1 to 2.4
  Mbit/s of its 15 at 51 to 55 dB. The cap matters for pictures, video and fast changes.
- **VP9 is not used:** Chrome's VP9 (libvpx) took the canvas' full-range RGB but tagged its
  decoded pictures with limited range and the BT.709 transfer, so a canvas drew them with a
  constant shift (33 dB at any bitrate; the Y plane 22 dB off the source). It also cost 1.5 to 2×
  AV1's CPU. VP9 would have been the codec for viewers without AV1 decoding (Safari on Macs before
  M3); they get H.264.
- **Who encodes and decodes AV1:** Chrome 156 and Firefox 159 here encode AV1 and VP9 in software
  only and decode both in hardware (`isConfigSupported`). VideoToolbox on this Mac has no AV1 or
  VP9 encoder (H.264, HEVC, ProRes, JPEG only) and decodes AV1 in hardware (Apple silicon from
  M3), VP9 not; Safari and the desktop's WebKit view decode AV1 where the hardware does. The
  desktop's own encoders stay H.264 (VideoToolbox, VA-API, OpenH264): a software AV1 encoder in
  the desktop (GStreamer's `svtav1enc` with its screen content mode, `av1enc`, `rav1enc`) is a
  follow-up.

**CPU in the browser** (same page, the encoder's CPU time per frame: the browser's processes
while encoding, as fast as it goes and without decoding, minus the page drawing the same frames
without encoding; one encoder at the top layer's size; a share adds its half-size layer, about a
quarter more):

| Picture, scene  | H.264, hardware | H.264, software | AV1, `"text"`  |
|-----------------|-----------------|-----------------|----------------|
| 1080p30, still  | 5.0 ms (15 %)   | 5.0 ms (15 %)   | 4.1 ms (12 %)  |
| 1080p30, scroll | 1.9 ms (6 %)    | 7.2 ms (22 %)   | 8.1 ms (24 %)  |
| 1440p30, still  | 3.6 ms (11 %)   | 7.6 ms (23 %)   | 7.5 ms (22 %)  |
| 1440p30, scroll | 3.7 ms (11 %)   | 13.1 ms (39 %)  | 15.4 ms (46 %) |
| 4K30, still     | 8.1 ms (24 %)   | 22.0 ms (66 %)  | 15.9 ms (48 %) |
| 4K30, scroll    | 7.6 ms (23 %)   | 27.0 ms (81 %)  | 32.9 ms (99 %) |
| 1080p60, scroll | 2.0 ms (12 %)   | 6.9 ms (41 %)   | 8.6 ms (52 %)  |
| 1440p60, scroll | 3.6 ms (21 %)   | 13.2 ms (79 %)  | 15.4 ms (92 %) |

The percentage is of one core at the picture's frame rate. libaom spreads the work over several
threads, so a 4K share in AV1 takes about one core in all, spread over several.

**What the share policy (#1733) makes of these numbers.** Software AV1 takes the top layer only up
to about 1080p and at 15 fps: 1080p scrolling then costs about 4 ms per 66 ms frame (12 % of a
core) instead of 24 %, and a 1440p share, at 46 % of a core in software AV1, goes out in H.264
(hardware where there is one) at full size instead. The H.264 base layer (480p, 15 fps) every
viewer decodes costs a ninth of a 1440p picture's pixels at half its rate. On a loaded machine
(Windows 11, Chrome 157, a VM with the host at load 34) the share had dropped its 1440p layer and
sent 720p at 27 fps; under the policy the encoder-load controller lowers the top layer's rate
first (30, 20, 15, 10, 5 fps), then stops the middle layer, then turns software AV1 into H.264,
and only then makes the top smaller, one size at a time (1080, 900, 720 lines at 5 fps) before
the 480p base is all that is left. No budget in the table at the top changes: a share's
uplink demand at 1440p grows by the base layer (about 0.5 Mbit/s) and shrinks where AV1 replaces
H.264 on top.

**The desktop's encoders** (`crates/teamcraft-media/examples/screen_bench.rs`: every layer of the
profile, encoded in real time as the call's encoder thread does, from a synthetic page of
pixel-font text on a 3840x2160 display, 10 s per row; CPU is the process's over all threads,
VideoToolbox's included; capture and its BGRA conversion are not part of it):

```sh
cargo run --release -p teamcraft-media --features gstreamer,openh264-source --example screen_bench -- \
  --profile detail-4k --scene scroll [--encoder software]
```

| Profile (top layer) | Scene  | VideoToolbox, CPU per frame | OpenH264, CPU per frame (frame rate reached) |
|---------------------|--------|-----------------------------|----------------------------------------------|
| detail (1440p30)    | still  | 0.6 ms (2 %)                | 19.2 ms (58 %)                               |
| detail (1440p30)    | scroll | 0.7 ms (2 %)                | 19.9 ms (60 %)                               |
| motion (1080p60)    | still  | 0.3 ms (2 %)                | 10.7 ms (64 %)                               |
| motion (1080p60)    | scroll | 0.3 ms (1 %)                | 10.5 ms (63 %)                               |
| 4K detail (4K30)    | still  | 1.4 ms (4 %)                | 46.6 ms (140 %; 18 fps)                      |
| 4K detail (4K30)    | scroll | 1.4 ms (4 %)                | 93.3 ms (280 %; 10 fps)                      |
| 4K motion (1440p60) | still  | 0.4 ms (2 %)                | 19.2 ms (115 %; 45 fps)                      |
| 4K motion (1440p60) | scroll | 0.4 ms (2 %)                | 18.9 ms (113 %; 46 fps)                      |

The GPU's utilization (`ioreg`, "Device Utilization %") stayed at 0 to 9 % during the
VideoToolbox runs, as without them: the encoder is a media engine of its own. Battery drain was
not measured (the Mac ran on AC power, and `powermetrics` needs root); CPU time is the stand-in.

**The desktop's share follows the same ladder** (#1777). The encoder thread measures its load
per encoded frame: the time the encoders took against the time since the previous frame (a
second at most, so a still screen's pauses don't count), and the frames the capture dropped
because the thread was busy. The call task feeds that, and how the uplink treats the top layer,
into the same `ShareGovernor` the browser uses, every 100 ms. Its steps are H.264 only (the desktop
encodes no AV1 or VP9): 30, 20, 15, 10, 5 fps, then the middle layer off, then the top layer's
sizes, then the 480p base alone. A rate step changes the running encoders in place (#1781:
OpenH264's `SetOption`, the GStreamer elements' `bitrate` while they play, which `vtenc_h264`
passes on as VideoToolbox's `AverageBitRate` and `mfh264enc` as Media Foundation's
`CODECAPI_AVEncCommonMeanBitRate`): no keyframe. A layer or size step publishes the share anew,
each layer starting with a keyframe, as the browser does. On a 1440p share in OpenH264 (60 % of
a core at 30 fps above) the first step to 20 fps should leave about 40 % (the same work per
frame, two thirds of the frames); not yet measured on a slow machine.

**Budgets** that follow from these numbers:

- Desktop with a hardware encoder: encoding a share costs at most about 2 % of a core at the
  standard sizes and 4 % at 4K. Without one (OpenH264), 60 % of a core at the standard sizes,
  and a 4K share doesn't keep its frame rate (10 to 18 fps on 1.4 to 2.8 cores): 4K needs a
  hardware encoder.
- Browser: a detail share in AV1 costs about half a core at 1440p30 (both layers) and one core at
  4K30, against a tenth and a quarter with the hardware H.264 encoder. The call's encoders skip a
  frame while two wait (`MAX_ENCODE_QUEUE`), so a slower machine sends a detail share at a lower
  frame rate, as detail mode intends.
- Viewers: AV1 is decoded in hardware where it is announced on Apple silicon (M3 and later) and
  by dav1d in Chrome and Firefox (not measured here).

Seen on the way: VideoToolbox sent the *lower* layers of the scrolling page far above their
targets. Fixed in #979 (next section).

### Screen layers at their bitrate (issue #979)

VideoToolbox at an average bitrate (`rate-control=abr`, what screen shares use since #530, so a
still screen sends next to nothing) encodes at the `quality` GStreamer always sets
(`kVTCompressionPropertyKey_Quality`, 0.5 by default) and ignores the bitrate: with the 720p
layer's bitrate set to 100 kbit/s instead of 1.7 Mbit/s it still sent 9.1 Mbit/s of scrolled text,
and `data-rate-limits` (`kVTCompressionPropertyKey_DataRateLimits`, set and accepted) changed
nothing either. The top layer of a share stays below its target because the scrolling page moves
by whole pixels in it; a half-size layer gets half-pixel motion that the encoder can't follow by
moving blocks, and at a fixed quality it pays for that in bits. A constant bitrate
(`rate-control=cbr`) holds the target but spends it on every change: 5.8 of 6.6 Mbit/s for a
line being typed on a 1440p screen, against 22 kbit/s at the average bitrate. A `quality` changed
on a running encoder is taken by GStreamer but not by VideoToolbox.

So `video_gstreamer::QualityGovernor` measures each VideoToolbox screen layer's pictures (the last
eight, without keyframes and the 200 ms of pictures after one, which VideoToolbox makes several
times larger) and rebuilds the layer's encoder at a lower `quality` while it sends more than its
bitrate, in a step sized by how far over (ln(share) / 4), and at a higher one once it sends less
than half (aimed at 60 %, at most once a second; straight back to 0.5 below 5 %, as a screen
that stands still sends). Each change costs a keyframe: 2 to 4 changes per scroll here. Cameras (constant
bitrate) and the other platforms' encoders (VA-API, Media Foundation: constant bitrate) are not
touched.

`screen_bench`, M4 Max, macOS 26.6, GStreamer 1.28.8, 10 s per row, every frame encoded; target and
sent in kbit/s, luma PSNR of the layer's own pictures (the synthetic 1 px text, harsher than real
text after scaling):

| Profile, scene        | Layer      | Target | Before: sent (PSNR) | After: sent (PSNR) | Keyframes after |
|-----------------------|------------|-------:|--------------------:|-------------------:|----------------:|
| detail, scroll        | 720p       |   1659 |      9096 (31.3 dB) |     1352 (19.2 dB) |               6 |
|                       | 1440p      |   6636 |       281 (40.8 dB) |      281 (40.8 dB) |               1 |
| detail 4K, scroll     | 540p       |    933 |     10616 (18.7 dB) |      502 (14.6 dB) |               3 |
|                       | 1080p      |   3732 |     20457 (32.4 dB) |     2698 (20.9 dB) |               4 |
|                       | 4K         |  14930 |       382 (43.8 dB) |      382 (43.8 dB) |               1 |
| motion, scroll        | 540p60     |   1866 |     23231 (29.9 dB) |     1261 (16.6 dB) |               5 |
|                       | 1080p60    |   7465 |      1099 (36.5 dB) |     1100 (36.5 dB) |               2 |
| motion 4K, scroll     | 720p60     |   3318 |     18300 (31.3 dB) |     2554 (19.2 dB) |               6 |
|                       | 1440p60    |  13271 |      1038 (40.7 dB) |     1038 (40.7 dB) |               2 |

Still screens (a line being typed) send the same before and after, every layer at its old quality
(detail: 31 and 70 kbit/s at 31.1 and 36.4 dB; 4K: 119, 41 and 216 kbit/s); with
`--frames changes` (only changed pictures, plus one a second, as a share sends) 11 and 22 kbit/s,
and the scrolling rows as above. The scrolled lower layers lose their sharpness (as with a
constant bitrate: 20.7 dB for the 720p layer at 1515 kbit/s) and get it back with the first
pictures after the scroll stops (on a still screen the next picture once a second, then a sharp
keyframe with the one after). Encoding costs a little more while a layer is held (detail scroll: 0.85 ms
CPU per frame against 0.6), and a rebuilt encoder answers each frame before the next (3.4 ms wall
per frame against 0.4: the first encoder of a layer, whose first picture takes longer than the
50 ms wait, then hands each picture out with the next frame; fixed in #990, below).

Through the real SFU (`screen_layer_bitrate` in the SFU suite, release build with
`--features teamcraft-media/gstreamer`; the viewer picks each layer and measures 4 s of it):

| Share | Layer | Target | Before            | After            | OpenH264 (debug build) |
|-------|-------|-------:|------------------:|-----------------:|-----------------------:|
| 1440p | 720p  |   1658 | 22613 (1363 %)    | 1235 (74 %)      | –                      |
|       | 1440p |   6635 | 537 (8 %)         | 575 (9 %)        | –                      |
| 720p  | 360p  |    500 | 6428 (1286 %)     | 314 (63 %)       | 480–585 (8 frames)     |
|       | 720p  |   2000 | 269 (13 %)        | 259 (13 %)       | 334–351                |

### Short bitrate peaks of the encoders (issues #1005, #1205)

The encoders get an average (VideoToolbox for screens, WebCodecs, OpenH264), and a whole new
picture can send far more than that for a moment. The question was whether those peaks hurt on a
capped uplink, and so whether a cap is worth what it costs. Measured on Linux (Chrome 156 and
OpenH264, #1005) and on a Mac (#1205: VideoToolbox, Chrome 156 and Firefox 159 on an Apple M4
Max, macOS 26.6, GStreamer 1.28.8). **Never run: Safari, the hardware encoders of Windows and
Android**; the tools below run on them unchanged.

**How.** Bytes per picture from the encoder, as a multiple of what the target bitrate sends in
the same time: the largest picture (a frame's share of the target), the busiest 100 ms and the
busiest 1 s after the first second. A model link drains at 1.25 times the layer's target and
queues 250 ms of itself before it drops a picture (a router's buffer); the report gives the
longest wait and the pictures dropped. It is a model, not a measured network: what the receiver
does after a drop (a keyframe request, a freeze) is not in it. Scenes: *typing* (a line being
typed), *slides* (the whole page changes every 2 s), *scroll* (a fast steady scroll), *camera*
(a swaying person, sensor noise) and *camera-cut* (the whole scene changes every 3 s).

- Browser: `dev/screen-codec-bench/` (`make_peaks.py`, `server.py`, `peaks_table.py`; Linux with
  `BENCH_CHROME=/usr/bin/google-chrome`), WebCodecs H.264 as the call configures it, 240 to 300
  frames at 30 fps.
- Desktop: `screen_bench` (`--scene slides`; the report ends with the peaks), OpenH264 through
  the call's `SimulcastEncoder`, 12 s, `--frames every`. It runs the preferred hardware encoder
  too, on a Mac or with VA-API, by leaving `--encoder` out. `camera_bitrate` reports the same
  peaks for the camera layers (`examples/common/peaks.rs`).
- Through the call: `screen_uplink_bursts` in the SFU's tests sends slides through a presenter
  uplink limited like a bottleneck (250 ms queue), with the real SFU and estimator in between,
  and times each slide until the viewer shows it.

```sh
cd dev/screen-codec-bench && python3 make_peaks.py
BENCH_CHROME=/usr/bin/google-chrome python3 server.py chrome peaks.json peaks.jsonl
python3 peaks_table.py peaks.jsonl
cargo run --release -p teamcraft-media --features gstreamer,openh264-source --example screen_bench -- \
  --profile detail --source 2560x1440 --scene slides --seconds 12 --encoder software
```

Chrome 156 on Linux (software H.264, `bitrateMode` unset; the layer's own target):

| Scene, layer             |       Target | Largest picture | Busiest 100 ms | Busiest 1 s | Model link: longest wait, dropped |
|--------------------------|-------------:|----------------:|---------------:|------------:|----------------------------------:|
| typing, 1440p            |  6636 kbit/s |           < 0.1 |           0.01 |        0.09 |                           0 ms, 0 |
| slides, 1440p            |  6636 kbit/s |            10.2 |           3.39 |        0.34 |                         250 ms, 3 |
| scroll, 1440p            |  6636 kbit/s |             2.1 |           1.34 |        1.00 |                          55 ms, 0 |
| slides, 720p (half size) |  1659 kbit/s |            17.3 |           5.77 |        0.58 |                           0 ms, 4 |
| scroll, 720p (half size) |  1659 kbit/s |             2.3 |           1.24 |        0.94 |                          62 ms, 0 |
| slides, 4K               | 14930 kbit/s |             7.9 |           2.64 |        0.27 |                         211 ms, 0 |
| camera, 720p             |  1500 kbit/s |             0.8 |           0.80 |        0.73 |                          23 ms, 0 |
| camera, 360p             |   375 kbit/s |             1.3 |           1.15 |        1.02 |                          34 ms, 0 |
| camera-cut, 720p         |  1500 kbit/s |             7.0 |           3.41 |        0.92 |                         215 ms, 0 |
| camera-cut, 360p         |   375 kbit/s |             3.3 |           1.76 |        1.08 |                          88 ms, 0 |

OpenH264 through the call's encoder (`screen_bench`, detail profile, a 1440p display, every frame
encoded, release build):

| Scene, layer             |      Target | Largest picture | Busiest 100 ms | Busiest 1 s | Model link: longest wait, dropped |
|--------------------------|------------:|----------------:|---------------:|------------:|----------------------------------:|
| slides, 720p (half size) | 1659 kbit/s |            26.4 |           8.81 |        0.88 |                    0 ms, 3 of 165 |
| slides, 1440p            | 6636 kbit/s |            14.1 |           4.72 |        0.47 |                  225 ms, 1 of 168 |
| scroll, 720p (half size) | 1659 kbit/s |            25.5 |           8.49 |        1.38 |                    25 ms, 9 of 18 |
| scroll, 1440p            | 6636 kbit/s |             0.2 |           0.10 |        0.07 |                    4 ms, 0 of 177 |
| typing (both layers)     |           – |           < 0.1 |           0.04 |        0.01 |                           0 ms, 0 |

On a Mac (#1205). VideoToolbox through the call's encoder (`screen_bench`, detail profile, a 1440p
display, 12 s; `camera_bitrate calm`, a 1080p camera's four layers at a constant bitrate):

| Encoder, scene, layer                     |      Target | Largest picture | Busiest 100 ms | Busiest 1 s | Model link: longest wait, dropped |
|-------------------------------------------|------------:|----------------:|---------------:|------------:|----------------------------------:|
| VideoToolbox, slides, 720p                | 1659 kbit/s |            28.2 |          10.94 |        1.61 |                   7 ms, 16 of 331 |
| VideoToolbox, slides, 1440p               | 6636 kbit/s |            17.3 |           6.11 |        0.62 |                  145 ms, 1 of 331 |
| VideoToolbox, slides, 720p, changes only  | 1659 kbit/s |            19.7 |           6.56 |        0.67 |                    15 ms, 6 of 12 |
| VideoToolbox, slides, 1440p, changes only | 6636 kbit/s |             5.5 |           1.83 |        0.21 |                   146 ms, 0 of 12 |
| VideoToolbox, scroll, 720p                | 1659 kbit/s |             5.5 |           2.77 |        1.03 |                  158 ms, 0 of 331 |
| VideoToolbox, scroll, 1440p               | 6636 kbit/s |            17.3 |           6.27 |        0.72 |                   30 ms, 1 of 331 |
| VideoToolbox, typing, 720p / 1440p        |           – |     28.0 / 17.5 |    9.41 / 6.15 |  0.96 / 0.63 |     6 / 23 ms, 1 of 331 each |
| VideoToolbox, typing, changes only (both) |           – |           ≤ 0.1 |         ≤ 0.03 |      ≤ 0.01 |                           0 ms, 0 |
| VideoToolbox camera, 180p                 |   94 kbit/s |             6.6 |           2.87 |        1.23 |                  175 ms, 0 of 331 |
| VideoToolbox camera, 360p                 |  375 kbit/s |             3.8 |           1.97 |        1.13 |                  102 ms, 0 of 331 |
| VideoToolbox camera, 720p                 | 1500 kbit/s |             3.5 |           1.83 |        1.12 |                   93 ms, 0 of 331 |
| VideoToolbox camera, 1080p                | 3000 kbit/s |             2.4 |           1.50 |        1.10 |                   63 ms, 0 of 331 |

The largest picture of a still screen with every frame encoded (typing) is the keyframe a rebuild
of the quality governor starts with; with only changed pictures (`--frames changes`, as
ScreenCaptureKit delivers them) nothing is rebuilt. A slide show with every frame encoded (the
worst case) makes the governor rebuild the 720p layer on most slide changes (11 keyframes in
12 s): the one row above 1.4 times the target in a second.

Chrome 156 and Firefox 159 on the Mac (`make_peaks.py`, `bitrateMode` unset; Chrome's encoder is
VideoToolbox, Firefox's by its sizes too):

| Scene, layer     |       Target | Chrome: largest, 100 ms, 1 s, link wait | Firefox: largest, 100 ms, 1 s, link wait |
|------------------|-------------:|----------------------------------------:|-----------------------------------------:|
| typing, 1440p    |  6636 kbit/s |                   0.1, 0.05, 0.49, 2 ms |                    0.1, 0.05, 0.49, 2 ms |
| slides, 1440p    |  6636 kbit/s |                 8.8, 2.97, 0.49, 235 ms |                  8.7, 2.95, 0.48, 233 ms |
| scroll, 1440p    |  6636 kbit/s |                  2.0, 1.30, 0.96, 53 ms |                   2.2, 1.34, 0.99, 60 ms |
| slides, 720p     |  1659 kbit/s |         17.0, 5.72, 0.88, 91 ms, 4 lost |          17.0, 5.72, 0.77, 99 ms, 4 lost |
| scroll, 720p     |  1659 kbit/s |                  2.8, 1.30, 0.95, 74 ms |                   2.5, 1.22, 0.91, 66 ms |
| slides, 4K       | 14930 kbit/s |                 7.3, 2.47, 0.40, 195 ms |                  7.3, 2.47, 0.40, 194 ms |
| camera, 720p     |  1500 kbit/s |                  1.1, 1.04, 1.01, 28 ms |       0.2, 0.16, 0.15, 4 ms (202 kbit/s) |
| camera-cut, 720p |  1500 kbit/s |                  2.4, 1.44, 1.04, 63 ms |                   3.1, 1.69, 1.01, 84 ms |
| camera-cut, 360p |   375 kbit/s |                  2.8, 1.64, 1.05, 75 ms |                   2.9, 1.53, 1.05, 79 ms |

Through the call (`screen_uplink_bursts`, release build; slides of text at a Retina display's
size fitted into 1440p, a change every 2 s, the first 8 s left out; the time until the viewer
shows each slide):

| Presenter's uplink   | VideoToolbox | OpenH264 | Dropped at the bottleneck (VideoToolbox / OpenH264) |
|----------------------|-------------:|---------:|----------------------------------------------------:|
| unlimited            |        16 ms |   177 ms |                                                   0 |
| 20 Mbit/s            |        62 ms |   233 ms |                                                   0 |
| 12 Mbit/s            |        94 ms |   271 ms |                                                   0 |
| 10 Mbit/s            |       110 ms |   310 ms |                                                   0 |
| 4 Mbit/s (720p only) |       217 ms |   205 ms |                                       5.7 % / 0.6 % |

The share's two layers ask for 8.3 Mbit/s. At 10 to 20 Mbit/s a slide costs the time its picture
takes on the link and nothing is lost. At 4 Mbit/s the top layer stops; what the bottleneck
dropped there were the top layer's keyframes when the uplink tried it again, not a peak of the
720p layer (its slides came within 0.22 s). A debug build with OpenH264 at 12 Mbit/s: 350 ms,
nothing dropped.

What it shows:

- **The peaks are the new picture, not the rate control.** A slide change, a cut or the half-size
  layer of a scroll (which re-keys when it can't follow the motion) sends a picture of 7 to 26 times
  a frame's share, 2.6 to 8.8 times the target in 100 ms, and at most 1.4 times in a second. The
  picture is what it takes to draw the new content; steady content (a typed line, a calm camera,
  a scroll at the top layer) stays at or just above the target in every window (camera 360p: 1.15 times
  in 100 ms, the encoder's own overshoot).
- **On a link with a quarter of headroom and a 250 ms queue** a slide change waits 210 to 250 ms
  in the queue and drops up to four pictures (the 720p layer's picture, 17 times a frame's share, does
  not fit the queue at all); a camera cut waits 90 to 215 ms. Nothing else drops anything. A viewer
  sees the slide late by a quarter second, or gets a keyframe request; the window is short and the
  next picture repairs it. The model does not tell what a real network does after a drop.
- **A cap would spend sharpness for this.** The bytes of a new slide are the bytes of its sharp
  text: a 1440p slide is about 10 times a frame's share of the layer (6636 kbit/s / 30 fps = 27.7 kB,
  so about 280 kB). A cap at 1.5 times the target per second (the issue's figure) would hold that
  back for roughly a second, and the text stays blurred for as long as the picture is spread out.
  The call also reacts to a congested uplink another way already: the layers' allocation
  (`uplink_layers`) and the estimator's probing margin are sized so that a keyframe's queue
  doesn't push a layer off (#939).
- **`bitrateMode` made no difference in Chrome's software encoder.** The runs with it unset, with
  `"variable"` and with `"constant"` sent the same rate (within 0.6 %) and the same peaks (within
  0.3 in the ratios, the 720p scroll the only one that moved at all). On the Mac, Chrome's
  hardware encoder and Firefox sent byte for byte the same in all three modes, in every scene
  (Firefox also ignored `hardwareAcceleration` in an earlier run). Safari is not measured. The
  call leaves `bitrateMode` unset. Were it
  set, a browser that refuses it must fall back through `VideoEncoder.isConfigSupported` rather than fail the call
  (the bench does: a refused config is reported as unsupported).
- **Keyframes** cost the same sharpness as the pictures after them in the software encoder: no
  cap, so nothing to turn blurry. The bench's PSNR column is not comparable on Linux (a constant
  22.4 dB in every run, a colour-range offset of the software decoder's output) nor in Firefox
  (9 to 15 dB: its decoded pictures aren't read right), so sharpness is judged from
  `screen_sharpness` and the tables above, which this change does not touch. Chrome's hardware
  encoder on the Mac makes its keyframes blurrier by itself: with one every 1.5 s, 42.5 dB against
  51.8 dB for the pictures after them (1440p, typing and slides).
- **VideoToolbox's data rate limits** (`data-rate-limits`, `kVTCompressionPropertyKey_DataRateLimits`)
  are a property of the `vtenc_h264` element and need no native call, but they cap nothing.
  GStreamer sets them only at an average bitrate (it leaves them out at a constant one, the
  cameras'), and there a screen share's slides sent the same with 1.5 times the target per
  second or 1 times per 200 ms as without (720 to 724 kbit/s for the 720p layer, the same
  keyframes; `GST_DEBUG=vtenc:5` shows them accepted), as #979 found: with `quality` set, which
  GStreamer always does, VideoToolbox ignores rate limits. Cameras need none: at a constant
  bitrate no second sends more than 1.23 times the target. Screens have the quality governor.

So there is **no cap**: the peaks are one picture long, at most a quarter second on a link that is barely
above the layer's target, and the encoders' average over a second stays at or near the target.
Through the call, a limited uplink that carries the share delays a slide by the time its picture
takes on the link and loses nothing.
A cap would blur the slide it is meant to deliver, for a problem the uplink allocation already
handles.

Cost on the call's hot path: none, no runtime code changed (the bench and its examples only).

### A camera's encoders held to their bitrate and to the machine (issue #1762)

The peaks above are one picture long. A test on 2026-11-11 found worse: a 720p Chrome fake camera
(a Mandelbrot zoom with noise) sent its 320×180 layer at 0.8 to 1.6 Mbit/s instead of about
0.15 Mbit/s, and on a 3 Mbit/s hotel link that layer alone took half the link and pushed the loss
to 17 %. The cause is in Chrome, not in our configuration. WebCodecs gives its H.264 software
encoder (OpenH264) the target bitrate in `RC_BITRATE_MODE` whatever `bitrateMode` says, but
frame skipping stays off (on only behind a feature flag and only in `realtime` latency mode; a
Mac's camera runs in `quality`). OpenH264's quantizer has a ceiling, so at 30 fps a
150 kbit/s layer has about 600 bytes per picture, and no noisy picture fits in that. Without
skipping, the encoder sends every picture at the ceiling's size. The desktop's OpenH264 keeps
frame skipping on and holds its target. With a hardware encoder on Windows (Media Foundation),
Chromium's source maps `variable` (the default) to a peak-constrained VBR, and WebCodecs sets its
peak at ten times the target. That path wasn't measured.

So the browser's camera is governed (`camera_governor.rs`, wired in `call_web/video.rs`; screen
shares are not, #1733):

- **Per layer:** if a layer sends more than 2× its bitrate over a 2 s window, its encoder is first
  configured at half its bitrate, then the layer gets every 2nd, 3rd and 6th frame, never below
  5 fps. A lax encoder follows the lower bitrate. One that can't shrink a picture sends fewer
  pictures. The 180p layer above ends at 5 fps and about 200 kbit/s.
- **Encoder load:** if more than 10 % of the frames find an encoder's queue full, or frames wait
  in the queues on average, the camera's capture drops to ⅔ and then ½ of its frame rate (not
  below 10 fps). After that, the top layers are left out one at a time (`CallEngine::limit_own_layers`).
  Receivers move down as they do when the uplink pauses a layer. Frame rate goes first because it
  also saves capture and background-effect work. A window past a threshold by less than twice it
  (and with encoding within the frame interval) steps down only when the window before was
  overloaded too (#1792): the keyframes of a layer switch fill a queue for a moment, and one such
  window (a decoder queue of 1.5 at a join) sent a viewer to 480p, where failed step ups held it
  for minutes.
- **Recovery:** both step back up in reverse order after two calm 2 s windows in a row (about
  4 s). A layer climbs only when its output, scaled to the next step, stays
  under 1.25× its bitrate. A step up that is undone within 5 s doubles the wait for the next, up
  to 8 s (#1792: at 10 s doubling up to 2 min, a share pushed down by a false overload stayed
  small for minutes).

The stats overlay shows what holds the camera back ("Held back"). Every threshold is a field of
`CameraGovernorConfig` with these defaults. Cost on the hot path: a few comparisons per frame and
per encoded picture, plus a deque of at most one window of picture sizes per layer. The budgets
above don't change.

### A receiver that can't decode what it gets (issue #1762)

The other side of the same tests: a loaded Windows VM showed "dropped 700" on a received share.
Its decoders fell behind, frames piled up in them, and the SFU went on sending the top layer
because nobody upstream knew. The picture froze and stuttered, and part of the "loss" in the
stats came from the page's own receive path, not from the link.

The browser's receive side is governed now (`receive_load.rs`, wired in `call_web.rs`), with the
camera's judge (`EncoderLoad`) and its timings:

- **Signals, all of this device:** frames waiting in a decoder beyond the one or two its pipeline
  holds while it keeps up (WebCodecs `decodeQueueSize` less 2, overloaded at 1 on average over
  two 2 s windows in a row, or at 2 over one; #1792: Firefox holds 0.3–0.9 frames at 480p on an
  idle machine, and counted as queued that kept its viewers at 480p for minutes), and frames that
  reach the decoder late, measured
  from the moment the datagram that completed them was read (overloaded past 0.9 of the frame
  interval). A frame that waited for a resent packet isn't late: its lateness starts when that
  packet came.
- **Step down:** one layer of one stream per step, the least important first: the smallest camera
  tile, then larger ones, a screen share last. Through the same `Subscribe { max_layer }` the
  tiles' sizes use; the SFU sends the lower layer from its next keyframe.
- **Recovery:** back up in reverse order after two calm 2 s windows; a step up undone within 5 s doubles
  the wait, up to 8 s.
- **Not the network:** datagrams carry the time they were read into the call's loop, and the
  transport feedback reports that, so a busy loop's backlog doesn't read as a congested downlink.
  The step down only changes what this device asks for: senders and other receivers keep theirs.
- **Hidden tiles:** a tile laid out at zero size decodes nothing (its decoder closes) and asks for
  a keyframe when it shows again.

The call says "Your computer is busy: video quality reduced" while it holds video back, and the
stats overlay shows the decoder's queue and "limited by this device". Cost on the hot path: one
`decodeQueueSize` read and a few additions per received frame, and a short list of the watched
tiles per 100 ms timer tick (a sort of them every 2 s).

The native driver (`call_native.rs`, its `receive` module; issue #1775) does the same with the
same `ReceiveLoad`, for the desktop and the headless client:

- **Arrival:** the connection's reader stamps each datagram when it reads it, and the engine gets
  that time (`handle_datagram`); a frame carries the arrival of the datagram that completed it
  (`ReceivedVideoFrame::arrived_at`).
- **Where the decoders are:** the driver's own decoder thread (`VideoReceiver`) reports the frames
  waiting for it. The desktop hands frames to its webview (WebCodecs) or its JPEG transcoder and
  reports each through `DecodeReporter`: the lateness at that hand-over, and the frames waiting in
  the decoder. The transcoder counts them; the webview tells its `decodeQueueSize` when it
  changed, at most every 250 ms (`call_video_decode_queue`), so a calm call costs no calls.
- **Hidden tiles:** the webview tells a zero size while the window is minimized or hidden to the
  tray (`visibilitychange`) or the tile is laid out at zero size (a `ResizeObserver`). The driver
  then drops the track's frames, and once it shows again asks for a keyframe and drops deltas
  until it comes.

Cost: an atomic counter per frame and handed-over frame, one command per frame to the call task,
and the same per-tick judgement as in the browser.

### Each picture with its own frame (issue #990)

The first VideoToolbox session of a process takes 55 to 71 ms for its first picture (a 1440p or
4K screen layer, a 720p camera; later sessions 12 to 35 ms), longer than the 50 ms the encoder
waited. That picture then came out with the next frame, and every picture after it too, for as
long as the encoder lived: a camera's pictures a frame late, a still screen's latest change until
the next picture pushed it out, up to a second later. Measured per frame by reading the frame
counter of `test_pattern` from the decoded pictures (`every_hardware_picture_shows_its_own_frame`
in `video_gstreamer.rs` fails on the old code when it runs alone, so that its camera is the
process's first session):

| Encoder (first of the process)  | Before: frame shown for frames 0, 1, 2, … | After          |
|---------------------------------|-------------------------------------------|----------------|
| 1440p screen layer, 30 fps      | –, 0, 1, 2, … (first wait 60 ms)          | 0, 1, 2, … (55 ms) |
| 1440p screen layer, once a second | –, 0, 1, 2, … (each change a second late) | 0, 1, 2, …    |
| 720p camera, 30 fps             | –, 0, 1, 2, … (first wait 62 ms)          | 0, 1, 2, … (71 ms) |
| Later encoders, rebuilt ones    | 0, 1, 2, …                                | 0, 1, 2, …     |

Now a pipeline's first picture may take 500 ms; a later one that misses its 50 ms is dropped when
it comes, and the next frame is a keyframe (the pictures after it would refer to the dropped one).
Waiting for each picture made the layers' times add up, which the late encoder had hidden: a
simulcast frame now hands every layer its frame first and then takes their pictures, so the
layers encode at once. `screen_bench`, 8 s per row, before and after, with the process's first
VideoToolbox session the share's ("cold") and with one started before it ("warm", a camera on):

| Profile, scene                | Before cold      | Before warm       | After cold       | After warm       |
|-------------------------------|------------------|-------------------|------------------|------------------|
| detail (1440p30), scroll      | 5.4 ms, 30.0 fps | 24.4 ms, 29.9 fps | 17.0 ms, 29.9 fps | 9.4 ms, 30.0 fps |
| detail, still                 | 6.0 ms, 30.0 fps | 24.1 ms, 29.9 fps | 17.6 ms, 29.9 fps | 9.5 ms, 30.0 fps |
| motion (1080p60), scroll      | 3.9 ms, 60.0 fps | 14.3 ms, 54.4 fps | 8.9 ms, 59.9 fps | 5.6 ms, 59.9 fps |
| motion, still                 | 4.3 ms, 60.0 fps | 13.2 ms, 59.2 fps | 10.7 ms, 59.9 fps | 5.7 ms, 59.9 fps |
| 4K detail (4K30), scroll      | 12.7 ms, 30.0 fps | 8.0 ms, 30.0 fps | 29.5 ms, 24.9 fps | 24.6 ms, 28.8 fps |
| 4K detail, still              | 16.4 ms, 30.0 fps | 44.1 ms, 17.8 fps | 28.3 ms, 24.7 fps | 29.1 ms, 25.7 fps |
| detail, `--frames changes`    | 7.9 ms, 9.1 fps  | 25.3 ms, 9.1 fps  | 15.1 ms, 9.1 fps | 19.3 ms, 9.1 fps |
| 4K detail, `--frames changes` | 9.2 ms, 9.1 fps  | 17.7 ms, 9.1 fps  | 31.2 ms, 9.1 fps | 20.2 ms, 9.1 fps |

The time is the wall time of a frame's encoding; the CPU time per frame stays as it was (0.9 to
2.3 ms, 2.5 to 5.4 ms with `--frames changes`). Before, the frame rate depended on whether a
layer's first picture had missed its wait (the cold 4K rows, and the warm 4K scroll, whose 4K
layer is rebuilt and missed it then). VideoToolbox takes 20 to 41 ms for a 4K picture, more than a
frame at 30 fps, so a 4K share whose every frame changes now reaches 25 to 29 fps: keeping 30 would
need two frames in the encoder at once, with each picture sent when it comes out rather than with
the next frame.

## Clients

How fast and how lean the apps are for the person using them; the budgets they must stay within are
in [.claude/rules/robustness.md](../.claude/rules/robustness.md) ("Performance and resources").

### Numbers from real use (issue #1521)

The measurements below are taken on purpose, on test machines. What real users see comes from the
apps' performance data, sent with the crash reports to the same backend
([crash-reports.md](crash-reports.md), "Performance data"): **GlitchTip → Performance**, one row
per transaction name, with its p50/p95, throughput and the slowest samples.

| Transaction             | Budget it checks (robustness.md)                    | Apps                         |
|-------------------------|-----------------------------------------------------|------------------------------|
| `start.channel_list`    | Signed-in start until the channel list is usable    | All                          |
| `channel.switch`        | Channel switch, p95                                 | All                          |
| `conversation.catch_up` | (none yet) open or foreground until up to date      | All                          |
| `message.send`          | (none yet) send until the server took the message   | All                          |
| `sync.reconnect`        | (none yet) connecting again until connected         | All                          |
| `file.download`         | (none yet) an attachment's download and decryption  | Web, desktop                 |
| `call.setup`            | (none yet) joining until the media is secured       | Web, desktop                 |
| App start, `ui.load`    | Signed-in start (mobile), slow and frozen frames    | Android, iOS (the SDKs' own) |

How to find a cause, not only a number:

- **Open the slowest samples** of a transaction: on mobile its steps are the core's jobs. A long
  `queue.wait` with a high `teamcraft.queue_depth` means the operation waited behind other jobs
  (the steps next to it name them); a long `core.read` or `core.engine` means the job itself was
  slow; `store.lock_wait` means it waited for another thread's writes, `store.write_group` how long
  those held the store.
- **Issues → "Slow core job: …"** lists jobs over the threshold even when no measure was open,
  with their wait, run and queue depth as tags.
- **Compare tags**: `start` cold against warm, `entry` (a notification start slower than a launcher
  start), `start_cache` miss (the first frame had nothing to show), `activities` above 1 (copies of
  the activity), `catch_up` by `source` and `gap_fill`.
- Filter by the tag `app.platform` (`web`, `desktop`, `android`, `ios`) and by release
  (`teamcraft@<version>`): a regression shows as a step in p95 from one release to the next.
- A transaction's steps (spans) say where the time went: the start's parts (the same names as
  `start_timing`, "Web start on phones" below), a channel switch's read from the core's store
  (`db.query core_messages`, tag `source` = `store`; without it the rows were in memory).
- The share of operations measured is the instance's "trace rate" (Administration → Crash
  reports), 100 % while the team gathers data; lower it as an instance grows. No rebuild needed.

### DPoP (issue #120)

What proofs cost a client with a bound token (e2ee.md §3.1.2); a client with an unbound token pays
nothing. Measured 2026-11-03 on one machine, by `dpop_clients.rs` (`measured`, with `--nocapture`) and
`tests/e2e` `dpop_web.rs`. The server and the native client are debug builds, so these are upper
bounds for the signature and for the server's check.

| What                                                    | Time     |
|---------------------------------------------------------|----------|
| `GET /me` on loopback, bearer token                     | 1.7 ms   |
| The same, bound token, nonce known                      | 5.7 ms   |
| The same, bound token, first request with its challenge | 10.8 ms  |
| One proof, software key (desktop)                       | 1.3 ms   |
| One signature, Web Crypto (Chromium)                    | 0.04 ms  |
| Reload to the channel list in Chromium, bound session   | 546 ms   |
| Reload to the channel list in Chromium, unbound session | 570 ms   |

The two reloads are medians of five and within each other's noise; the budget is 1.5 s.

**The extra round trip.** A request whose proof has no current nonce is answered with a challenge and
sent again: one round trip of the network more, plus one more signature and check. It is rare by
design. The start doesn't pay it: the nonce comes from `server-info`, next to the token refresh on the
desktop and before it in the browser, so the request that confirms the account goes out with it. A
challenge is met after a server restart, after more than two nonce rotations without a request (the
default rotation is in `DEFAULT_DPOP_NONCE_ROTATION_SECS`), and when a cache on the way holds an old
`server-info`. Requests that go out together each meet it once. On the gateway or the
relay it is one more connection, so a reconnect then takes one more handshake. A proof adds about
600 bytes to a request.

### Background effects in the browser (#439)

Local bench page with the call module's code paths (`call_web` camera effects, the segmentation
worker built like the release), MacBook (Apple Silicon), a 720p `<video>` in, strong blur, one
picture every 33 ms, encoded with WebCodecs H.264. Main thread per frame:

| Browser     | Compositing | Main thread per frame | Of that                                                   | Segmentation (worker) |
|-------------|-------------|-----------------------|-----------------------------------------------------------|-----------------------|
| Chrome 156  | WebGPU      | 5.4 ms                | getImageData 1.4, RGBx→I420 1.9, GPU and read-back 1.6    | 7.4 ms, every frame   |
| Safari 26.6 | WebGPU      | 6.2 ms                | getImageData 2.3, RGBx→I420 2.1, GPU and read-back 1.4    | 8.2 ms, every frame   |
| Firefox 158 | WebGL2      | 5.3 ms                | texImage2D 0.4, model input 1.2, `VideoFrame(canvas)` 3.5 | 11.0 ms, every second |
| Firefox 158 | WebGPU      | 103 ms                | read-back 93                                              | –                     |

- The segmentation runs in its worker, as often as keeps it within 8 ms a frame on average
  (`MaskCadence`); the main thread only hands it the model's 256x144 input. Before the worker it
  ran on the main thread: 14.6 ms a frame in Chrome with the WebGPU compositor, 13.3 ms in Safari.
- WebAssembly SIMD (`+simd128`) for the segmentation module, in the worker with one picture a
  frame: 8.6 instead of 15.0 ms in Chrome, 9.2 instead of 15.3 ms in Safari, 11.4 instead of
  19.8 ms in Firefox 147.
- RGBx→I420 has a function of its own for the browser (`capture_convert::rgbx_to_i420`, whole rows
  and 2x2 blocks): 1.9 ms instead of about 30 ms with the general per-pixel path in the
  size-optimized call module.
- WebGL2 in Chrome and Safari would cost 1.1 and 4.2 ms a frame on the main thread (Chrome uses it
  since #1336, see below).
- A CPU compositor for Firefox would pay getImageData and RGBx→I420 alone with 9 ms a frame there,
  more than the whole WebGL2 path. Firefox 147 measured 7.0 ms with WebGL2.
- Since #1295 the WebGPU compositor takes the canvas picture as RGBA and writes I420 itself
  (`Compositor::apply_rgba`): the composite passes, which converted their result to I420 anyway,
  read RGB straight from the picture, so the CPU's RGBx→I420 (1.9 ms in Chrome, 2.1 ms in Safari)
  is gone and the picture is read back once, as before. The upload is 3.7 instead of 1.4 MB a 720p
  frame. A picture too large for one RGBA buffer of the GPU (beyond 4K at WebGPU's default limits)
  is still converted on the CPU first. Natively the GPU's I420 is the CPU's within rounding
  (tested). `copyExternalImageToTexture` from the `<video>` would also save the getImageData, but
  wgpu panics where the browser throws on that call, which would take the call module down.

#### WebGPU against WebGL2 (#1336)

The effects bench (`dev/effects-bench`: `build.sh`, then `server.py`; the module is
`crates/teamcraft-media/examples/effects_bench.rs`) runs the call module's pipeline
(`call_web::TimedEffects`, the segmentation worker and module as the release builds them) on a
720p test picture (a canvas stream in a `<video>`), strong blur, one picture every 33 ms, 165
pictures, each encoded with WebCodecs H.264 like a call's (skipped while more than two wait).
`server.py --browser chrome` starts a Chrome of its own and adds the CPU time of all its processes;
`table.py` prints the results. Same MacBook, Chrome 157, the WebGPU path after #1295, three runs of
each in one browser; per picture:

| Compositing | Main thread (mean / p95) | Picture | Model input | Composite | Frame | Whole browser CPU |
|-------------|--------------------------|---------|-------------|-----------|-------|-------------------|
| WebGPU      | 4.0–4.3 / 4.6–5.3 ms     | 1.2–1.5 | 0.2         | 2.4–2.5   | 0.1   | 15.4–15.9 ms      |
| WebGL2      | 1.5 / 1.7–1.8 ms         | 0.2     | 1.2–1.3     | 0.1       | 0.0   | 12.9–13.4 ms      |

- "Picture" is drawImage and getImageData (WebGPU) or texImage2D (WebGL2), "model input" the
  256x144 copy for the worker (the shrink, or readPixels), "composite" the GPU passes (with WebGPU
  including the upload and the read-back as I420, awaited), "frame" the `VideoFrame`. The worker's
  segmentation took 7.0–7.2 ms on about 151 of the 165 pictures either way. The whole browser's
  CPU includes the test picture's drawing, the worker and the encoder.
- The first run in a fresh Chrome, whichever the compositing, encoded only 66–71 of the 165
  pictures: the encoder's first chunk came 3.4–3.5 s after its first picture (opening the hardware
  encoder), and the pictures meanwhile were skipped because its queue was full. Every later run
  encoded all 165, its first chunk after about 20 ms. That was #964's Chrome WebGL2 run with 60 of
  165 frames: not the compositing.
- So Chromium browsers (Chrome, Edge, …) composite with WebGL2 since #1336
  (`page_calls::browser_effects_compositing`), like Firefox. Safari keeps WebGPU until it is
  measured with the new path (#964: WebGPU 6.2 ms with the CPU's conversion, WebGL2 4.2 ms).
- To compare on another device: `localStorage.setItem("teamcraft.effectsTiming", "1")` makes the
  call module log a summary of the same steps every 300 pictures in a call or the pre-join check
  (console, `effects timing`; also `globalThis.teamcraftEffectsTiming`), with the encoders' frames
  and those skipped; `localStorage.setItem("teamcraft.effectsCompositing", "webgpu")` (or
  `"webgl2"`) picks the compositing where the browser has it. Nothing is sent anywhere.

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

#### Scrolling back through held history (issue #1050)

The scroll back above never left the newest page: a browser that joins a channel holds only its
newest messages (the core loads one page of history), so the 1,500-message channel was about 50
rows in the browser. The desktop app, open for weeks in a busy channel, holds all of it and reads it
from the core a page of 100 at a time while the reader scrolls up; that is where scrolling stalled.
`perf.rs` now also lets Carol be away while the long channel grows by 1,500 messages, so the
catch-up stores them all, and scrolls up from the newest one at a brisk, steady wheel speed (60 px
a frame, about 3,600 px a second, 400 frames, several pages). Release web build in headless Chrome,
the machine above with other builds running (load 3 to 6), CPU profile of the scroll:

| While scrolling 400 frames                           | Before  | After   |
|------------------------------------------------------|---------|---------|
| Main-thread time of the app                          | 2.44 s  | 1.73 s  |
| Of that in `document.execCommand` (forced layout)    | 0.85 s  | –       |
| Frames with a placeholder row in sight               | 0       | 0       |
| Frames over 33 ms / longest frame                    | 1 / 33  | 1 / 33  |
| Tasks over 50 ms                                     | 0–1     | 0       |
| Frames in which the list moved backwards             | 0       | 0       |

What the numbers say, and what changed:

- In the browser the core runs in the page and answers a page within the frame, so nothing waited
  for messages even before; the stall there was work on the main thread when a page arrived. On the
  desktop the answer crosses the shell's IPC from the core thread, which may be busy, and the
  WebKitGTK webview lays out slower: there the same work showed as a stop.
- Each page the list read told the whole UI to read again (one change notification for the app),
  and with no draft for the conversation, the composer answered every such notification with two
  editing commands that make the browser lay out the page. The composer prepares its editor once
  now, and a page loaded ahead tells the UI nothing unless a row on screen waits for it: the rows
  read it from the mirror when they come into view.
- The list asks for the rows four viewport heights beyond what it renders
  (`ChatBackend::prefetch_messages`), so a page is in the mirror before it is reached, also over a
  slow IPC answer. With 22 px log lines that is about 150 rows ahead.
- A refresh after a change in the core (a new line in a log channel) reloaded every page the
  mirror ever read: the further back the reader had scrolled, the more pages every new line cost.
  It now reloads the pages read or asked ahead for since the last change and drops the others.
- Android and iOS show the newest 200 messages without paging back, so they have no page boundary
  to stall at.

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

#### Opening the store and the first catalog build (issue #1127)

The large workspace on this machine (Linux, Ryzen, headless Chrome, release server and web build),
November 2026, other sessions' builds at times beside it (load 1 to 3). One run on `main` a7182eb2
plus only the measuring (`perf.rs`, one log line), and two of the change. `perf.rs` forces the build
by deleting the catalog's state from IndexedDB, so the next start builds it under a new generation,
as an update that changes the catalog does.

| Large workspace                                   | Before    | After, run 1 | After, run 2 |
|---------------------------------------------------|-----------|--------------|--------------|
| Build: page load until the channel list           | 11.8 s    | 18.7 s       | 18.1 s       |
| Build: longest main-thread task                   | 10,974 ms | 928 ms       | 904 ms       |
| Build: tasks over 50 ms / their total             | 3 / 11 s  | 92 / 12.9 s  | 91 / 12.5 s  |
| Build: steps the progress bar showed              | none      | 89           | 87           |
| Reload during the build: percent before, after it | -         | -            | 36 / 37      |
| The same: from the reload until the channel list  | -         | 24.6 s       | 21.9 s       |

| The store, on a reload                             | Value                   |
|----------------------------------------------------|-------------------------|
| Records / their ciphertext                         | 23,487 / 22 MiB         |
| Reading them, before anything else (`read_all`)    | 350 ms                  |
| Wasm memory right after that                       | 29 MiB                  |
| Reload until the channel list                      | 1.6 s (budget ≤ 1.5 s)  |
| PSS of the browser: empty / app started            | 294 / 577 MiB           |

What the numbers say:

- **The page keeps its thread:** the one task of 11 s is gone. The build takes one batch per task
  and waits for its commit. Most tasks still run over 50 ms (median 133 ms, up to 193 ms, longer as
  the indexes grow), so the bar moves a few times a second; the 904 ms is not a batch, a plain reload
  has one of that size too (777 ms). On this screen nothing but the bar and "Sign out" responds, and
  it happens once per update.
- **It costs about 6 s more (+53 %) here:** the commit after every batch, and before it the writes
  went out in far fewer transactions. Not profiled. A smaller batch would mean shorter tasks and more
  rewrites of the same index nodes; that trade was not tried.
- **A reload goes on:** the bar stood at 36 % in a screenshot just before the reload and came back at
  37 %. The rest then took longer than a whole build from the start (21.9 s for about 64 % against
  18.1 s), which is not explained.
- **The waiting costs no timers:** the commit is what gives the thread back, so a hidden tab's
  throttled timers don't slow the build. Not measured in a hidden or frozen tab.
- **This store is small for the workspace:** 100k messages on the server are 23,487 records here,
  because a browser holds what it caught up (one page per channel, plus what it read). A store that
  really holds 100,000 messages would be about four times this if it grows linearly (about 1.5 s and
  90 MiB); that is an estimate, no such store was measured.
- **Reading a page at a time was tried and dropped:** `getAll` with a limit, in one transaction, read
  in 405 ms instead of 350 ms in both runs, with the same three long tasks on a reload and the same
  wasm memory, so it bought nothing measurable on a hot path over its budget. The tasks of the reload
  before the change were not recorded separately, and a larger store may show a difference. The
  columns "After" were measured with it; the code is back to the single read of "Before", and that
  combination has no large run of its own. It changes only the first 400 ms of a start.
- **Not the change, as far as the code says:** channel switches at p95 while catching up went from
  126 to 164 and 152 ms, right after it from 75 to 100 and 93 ms, and one long channel open from 38
  to 130 ms in the second run. Nothing after opening changed; one baseline run can't rule it out.

**Fetching records per conversation at the async boundaries: not sound, so not done.** `LocalStore`
is synchronous, and IndexedDB has no synchronous read, so what a read may touch has to be in memory
before it starts:

- A record that isn't loaded can't look absent: `upsert_message` would take an existing message for
  new and leave its old place in the indexes. It would have to fail instead.
- It can't fail either: a group of writes can't be undone in the browser (the backend keeps every
  write, and `Client` state has moved on), so a read that fails in the middle of a group leaves half
  of it applied, MLS state included.
- So every path would have to name its records up front. About 128 calls read or write messages in
  the client and the UI backend alone, a gateway frame names its conversation only once it is parsed,
  a catch-up crosses conversations, record IDs are pseudorandom (so a conversation's records are found
  only through its sealed index, one step after another), and a repair of the catalog or a removed
  conversation's sweep walks every record, synchronously, from inside a read.
- A key prefix per conversation would make the fetch easy, and would show in one copy of the files
  which records belong together, more than e2ee.md §9.1 allows today.

What it would take, either of: the core in a dedicated worker over a store with synchronous reads
(OPFS sync access handles), which gives the browser what the mobile apps have, O(1) open, and takes
the build and the catch-up off the main thread; the `remote_backend` calls are already the boundary
for that. Or an async `LocalStore`, which rewrites the client on every platform. At the numbers above
(350 ms of a reload, 22 of 283 MiB) neither is urgent for this workspace; it matters for a browser
that holds far more history. The service worker reads the whole store for every push, too.

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
- The budget test fails above 19 requests (18 plus the one user-groups read of #1527) or 300 ms (1.75 times that on a shared CI runner) for an unchanged workspace, also at 30 ms RTT.

Left open (see the PR of #714/#717):

- The web and desktop shells restored the sign-in and read `GET /me` before they started the core.
  Fixed since, see "Starting from the cached account" below.
- The web core decrypts the whole IndexedDB store into memory before it answers the first read.
  (Since #155 it only reads the ciphertext; #1127 says what fetching less would take.)
- The MLS state was still one record (#424); a catch-up of many changed groups rewrote it per group.
  Fixed since, see the next section.

The desktop and Android apps are measured against the same harness (`PERF_MOBILE_USER`) in a local
session. Slack, Teams and Element were not measured: that needs their apps installed and accounts
on their services.

#### Starting from the cached account (issue #778)

Both shells waited for the token refresh and `GET /me` before they started the core, so every start
paid for the network, and offline the web app showed the sign-in page instead of the stored chats. Now
the core opens the local data of the account the server confirmed last, and the session is checked
beside it. What decides what happens to the session is in [e2ee.md](e2ee.md) §9.8.

Signed-in start of the web client until the channel list shows, a small workspace, debug server, the
web client's release build, local machine, November 2026, median of 5. Every start has an access
token that has run out, as after a night: the page's clock moves on by an hour per load. "Late" holds
every request to the server's API and the identity provider back for a second (DevTools), "no
answer" fails them.

```sh
CACHED_START_RUNS=5 E2E_REQUIRED=1 cargo test -p teamcraft-e2e --features browser --test suite -- \
  --nocapture cached_start::
```

| Network                | List: before         | List: after       | Connected: before | Connected: after | Budget (list) |
|------------------------|----------------------|-------------------|-------------------|------------------|---------------|
| Good                   | 624 ms               | 560 ms            | 697 ms            | 687 ms           | ≤ 1.5 s       |
| Every request 1 s late | 5717 ms              | 1591 ms           | 6779 ms           | 6502 ms          | ≤ 1.5 s       |
| No answer              | sign-in page, 462 ms | 585 ms, the chats | -                 | -                | ≤ 1.5 s       |

"After" is before the last fixes of the review, measured against "before" at the same machine load. On the final
code, with the machine busy with other builds (load 9 to 16): 617 ms list and 742 ms connected on a good network,
1643 and 6559 ms when late, 675 ms without an answer. The case without any network moved as much as the others, so
this reads as the load; "before" was not measured again under it.

"Connected" is until the core logged its gateway connection. Nothing goes out until the server
confirmed the account (e2ee.md §9.8), so that time doesn't get better, only the list does. It doesn't
get worse either: before, the core also started only after `GET /me`.

- **The second that is left when requests are late** is one stylesheet: `index.html` links
  `/api/v1/brand/theme.css`, which holds back the first paint and the app's scripts until it is there
  (the service worker leaves `/api/` alone). On a link that swallows packets instead of refusing them
  this wait lasts until the browser gives up. Not changed here: without it, a branded instance
  starts in the default colors. To decide separately.
- **The service worker serves the app's files offline only in release builds with a signed bundle**
  (e2ee.md §9.3). Other builds need the server for the page itself, so "no answer" above means the API
  and the identity provider, not the page.
- **Desktop:** the same start, through `auth_cached_account`. It reads the sealed file the tokens are
  in, so a start stays at one keychain read (e2ee.md §9.7). `CoreHost::start` doesn't wait for a token
  until the account is confirmed. Covered by tests (`token_expiry::cached_account`), not measured
  in the app.
- **Left of #778:** live events don't move the remembered sync heads (a head moved by an event says
  nothing about what came before it, so it needs proof that the connection had no gap). The rest is in
  the next section.

#### Web start on phones (issue #1465)

The start of the web app was measured part by part for the first time, because a phone showed the
loading screen for a long time. What the start spends is on the page on demand, nothing is sent:
`localStorage.setItem("teamcraft.startTiming", "1")`, then start the app again. `init.js` and the
app put `performance.mark`s `teamcraft:<part>:start` and `:end`, a `performance.measure` per part,
and at the first view a summary (`start timing: …`) into the console and on
`globalThis.teamcraftStartTiming`: each part's `ms` and `endMs` (since the navigation started) and
`firstViewMs` (`crates/teamcraft-ui/src/start_timing.rs`).

Release `trunk build` (trunk 0.22.0, wasm-opt 131, macOS), `main` a31c48b2, medians of warm starts,
milliseconds. The left pair is Safari in the iOS Simulator (iPhone 17 Pro, iOS 26.0, the Mac's CPU,
not throttled) with a release signed by a test key, so the files come from the service worker, and
a store of 30 channels and 3,150 messages. The right pair is WebKit on macOS (Playwright 1.56,
WebKit 26.0), unsigned, no service worker, and 30 channels with 21,150 messages. Neither is a phone:
the iPhone's numbers are in #1475. Both are the signed-in start from the local data, as after a
reload or the next tap on the icon, with no server behind it (`dev/start-bench`, below).

| Part (ms)                                         | Safari sim: before | after   | WebKit macOS: before | after   |
|---------------------------------------------------|--------------------|---------|----------------------|---------|
| The app's wasm: fetch (`wasm-fetch`)              | 452                | 68      | 234                  | 67      |
| ... compile after the last byte (`wasm-compile`)  | 8                  | 7       | 12                   | 6       |
| The UI's texts: fetch (`texts-fetch`)             | 389                | 6       | 199                  | 3       |
| ... reading them (`texts`)                        | 4                  | 4       | 4                    | 4       |
| The signed-in app's pack (`session-pack`)         | –                  | 6       | –                    | 11      |
| Its chunks until its first view (`session-chunk`) | 519                | 77      | 325                  | 71      |
| The cached account (`cached-account`)             | 0                  | 0       | 0                    | 0       |
| Store key (`store-unlock`)                        | 3                  | 5       | 5                    | 4       |
| Store records (`store-read`)                      | 34                 | 27      | 57                   | 59      |
| Client on the store (`client-open`)               | 17                 | 14      | 14                   | 14      |
| The core until its status (`core-start`)          | 66                 | 53      | 88                   | 85      |
| First snapshot, built (`snapshot-build`)          | 17                 | 15      | 17                   | 17      |
| ... until the UI has it (`snapshot`)              | 21                 | 18      | 20                   | 20      |
| **First view (`firstViewMs`)**                    | **1160**           | **265** | **759**              | **302** |

The parts overlap (the fetches run side by side), so they don't add up. The first view is the end of
the start: the screen hands over to it. The rest of what the numbers say:

- **The chunks were most of it.** The split leaves the signed-in app with 866 files, its own and
  the shared chunks, 669 of them under 1 kB (one per combination of split modules, #334). Each was
  a request of its own and a streaming compile, through the service worker in a signed release, and
  they were queued before the app's own wasm and texts: the texts (75 kB) took as long as the
  6.9 MB wasm, and took 3 to 6 ms with the queue gone (the wasm 67). `teamcraft-web-bundle` now
  joins them into one file, `teamcraft-ui-session-<id>.pack`, in the signed release like any other
  file and checked by the worker before it is served. The chunk loader cuts the files out of it, by
  offsets in the (signed) loader script. Tiny parts compile right there, synchronously, in the same
  turn; the few large ones go through `WebAssembly.instantiate` off the main thread. A part not in
  the pack, or a page whose pack didn't arrive, fetches its file as before, so the guest's page and
  the failure path stay the same. Compressed as one, the download shrinks too (below).
- **The first paint waited for the network.** `index.html` links the brand's colors
  (`/api/v1/brand/theme.css`), the page waits for them before it paints anything, and the service
  worker didn't touch `/api/`. With every API request 2 s late the first paint came after 2,015 to
  2,070 ms, before the loading screen showed, in WebKit on the sign-in path (it doesn't show in the
  table: that server answers at once, and `first-view` is timed by script, which keeps running).
  The worker now answers that one file from the copy of the last start and fetches the fresh one
  for the next: first paint after 9 to 13 ms (signed-in: 15 to 26 ms), with the same 2 s delay, from
  the start after the worker has its copy. A change of the brand's colors shows one start later.
- **The rest of #778's list:** the start doesn't wait for the token or `GET /me`. Refused API
  requests didn't change the first view, and with every API request 2 s late the signed-in first
  view came at 198 to 249 ms (WebKit, signed, 10 channels). The web core doesn't decrypt the store
  before the first read either, as #155 described: `store-read` is the ciphertext only, 57 ms for
  21,150 messages' records on this machine. What is still O(store) is that read, and the sidebar's
  counts for a store that has none kept: 21,150 messages counted once are 200 ms here, written then
  (`entry_tally.rs`), and read back in 17 ms from the next start on. A start interrupted within the
  first moments writes none, which is why a bench that reloads at once keeps paying it.
- **Compile:** not where the time goes. In WebKit on this Mac `WebAssembly.compile` of the 6.9 MB
  app takes 11 to 13 ms and `compileStreaming` 49 to 64 ms with its fetch, the same on every page
  load: WebKit keeps nothing between loads that shows here, and there is nothing for a cache to
  save. The app already compiles while it streams (`instantiateStreaming`). The pack leaves that as
  it was and removes the cost that grew with the number of files, not their size.
- **What the phone adds** isn't in these numbers: a phone's CPU and storage are slower by a factor
  only the phone can say (#1475), and it may reach the server slowly, which no longer holds
  anything back in a signed release.

The download of a signed-in first visit shrinks too (gzip -9, all of it, same build):

| Download                                   | Before      | After       | Delta      |
|--------------------------------------------|-------------|-------------|------------|
| Signed-in first visit                      | 5,432,009 B | 5,168,840 B | −263,169 B |
| The signed-in app's files (866 / one pack) | 2,231,031 B | 1,967,862 B | −263,169 B |

To measure the same: build the release as above, then the seed and the bench page, and serve them
(`dev/start-bench`; Playwright with its WebKit, or Safari in the iOS Simulator with
`xcrun simctl openurl booted "http://127.0.0.1:8091/__bench/index.html?frame=10"`, which reports the
timing to the local server, the page's console being out of reach):

```sh
cargo build --example web_start_seed -p teamcraft-core --target wasm32-unknown-unknown --profile web-release
wasm-bindgen --target web --out-dir /tmp/bench --out-name web_start_seed --no-typescript \
  target/wasm32-unknown-unknown/web-release/examples/web_start_seed.wasm
cp dev/start-bench/index.html /tmp/bench/
python3 dev/start-bench/serve.py crates/teamcraft-ui/dist /tmp/bench 8091 /tmp/timing.jsonl
node dev/start-bench/measure.js http://127.0.0.1:8091 cached 8 30 5 700   # needs playwright; channels, messages, history
```

`--api=delay:2000` on `serve.py` answers every API request 2 s late; sign a copy of the build
(`crates/teamcraft-ui/scripts/sign-web-release.sh`, a key of your own) to run it through the service
worker. The first start with empty caches is left out of the medians: it took 98.8 s in the
simulator before and 0.68 s after. Before, 93 s of that were the texts waiting behind the 866 other
requests of this local test server, so read it as the shape of the problem, not its size.

#### Own changes show at once (issue #778)

A sent message reached the UI's mirror only after the core had answered, and then after two more
round trips (what changed, the page of messages): a request and an answer through the shell on
desktop, through the core's loop in the page on the web. On the web an edit, a delete, a reaction or
a pin waited for the end of a connection that was being made, and read markers still do.

Now the UI picks the message's ID (`OutgoingMessage::maybe_client_message_id`), puts the row into the
feed in the same call (`RemoteChat::show_pending`), and the core keeps that ID, so its answer
replaces the row in place (`pending_message_id` is the one function that derives the row's ID from the
client ID, for both). A send the core refuses takes the row back, and the
same ID sent twice is one message, also when the first has been sent by then. A message that needs
more than its text for its row (a reply's quote, files, urgency, silent) appears with the core's
answer as before. Edits, deletes, reactions, pins and forwards are outbox writes like a new message
(`ChatAction::is_outbox_write`): the core answers once they are stored and sends them afterwards,
also while a connection or an upload holds the client.

Web client (release build), a small workspace, debug server, local machine, November 2026. The test
types a message and presses Enter, 20 times; the page notes when the key event happened and when the
row with the text was added to the DOM. The second line reloads the page, so that the fresh session's
full sync starts, holds back the reads of the connection by 6 s each, and clicks a quick reaction
once the sync has started.

```sh
PENDING_ECHO_RUNS=20 E2E_REQUIRED=1 cargo test -p teamcraft-e2e --features browser --test suite -- \
  --nocapture pending_echo::
```

| What                                                    | Before                | After                 | Budget      |
|---------------------------------------------------------|-----------------------|-----------------------|-------------|
| Enter until the pending row is in the DOM, median / p95 | 35.0 / 43.9 ms        | 9.6 / 41.1 ms         | one frame   |
| Reaction while the connection waits, until its chip     | 18,063 ms             | 45 ms                 | one frame   |

"Before" is `main` 4167ebd6 with the same test (three more runs: medians 35.6, 36.7, 36.9 ms, p95 43 to
52 ms, one 213 ms). The p95 of "after" is its first send, with the page's code still being compiled;
the other 19 are 8.0 to 11.9 ms. The row is in the DOM in the same task as the key event; what is left
is the framework's work.

- **No work per conversation** on the way: a plain message reads neither the user directory nor the
  members unless it contains an `@`, and a reply, a pin, a save or an edit of a message the server
  numbered finds its target by one keyed read instead of reading the conversation. Nothing else on
  the send path changed, so the other budgets (channel switch, memory, idle) are as measured above;
  `perf.rs` was not run for this change.
- **Left:** read markers during a connection (they need the client's account data), reactions and
  edits still read the conversation to find their target and its reactions (a keyed lookup for
  reactions needs an index of them), a reply, files and urgent messages show with the core's answer,
  and live events still don't move the sync heads. Covered by tests: `chosen_ids::*` (core),
  `pending_rows::*` (UI mirror), `changes_that_live_in_the_outbox_answer_before_the_network`
  (client API) and the browser test above, which also sends offline, reloads the page with the
  message queued and checks that it goes out once when the network is back.

#### Coming back to 300 channels: no work per conversation (issue #714)

The same test on a workspace like the one after an import: 10 spaces with 30 channels each
(`SYNC_PERF_SPACES=10 SYNC_PERF_CHANNELS_PER_SPACE=30`, seeding takes about three minutes). Debug
build, local machine, November 2026; "before" is `main` 454d92c with the same test.

```sh
E2E_REQUIRED=1 SYNC_PERF_SPACES=10 SYNC_PERF_CHANNELS_PER_SPACE=30 \
  cargo test -p teamcraft-e2e --features e2e --test suite -- --nocapture sync_perf
```

| What, 300 channels                      | Requests | Before: local | Before: 30 ms RTT | After: local | After: 30 ms RTT | Budget   |
|-----------------------------------------|----------|---------------|-------------------|--------------|------------------|----------|
| Fresh session, unchanged workspace      | 16       | 314 ms        | 415 ms            | 155 ms       | 258 ms           | ≤ 300 ms |
| Fresh session, three channels with news | 22       | 337 ms        | 468 ms            | 196 ms       | 319 ms           | -        |
| Resume within the window                | 6        | 18 ms         | 80 ms             | 16 ms        | 81 ms            | ≤ 300 ms |

The requests were already independent of the workspace (16 for 24 channels as for 300); the time
was not. About 140 ms of it was local work with no request at all, and the phase trace showed it
moving from step to step as each was fixed:

- **Every MLS group was loaded to ask whether it exists.** A new client holds no group in memory;
  the first step that asked `has_group` for every conversation (deciding what is due, then joining
  missing groups, then the check for revoked devices) deserialized all 300 groups, about 0.45 ms
  each in a debug build. These checks now read the group's state entry only
  (`ConversationGroup::stored_state`: whether it exists and whether an own commit waits), and the
  revocation check looks at each group once per version of the own device list, so after that
  only groups joined since. `sync_heads::an_unchanged_workspace_loads_no_group_after_a_restart`
  fails if a sync of an unchanged workspace loads any group again, at 2 channels as at 8.
- **Unchanged conversation records were written again** at every fresh session; they are compared
  first now. On the desktop's and the phones' SQLCipher file each was a write.
- **People no list names** were asked about one after the other (up to 25 round trips, once a day
  each), after reading every stored member list. They are asked at once now, and the member lists
  are read only when somebody is left to ask.

What is left at 300 channels, 30 ms away: socket and `hello` 33 ms, `ready` 35 ms, the first reads
80 ms, local work about 60 ms (the device list check 15, joining missing groups 10, deciding what
is due 9).

**The heads answer on the server** digested the member list answer of every conversation and space
of the caller on each request: every member row, formatted and serialized. In the test above (two
people per channel) that was about 45 ms more than for 24 channels; with channels of real size it
was the largest cost of a fresh session. Each list now has a token that database triggers renew
whenever its answer would change (migration 0075, docs/gateway.md), and the digest covers the
token and the directory policy. The server's load test (`sync_heads::heads_of_a_large_workspace`,
ignored by default; debug build, dev Postgres, local machine):

```sh
DATABASE_URL=postgres://teamcraft:teamcraft-dev-only@127.0.0.1:5432/teamcraft \
  cargo test -p teamcraft-server --features integration --test suite -- --ignored --nocapture \
  sync_heads::heads_of_a_large_workspace
```

| `GET /sync/heads`, 300 channels × 200 members, space of 2,000 | Before | After   |
|---------------------------------------------------------------|--------|---------|
| Server time, p50                                              | 831 ms | 19 ms   |
| Answer                                                        | 24 KB  | 24 KB   |

The first sync after the update reads every member list once: all digests change with the new
scheme. Opening the store before the connection took about
60 ms; it is outside the measured time and belongs to #778 (the stores loading lazily).

`tests/e2e` `perf.rs` reads the same trace from the web client's console after the reload
(`reload_sync_ms`) and fails above 3 s, a bound for the shared nightly machine.

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
- What a save cost besides the writes until #840: the client compared the provider storage with what
  it wrote last, a copy of the state in memory, so 0.3 ms per save with the state of 300 groups in a
  release build, and as much memory again as the state. See "What a save costs" below.
- The catch-up time still grows faster than the number of groups (memory store, debug build: 1.3 s
  for 100, 8 s for 300). The comparison was one part of it (unoptimised in a debug build); the rest
  was not taken apart here.
- The browser's batches stay small now, so the extra wait for a batch over 1 MiB (#326) rarely
  applies; the large web workspace (`perf.rs`, `PERF_SCALE=large`) is still to be measured with it.

##### What a save costs (issue #840)

The provider storage now notes the keys it changes (crates/vendor/openmls_memory_storage/VENDORED.md),
so a save writes those: no copy of the state to compare with, and no comparison. (A sealed store's cache
stays, bounded by `SealedStoreLimits`: tiny in the extension, which is what the table measures.) Every save also bumps a generation
record, so another process's commit that left the MLS state alone (the iOS notification extension's
push bookkeeping, on every app resume) costs no rewrite. Debug build, local machine, November 2026,
the state of 300 groups (2.7 MB in 3,600 entries); "before" is the same test on c47464d1:

```sh
cargo test -p teamcraft-core --all-features --lib -- --ignored --nocapture measure_save_of_one
cargo test -p teamcraft-core --all-features --test suite -- --ignored --nocapture measure_save_after
cargo test -p teamcraft-core --all-features --test store_memory -- --ignored --nocapture measure_mls
MLS_STORAGE_CHANNELS=100 cargo test -p teamcraft-core --all-features --test suite -- \
  --ignored --nocapture measure_catch_up
```

| What                                                        | Before    | After     |
|-------------------------------------------------------------|-----------|-----------|
| A save of one changed entry, memory store                   | 1.585 ms  | 4.2 µs    |
| First save after another process's commit: bytes of entries | 2,742,997 | 27        |
| First save after another process's commit: time, sealed     | 2.69 s    | 25.7 ms   |
| Loading the state in the extension's store: peak            | 7.09 MB   | 4.37 MB   |
| Loading the state in the extension's store: kept            | 7.09 MB   | 3.97 MB   |
| Catch-up of 100 groups: bytes of MLS state written          | 1,112,708 | 1,111,240 |
| Catch-up of 100 groups: records of MLS state written        | 1,203     | 1,404     |
| Catch-up of 100 groups: time, memory store                  | 12.14 s   | 12.17 s   |

- **A save no longer grows with the state.** Issue #840 projected about 0.5 s of comparing over a
  catch-up of 300 groups, 6 s for 1,000 and a minute for 3,000 in a release build. That work is gone;
  it was a small part of a catch-up of 100 groups in a debug build, which took as long as before on
  this (busy) machine.
- **One more small record per save:** the generation, 8 bytes sealed, which is where the catch-up's
  201 extra records come from. They were offset in bytes by a value written again unchanged, which
  no longer counts as a change.
- **The extension** (about 24 MB) holds the state once instead of twice, beside its minimal cache. It no longer moves the record
  of a version before 1.0.9 itself, which was estimated at 5 to 6 times the state at the worst
  moment. No release build was measured here; the ratios of a save are what to take from the table.

### Administration console: requests of an open console (issue #1217)

An open console used up the address's rate limit (`ip_requests_per_minute = 1200`, shared by everybody
behind the address): every 30 s it read about ten lists and then, for each of the first 100 channels,
its members and its retention, plus the devices of every user. Measured with the real web client in
headless Chromium on the Retention page, signed in as an owner, 220 channels in one space,
the dev stack's four users, a debug server; the page's API requests counted from the
browser's network events (`tests/e2e/tests/suite/admin_console_requests.rs`, run once against the
web build of `main` and once against the change). The app's own catch-up as a new device (about
1,300 requests) is waited out first and not counted.

| What                                                   | Before    | After |
|--------------------------------------------------------|-----------|-------|
| Opening the console, API requests                      | about 215 | 14    |
| Open console, requests per minute (100 s, 3 refreshes) | 385       | 7.2   |
| Of those, per channel (members, retention)             | 360       | 0     |
| Share of the address's 1,200 a minute                  | 32 %      | 0.6 % |

What changed:

- `GET /admin/conversations` carries each channel's owner and effective retention, computed in the
  same query (docs/admin.md §3.1.2). An older server omits both; the console then reads them per
  channel for the first 100 channels, only on the Channels and Retention pages, and at most every
  ten minutes.
- The timer (30 s, while the console is visible) reads the open section's data only: Retention is
  spaces, channels and the policy (3 lists, 6 requests with the second page), the audit log is read
  on the Audit page only. The whole console loads once when it opens and after the admin's own
  changes.
- Devices load when a user's details open. Lists were read in pages of 200, up to 1,000 entries,
  and said so when they were cut; since #1241 the Channels, Retention and Users pages load a page of
  100 and the next one on request, search through the server, and the timer reads at most the first
  three pages of what is loaded (see below).
- A 429 stops the console's reads until `Retry-After` (the body's `retry_after_secs`) has passed.

The unit tests (`admin_live.rs`, `mod requests`) hold the cap against a fake server with 450 channels
and 300 users: opening reads at most 15 requests (11 lists and settings, the audit log, the guests,
the owners and admins by role), whatever the numbers of channels, users and spaces (since #1241:
no request per space, one page of each list), and no section's refresh exceeds 8, so at most 16 a
minute; none is per channel or per user. A refresh re-reads at most 3 pages of a list (100 each),
however far the admin scrolled; pages loaded beyond stay as they were read. The browser test
asserts at most 80 requests when opening and 30 a minute while open.

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

### Desktop core: CPU with a large channel and after a long absence (issues #1000, #972)

The 1.0.11 desktop app on Linux kept its core thread busy for long stretches while minimized and
idle (on average 12 % of a core over 1.6 days, 4 % in the system monitor), and after 12 hours of
standby it showed the connecting spinner for minutes. Measured on a real workspace (38
conversations, a 32 MB SQLCipher database, the open channel holding 26,829 messages) with a
symbolized release build, `perf` and `strace` on the `teamcraft-core` thread, and the time each UI
read took on the core.

Where the time went: SQLCipher decrypts and authenticates (HMAC-SHA512) every page it reads, and the
UI's reads of the open conversation each read and decoded all of it, several times per change:

| Read of the 26,829-message channel  | 1.0.11 per read  | Fixed per read  |
|-------------------------------------|------------------|-----------------|
| Feed window (count and 50 rows)     | 75–79 ms         | under 6 ms      |
| Extras of one row (asked per row)   | 4.5 ms, ≤ 78 ms  | 0.2 ms          |
| Activity feed (all conversations)   | 95–99 ms         | under 9 ms      |
| Snapshot entry of the conversation  | 86–89 ms         | under 0.2 ms    |

Every message, reaction or receipt in the channel set off this chain once more, and it grew with the
channel. The core thread spent 7.8 s of CPU on the reads after a start; now 0.3 s. Minimized for
5 minutes on a working morning, with the log channel receiving messages, the whole process now uses
0.22 % of a core (core thread 0.13 %), within the 0.5 % budget. What changed:

- The UI's reads of a conversation come from a cache of decoded messages (`TrackedStore` message
  cache, up to 64 MiB on every platform, `DEFAULT_MESSAGE_CACHE_BYTES`; bounded by bytes since
  #1125, see "Local store" below): it is filled on the first read
  and updated with every write the core makes, so a new message no longer means reading the
  conversation again. A revision per conversation lets the activity feed keep each conversation's
  entries until its messages or read state change.
- Row extras look their message up by its place in the order index; the catch-up cursor, older
  history and the read position ask the index for the oldest or newest server message instead of
  reading the conversation; parked messages and read receipts are listed by key range.
- A feed window, a thread and the activity feed borrow the cached conversation instead of copying
  it, read it once for count and rows, and gather reactions and replies in one pass instead of once
  per row. The sidebar entry counts in one pass by reference, and whether a message mentions the
  user is decided once per message and edit while the mention rules stay the same: a log channel
  with thousands of unread messages had its every text lowercased and split again on each change.

The catch-up after a long absence (#972) waited on the disk: the store used SQLite's rollback journal
with full syncs, about four syncs per write, and every received message was several writes. In 20
seconds of a catch-up the core made 1,335 `fsync` calls (12.8 ms each, 91 % of its time); 12
conversations took 58.7 s. Now a page of received messages is one group of writes, a parked message
that still can't be read is not written again on every retry, and both SQLite stores write ahead
(`journal_mode = WAL`, `synchronous = NORMAL`, e2ee.md §9.4). A single commit on the same disk
(btrfs on NVMe, 1.5 KB record): 17.6 ms with the rollback journal, 6.0 ms with WAL and full syncs,
0.04 ms with WAL and `NORMAL`.

### Desktop on Linux: hidden in the tray, and memory with a large workspace (issue #1022)

`tests/e2e` `desktop_perf.rs` measures the Linux app as it ships (tests/e2e/README.md, "The desktop
app"): a debug build of the shell with the release web client embedded, on a virtual display
(Xvfb, so WebKit paints on the CPU), signed in through Keycloak, with its own session bus, keyring
and tray. Carol signs in and quits, the `perf.rs` workspace is seeded while she is away, she starts
the app again, it catches up, she opens 40 channels, and the app is watched untouched, first with
the window shown and then closed to the tray. CPU and memory (PSS from `smaps_rollup`) come from
`/proc` for each process of the app; wakeups are the threads' voluntary context switches; requests
are the server's own log. The inspector the test drives the page with is closed while anything is
measured (an attached one instruments the page). The nightly E2E workflow runs it at the small
scale (3 spaces, 24 channels, 3,000 messages, 60 s per window) and fails when the app hidden in the
tray uses 2 % CPU or more, wakes up 100 times a second or more, or sends any request.

Measured 2026-11-10 on main with the store rework (#1690, #1691, #1692), AMD Ryzen 9 5900X, Fedora
44, WebKitGTK 2.54. Whole app, percent of one core:

| Small workspace, 60 s untouched | Before                        | After  | Budget  |
|---------------------------------|-------------------------------|--------|---------|
| Window shown: CPU               | 19–22 % (spinner, see below)  | 0.35 % | ≤ 0.5 % |
| Closed to the tray: CPU         | 0.15 %                        | 0.15 % | ≤ 0.5 % |
| Closed to the tray: wakeups     | 26 per second (old count)     | 50–55  | -       |
| Closed to the tray: requests    | 0                             | 0      | none    |

What the measurement found and fixed:

- **A spinner that never stopped.** A channel a little taller than the window, opened at its
  bottom, showed "Loading older messages…" for good: the list asks for older history only once
  the reader leaves the bottom (or the list doesn't fill the window), but showed every
  conversation with more history on the server as loading. Its animation kept WebKit painting at
  about 20 % of a core with the window shown and untouched (on the CPU here; on a GPU it costs
  less CPU but the same frames). The top of the list now says "loading" only while a page is asked
  for (`shown_older_status`, `message_list.rs`).
- **A key grabber polling all day.** The global-shortcut plugin's X11 grabber (`global-hotkey`)
  polls the X server every 50 ms from the moment the plugin exists, grabbed key or not: 20 wakeups
  a second, minimized or not, also on Wayland (through XWayland), where the GlobalShortcuts portal
  binds the keys and the plugin is never used. The app now adds the plugin the first time
  push-to-talk or the mute shortcut is registered on X11 (`global_keys.rs`).

Hidden in the tray the app was already within the budget once #1003 had fixed the core: the page
stops painting and its timers when the window is hidden, and the core reads nothing on a timer. The
wakeups left are mostly WebKit's own: the IPC threads between the web process and the shell, the
web process's main loop, and its memory pressure monitor, which reads `/proc` files (about 170
reads a second that don't wake it).

Not fixed here: after a catch-up, the core reads the workspace roster and policy log once per
channel whose role head moved (`sync_channel_roles`), so the first upkeep pass after coming back
sends three requests per channel (74 on the small workspace, one burst): #1706.

**Memory.** The budget is 400 MiB for the whole app with 300 channels and 100k messages. PSS by
process, the large workspace (20 spaces, 300 channels, 100k messages, 200 pictures) after 40
channel switches, window closed to the tray:

| Process                                | PSS      | Of it anonymous | Notes                                   |
|----------------------------------------|----------|-----------------|-----------------------------------------|
| `teamcraft-desktop` (shell and core)   | 244 MiB  | 152 MiB         | the debug binary maps about 90 MiB      |
| `WebKitWebProcess` (the page)          | 352 MiB  | 294 MiB         | painting on the CPU (Xvfb)              |
| `WebKitNetworkProcess`                 | 21 MiB   | 13 MiB          |                                         |
| Whole app                              | 617 MiB  | 459 MiB         | budget 400 MiB                          |

For comparison, the installed 1.0 release on a real workspace (38 conversations, 13 hours in use,
Wayland, GPU): 411 MiB, of it the shell and core 142 MiB (97 anonymous, the release binary's
mapped files 44 MiB), the page 263 MiB (201 anonymous) and the network process 6 MiB.

Where it goes. The same measurement on the small workspace (24 channels, 3,000 messages) puts the
page at 312 MiB (255 anonymous) and the shell and core at 210 MiB (100 anonymous): the large
workspace adds about 40 MiB to the page and 50 MiB to the core, and the rest is there whatever the
workspace. Inside the page, with the large workspace held in the inspector after the channel
switches (`PERF_DESKTOP_HOLD_SECS`):

| Page (`WebKitWebProcess`), large workspace | Value    |
|--------------------------------------------|----------|
| The UI's wasm heap (`memory.buffer`)       | 19 MiB   |
| JIT code (anonymous executable mappings)   | 11 MiB   |
| DOM nodes                                  | 671      |
| Pictures shown                             | 6, as 960×540 previews |
| Freed by a full collection (`Heap.gc`)     | 17 MiB   |
| Anonymous memory in all                    | 305 MiB  |

So the UI's own data is small and bounded (the message list is virtualised, pictures arrive as
previews), and about 250 MiB of the page is WebKit's own at any workspace size: the JavaScript
heap, its allocator's arenas and the backing stores of what it paints, which with CPU painting
under Xvfb sit in RAM where a GPU keeps some in VRAM. In the shell's process GTK and WebKit's UI
process sit next to the core; the core's caches are bounded (the message cache at 64 MiB). Nothing
of ours explains the gap to the budget, and WebKit's limits can only be set where Tauri creates its
web context, without a hook: #1708 (WebKit's memory pressure settings, a heap profile of the core,
a release build on a GPU). What came of it: "Desktop on Linux: memory pressure and the allocator".

### Desktop on Linux: memory pressure and the allocator (issue #1708)

Same harness and machine as above (`desktop_perf.rs`, large workspace, debug shell with the release
web client, Xvfb, CPU painting), measured 2026-11-11 on WebKitGTK 2.54.2. Each change was run on
its own against a baseline of the same build with WebKit's own settings; one run takes about 30
minutes, 14 of them seeding. The settings of a run are environment variables, so one binary serves
all of them.

**WebKit's memory pressure settings.** WebKit gives memory back in its web process (dead resources,
decoded pictures, font and layout caches; at the strict threshold also JIT code and a full garbage
collection) only once the process passes a fraction of `WebKitMemoryPressureSettings`' limit. The
limit defaults to the machine's RAM, at most 3 GB, and the first threshold to a third of it: about
1 GB, which the page never reaches, so WebKit never released anything. The settings are a
construct-only property of the web context, no environment variable sets them, and Tauri creates
the context through wry without a hook. wry is vendored with a setter its web contexts pick up
(`crates/vendor/wry/VENDORED.md`, with an upstream PR draft); the shell calls it before the first
window (`webview_memory.rs`). The kill threshold stays 0: a killed web process would reload the
page (`webview_recovery.rs`), and no run logged a terminated web process.

| Web process, large workspace, after 40 switches | Shown, PSS (anon) | Hidden, PSS (anon) | Web process CPU, shown / hidden |
|-------------------------------------------------|-------------------|--------------------|---------------------------------|
| WebKit's settings (3 GB, 0.33 / 0.5, 30 s)      | 386 MiB (319)     | 357 MiB (292)      | 0.04 / 0.05 %                   |
| 512 MiB, 0.3 / 0.5, 5 s                         | 307 MiB (239)     | 305 MiB (239)      | 0.14 / 0.14 %                   |
| 384 MiB, 0.3 / 0.5, 10 s                        | 301 MiB (233)     | 299 MiB (234)      | 0.10 / 0.11 %                   |
| 512 MiB, 0.5 / 0.7, 10 s (the default)          | 307 MiB (240)     | 305 MiB (240)      | 0.10 / 0.11 %                   |

Below about 240 MiB anonymous nothing more comes free: a strict threshold the page sits above
(384 MiB × 0.5) saved 6 MiB more than a conservative one, and costs a full collection every poll.
The default limit keeps the page near that floor with the least work: WebKit checks every 10 s and
releases while the page is above 256 MiB by its measure. Channel switches stayed at 101–106 ms p50
and 156–173 ms p95 in all runs (the same within the spread between runs). Each value is a
`TEAMCRAFT_WEBVIEW_MEMORY_*` variable (`LIMIT_MIB`, 0 for WebKit's own settings; `CONSERVATIVE`,
`STRICT`; `POLL_SECS`).

**The shell's allocator.** A heap profile of the shell during the same run (heaptrack on the debug
binary, from the start after seeding to the quit, about 13 minutes) found little of the core alive:

| Shell and core, heaptrack               | Value                                      |
|-----------------------------------------|--------------------------------------------|
| Peak of the heap (during the catch-up)  | 73 MiB                                     |
| Alive at the end                        | 34 MiB                                     |
| Of it the message cache                 | 13 MiB                                     |
| Of it the openmls provider (300 groups) | 3.8 MiB                                    |
| Of it SQLite and SQLCipher              | 1.6 MiB                                    |
| Allocations in all                      | 60 million, 37 million of them short-lived |

Yet the shell's process held 145 MiB anonymous. glibc's allocator keeps freed memory in its arenas
(up to eight per core, one per thread that allocates) and, after a large block is freed, raises its
trim threshold up to 64 MiB, so a peak during a catch-up stays: 51 MiB in 122 arena mappings next to
31 MiB of main heap in the harness, and 314 MiB in arenas after four hours of the installed 1.0.30
on a real workspace. The start already re-executes itself for GTK's variables (`startup_env.rs`); it
now also sets `MALLOC_ARENA_MAX=2` and a fixed `MALLOC_TRIM_THRESHOLD_=131072` (set, glibc keeps
the threshold and gives blocks from 128 KiB up their own mapping), unless the user set them or
`GLIBC_TUNABLES`. The arenas went from 51 to under 1 MiB, the shell's anonymous memory from 145 to
89 MiB, with no change in channel switches or idle CPU.

| Whole app, large workspace (PSS) | Before        | After         | Budget  |
|----------------------------------|---------------|---------------|---------|
| Window shown                     | 657 MiB       | 515 MiB       | 400 MiB |
| Closed to the tray               | 626 MiB       | 510 MiB       | 400 MiB |
| Of it the page                   | 357 MiB       | 296 MiB       | -       |
| Of it the shell and core         | 249 MiB       | 194 MiB       | -       |
| Of it the network process        | 20 MiB        | 19 MiB        | -       |
| Idle CPU, shown / hidden         | 0.13 / 0.24 % | 0.19 / 0.30 % | 0.5 %   |

Still above the budget in this measurement. What is left: the debug binary's own mapped files
(about 90 MiB of the shell's 194; the release binary maps about 44 MiB), and the page's floor of
about 240 MiB anonymous that WebKit doesn't release, which with CPU painting under Xvfb holds
backing stores a GPU keeps in VRAM. The release build on a GPU with the same workspace is the
number the budget is about; it is the open part of #1708.

### Coming back and bad networks, end to end (issue #972)

`tests/e2e` runs the app as the apps do (the sync runner with its connection supervisor, the
desktop's store on a disk, or the mobile store with `E2E_STORE=mobile`) against the real server,
Keycloak and MLS, and makes its link to the server bad in the kernel (`tc netem`, `nft`) inside a
rootless network namespace: `scripts/e2e-network.sh`. Each scenario checks that every message
arrives exactly once.

**Coming back** (`long_absence.rs`): 38 conversations, 12 of them changed while the device was away
(40 messages each, somebody added and removed three times), the session expired, 30 ms round trip.

| Coming back                      | Before                                         | Fixed   |
|----------------------------------|------------------------------------------------|---------|
| Messages readable afterwards     | `main`: 313 stayed parked, the device fell out | all 481 |
| Per changed conversation         | 1.2 s (with the identity fix alone)            | 0.24 s  |
| Connected after                  | 16.4 s (with the identity fix alone)           | 4.8 s   |
| Message sent to the open channel | `main`: never                                  | 0.6 s   |

The device verified each late commit against identities pinned from today's member list; somebody
added and gone again since was not on it, the commit was refused and the device fell out of the
group (e2ee.md §4.6). It now pins whom the commit's signed change names first. A pass over a
conversation's queue reads the member list and each device list once, instead of again for every
entry, one round trip after the other. With the issue's 80 ms to the server the saving is larger.

The mobile apps (Android, iOS) never told the core which conversation is open, so after a long
absence the one opened from a notification waited behind every other conversation, shown as an empty
conversation with "?" for its name until then. Their reads of a conversation's messages now set the
catch-up focus, as the desktop's do. Mobile store, the network back after 15 s, a direct message new
to the device opened from its notification: it shows 4.7 s after the network is back without the
focus (once the whole backlog is in), 0.6 s with it.

**Bad links** (`network_conditions.rs`, desktop store, writers sending every 0.7 s):

| Link                                       | Noticed by the app        | After it was good again           |
|--------------------------------------------|---------------------------|-----------------------------------|
| No UDP at all                              | nothing to notice         | live throughout (≤ 21 ms)         |
| Loss 5–60 %, changing every 4 s, 80±60 ms  | stays connected           | all there after 28 ms             |
| Drop-outs: 2 s dead, 3 s fine, eight times | stays connected           | all there after 34 ms             |
| Dead 30 s, then 96 kbit/s with 300 ms      | stays connected           | all there after 224 ms            |
| Resets for 10 s                            | at once                   | connected after 2.8 s (backoff)   |
| Dead 8 s                                   | not (shorter than a beat) | 4.8 s (TCP retransmission)        |
| Dead 120 s, past the resume window         | after 53 s, now 45 s      | connected after 0.6–0.8 s, all in |

A silently dead link (no signal from the OS) was found only after two heartbeat intervals without a
frame plus the probe slack, 53 s in which the app showed "connected". Every heartbeat now has to be
answered within 10 s (`heartbeat_answer_timeout`): at most one interval plus that, 36 s in the unit
test on a paused clock; the run above found it after 45 s, which is not yet explained.

**Live events during the catch-up** (issue #1021). The gateway connects before the catch-up of a
fresh session (`identify` is answered first, and the banner goes then), but what arrived on it
waited for the end of the catch-up: a message to the open chat, caught up within a second, showed
only once every other conversation was through. The catch-up now applies the oldest held events
between two of its steps, in their order, while each concerns a conversation it has finished
(messages, MLS messages other than welcomes, receipts) or no conversation (typing, presence); the
first that has to wait (a welcome, a membership change, an event of a conversation still to come)
holds back everything after it, so the order stays the server's. `coming_back.rs`, 24 channels with
400 ms member lists: Ann's message to the open chat during the catch-up shows at the next step,
within one batch of member lists, before the catch-up ends (before: at its end). An epoch change in
a finished channel during the catch-up keeps its order, every message readable once.

**More ahead of the catch-up, and a busy channel's newest page first** (issue #1423). Two kinds of
events held back everything behind them although nothing they touch is still being caught up:

- Events of a conversation the catch-up doesn't visit, because nothing moved in it since the last
  sync (heads, #714): it is current, and now counts as finished from the start. `coming_back.rs`:
  Ann's message to such a channel during the catch-up shows at the next step (before: at the end).
- Membership and conversation changes (`MembershipAdd`, `MembershipRemove`, `ConversationUpdate`)
  of a finished conversation: they read that conversation's record and member list, and the only
  commit among them goes into its own group, which is caught up. In the epoch-change test, Ann's
  message after adding Bob now shows before the catch-up ends.

A channel with hundreds of new messages was read page by page, in order, before the catch-up went on
to the next conversation and before its own newest messages showed. With
`catch_up_message_pages` (every app: 4 pages, 200 messages) a catch-up reads that many, then
the conversation's newest page, and moves on; the conversation counts as caught up for live events
and the outbox, and the feed shows a hole between the two (#1156's gap row). The MLS queue is read
whole as before. Once every conversation got that far, the same catch-up reads each hole full, in
order, and takes pins, bookmarks and channel role entries from the newest page on again, so a pin in
the hole doesn't undo a newer unpin; one cut short leaves the hole noted, and the next catch-up
reads it (the conversation counts as due without a head that moved). Nothing a message carries is
skipped, the store ends as before. `catch_up.rs`: 30 new messages in pages of 5, two in order: cut
short after one page of the hole, the newest five are there; the next catch-up reads the rest, every
message once, in order.

The mobile apps turned it on once their feeds showed holes (issue #1438): the FFI's pages
(`messages_from`, `message_tail`) carry the holes among their messages (`gaps`, each with its place
in the page), iOS and Android draw a row there ("Loading messages…", or the failure with "Try
again"), and a reader within 40 rows of it, not at the bottom of a list that scrolls, asks for a
page of it from the side they come from, as web and desktop do. Paging back, a jump to a date and
its holes, and the first sync's older history read pages out of order too: a pin or bookmarks
message in such a page now takes the controls from it on again in their order (`replay_controls_from`),
so an older one read later no longer undoes a newer one (`older_history.rs` tests).

**The backlog after the sync** (issue #1448). The holes were read before the catch-up ended, so
what comes after it in the full sync (membership changes waiting, the outbox, key packages, account
data) and the session the connection keeps waited for them too; a phone sent to the background
during that read started over with a fresh session. Every app's loop now reads the backlogs itself
after the sync (`Client::read_backlogs_in_background`): a page per turn when nothing else is waiting,
as the first sync's older history, the focused conversation first, a failed page again after 30 s
or with the next connection, and what an earlier run left on the next start, also on a resumed
session. A background wake-up of the phone (`RunnerCommand::CatchUp`) still waits for the backlog,
but its session is kept before that read starts. Each hole's end still takes the order-sensitive
controls again, and the conversation keeps its "catching up" mark until then. `catch_up.rs`: the
sync ends with the newest page and the hole marked; the loop reads the rest in order, an older pin
in it doesn't undo the newer unpin, and a backlog a cut-short catch-up left is read on without
another catch-up. Without the loop (tests, tools) the catch-up reads them as before.

Not measured end to end yet: the e2e scenario (`long_absence.rs`) with a live message during the
catch-up and the issue's 12 h gap with 40 conversations needs the Linux network namespace
(`scripts/e2e-network.sh`) and was rerun neither for #1021 nor for #1423 nor for #1448.

### Local store: opening and reading a large account (issue #155)

The sealed store of the mobile apps and the browser decrypted every record when it opened, so open
time and memory grew with the history, and the iOS notification extension (about 24 MB for
everything) opened the same store. Records are now read when they are asked for, through a sealed
catalog (docs/e2ee.md §9.1): a table's keys and each conversation's messages in display order,
kept as paged sealed records. Opening reads one record; a message is the conversation's index root,
one index page and the record.

Budgets for a large account, 300 conversations with 100,000 messages (one channel holds half of
them); `tests/store_memory.rs` checks them with generous bounds in every test run (20,000 messages,
debug build):

| Budget                                                      | Budget   | Test bound (debug) |
|-------------------------------------------------------------|----------|--------------------|
| Opening the store                                           | ≤ 50 ms  | 1 s                |
| Notification extension: opening and the message, Rust heap  | ≤ 4 MiB  | 8 MiB              |
| The newest and the oldest message of a conversation         | ≤ 20 ms  | 200 ms             |
| The first 50 rows of the feed, on empty caches              | ≤ 20 ms  | 300 ms             |
| The same, Rust heap at its peak                             | ≤ 1 MiB  | 4 MiB              |

Measured on the sealed SQLite store (the mobile apps' store), release build, local machine
(Linux, Ryzen, NVMe), November 2026; "before" is the same test on `main` 7dba4e60:

```sh
TEAMCRAFT_STORE_BUDGET_MESSAGES=100000 cargo test --release -p teamcraft-core --all-features \
  --test store_memory -- --nocapture
```

| 300 conversations, 100,000 messages                        | Before                        | After                |
|------------------------------------------------------------|-------------------------------|----------------------|
| Notification extension: open and show the message          | 461 ms, 148 MiB at its peak   | 0.7 ms, 0.06 MiB     |
| App: open the store                                        | 414 ms                        | 0.3 ms               |
| Newest and oldest message of the 50,000-message channel    | 96 ms                         | 0.6 ms               |
| All 50,000 messages of the channel, first read             | 30 ms (all were in memory)    | 350–475 ms, 50 MiB   |
| Newest 50 feed rows of that channel, first read            | 350–475 ms, 50 MiB (whole)    | 1.1 ms, 0.11 MiB     |

- **The extension stays far under its limit**, independent of the history: its Rust heap is the
  index root and one page of the notified conversation plus a few entries (names, the push key, the
  policy). SQLite's own page cache comes on top (its default, 2 MiB at most; the counting allocator
  doesn't see it). It opens the store with `SealedStoreLimits::minimal()`, keeps no message cache and
  never builds the catalog: on the first push after an update, before the app ran the migration, it
  fetches the message instead.
- **The app's memory for the store is bounded, in bytes:** decrypted entries (8 MiB), decoded
  messages (4 MiB) and index nodes (2 MiB for each of the eight root and page caches) by default,
  plus the message cache of the conversations read last (64 MiB on every platform). Before, the
  whole store was in memory: 148 MiB for this account, growing with every message.
- **Limits count bytes, not items (#1125):** they used to count messages (4,096 in the store, 60,000
  in the message cache) and nodes (256), which assumed about 1 KB a message. A channel of 10 KB
  messages held ten times that, so the mobile budget (250 MiB) was only kept by messages of
  ordinary length. Each message is weighed once when it enters or leaves a cache
  (`StoredMessage::footprint_bytes`: the structs plus the length of every string, list and byte
  buffer, 96 bytes of bookkeeping on top; an estimate, not the allocator's), an index page when it
  is read or written (at most 256 entries), a root as pages times the size of a page reference.
  The caches keep a running sum, so a write or a read costs O(the message) and never looks at
  the rest of the conversation. A message cache write that grows the cache past its budget drops
  the least recently read other conversations, never the one being written to while it fits
  alone; one that outgrew the whole budget is dropped and read from the store next time (its
  revision has moved, so no stale read is kept). Readers hold their conversation by `Arc`, so
  eviction never takes it from a read in progress. A conversation larger than the whole budget is
  not cached at all. The cap on the sealed store (`SealedStoreLimits::max_message_bytes`,
  `max_index_node_bytes`) and on the message cache (`TrackedStore::with_message_cache`) are
  arguments, so a platform can set its own; the defaults are the figures above (`minimal()`: 128
  KiB each, no message cache). With the 64 MiB default, the 50,000-message channel measured above
  (about 50 MiB read whole) still fits; 50,000 messages of 10 KB would not, and are read from
  the store on each use instead of taking 500 MiB.
- **A full read of a big conversation got slower:** records are read one by one under pseudorandom
  IDs, about 9 µs a message (the file is memory-mapped, so a read is no system call).
- **The feed no longer needs a full read:** the sealed store's catalog also keeps, per conversation,
  a feed index (the rows; the root's page lengths count positions) and a relations index (reactions
  by target, replies by thread, events), and the SQLCipher store has columns with partial indexes for
  the same. `feed_tail` and `message_page` read the rows they return, their reactions and their reply
  counts: the first page of the 50,000-message channel is at most 80 records
  (`the_first_page_of_a_large_channel_reads_that_page` counts them). A channel with an import account
  also reads its events once per change, the only messages the import's additions can be. The memory
  store keeps the whole-read view, which costs it no decryption.
- **Writes pay for it:** filling this account took 12.0 s instead of 9.7 s (+24 %, one run each; an
  earlier pair differed by 9 %). A message outside a group of writes seals the feed index's root and
  page as well.
- **What still reads a conversation whole:** a sidebar entry's counts (`entry_counts`), the last
  activity, the activity feed, a thread, toggling a reaction and the other actions that look a message
  up. The message cache serves those after their first read, so opening a big channel is only as fast
  as these numbers once none of them runs for it. Not measured end to end in an app.
- **The browser** keeps the records' ciphertext in memory (IndexedDB has no synchronous reads) and
  decrypts a record only when it is read: opening no longer decrypts the store (part of #778), but
  it still reads all of the ciphertext, so its open time and memory stay O(store) (issue #1127, under
  "Web client" above, has the numbers and why). The first catalog build gives the page its thread back
  between batches and shows its progress. The catalog costs download size: about 28 kB gzip in the
  app and 25 kB in the service worker, which doesn't carry the build (27 kB less). It is needed
  from the first page on, so it can't move to a chunk; the guest budget went from 5.6 to 5.7 MB.
- **Migration:** a store written before builds its catalog once, when the app opens it: every
  record is decrypted once (what every start did before), in batches of 4,096 records (512 before
  #1509), each a group of
  writes that saves its progress, so a build that is killed goes on after its last batch. Tested on
  the released 1.0.9 fixture and on a 4,500-message store stopped after two batches
  (`store::sealed::tests::catalog`). A damaged or inconsistent catalog is built again the same way,
  once, when it is noticed (`store::sealed::tests::repair`); that read takes as long as the build.
  A catalog from before the feed indexes is built again under a new generation too, and SQLCipher
  fills its new columns from every record in one layout step when it opens.

### Search on a large workspace (issue #154)

Every search used to read every stored message, build a tantivy index of them in memory (with a
15 MB writer) and throw it away: time and memory grew with the history, on every query. The local
index (docs/e2ee.md §10.1, "Local index", `teamcraft_core::search_index`) is now kept in the store,
sealed like it, and brought up to date from a queue written with each message; the upkeep indexes a
bounded batch per tick. A search reads the saved index (once, then only the files a save added) and
indexes in memory only what still waits. Hits are rendered from the store's indexes of reactions and
replies instead of reading each hit's conversation whole.

Measured with `tests/search_scale.rs`: 300 conversations, 100,000 messages of about a hundred bytes
(half in one channel), SQLCipher with the desktop's message cache, release build, Apple M-series
laptop, November 2026, store on the internal SSD (`TEAMCRAFT_SEARCH_DIR`); "before" is the same
test on `main` 5119fc2e. Medians of seven searches; the machine was busy with other builds, so the
"before" column has outliers of several seconds.

```sh
TEAMCRAFT_SEARCH_DIR=$TMPDIR TEAMCRAFT_SEARCH_MESSAGES=100000 cargo test --release -p teamcraft-core \
  --all-features --test search_scale -- --nocapture
```

| 100,000 messages                                      | Before               | After                        |
|-------------------------------------------------------|----------------------|------------------------------|
| Core search, a rare word (100–200 hits)               | 0.96 s, 175 MiB peak | 0.7 ms, 1 MiB peak           |
| Core search, the same with a typo                     | 0.84 s, 141 MiB peak | 0.6 ms                       |
| Core search, a word in half the messages (2,000 hits) | 0.89 s, 141 MiB peak | 7.6 ms                       |
| UI search, a rare word (hits rendered)                | 0.95 s, 141 MiB peak | 3.5 ms, 6.7 MiB peak         |
| UI search, a word in half the messages                | 2.7 s, 118 MiB peak  | 17 ms, 2.1 MiB peak          |
| UI search, `in:` one channel                          | 0.91 s, 98 MiB peak  | 2.9 ms                       |
| Held between searches (the index read from the store) | nothing              | 5.3 MiB                      |
| The index in the store                                | –                    | 5.3 MiB (about 55 B/message) |
| Building it once (a store from before), 12 steps      | –                    | 1.9 s; slowest step 0.8 s    |
| 100 new messages, one step                            | –                    | 3 ms                         |

- **Bounded per step:** a step takes 5,000 queued messages (`DEFAULT_STEP_MESSAGES`) or one
  conversation queued whole (a store from before the index, a conversation whose history went), read
  once through the message cache. Its memory is tantivy's 15 MB writer plus that conversation; the
  50,000-message channel took the build's peak to 148 MiB (the cache holds the conversation already
  in the app). Merges stop at segments of 200,000 messages, so no step rewrites the whole index.
- **Writes:** every write of a text or file message (or one deleted or removed) also writes one
  small queue record in the same group of writes (one more row per message), and a step deletes the
  records it handled.
- **On the SD card** of the same laptop (the default target directory) the searches are the same,
  but steps that save a merged segment wait for SQLite's log (one took 13 s): the writes are a few
  MB, the card's sync is slow. Phones' flash is far faster; not measured on a phone yet.
- **The browser** keeps its substring scan (tantivy doesn't target wasm32) and writes no queue.
- **Still open** (#154): full-history search on a new device (segments are fetched on mobile, but
  no app publishes them), search off the UI thread on the desktop, a measured 500,000-message run.

### Android (first numbers)

Release Rust core, debug Kotlin build (`assembleLibreDebug`), API 35 emulator (the Pixel 10 / API 37
image does not boot on this AMD host), signed out:

| What                                          | Value     |
|-----------------------------------------------|-----------|
| Cold start to the first frame (`am start -W`) | 1.0–1.2 s |
| Memory after start (PSS)                      | 95 MiB    |

### Android on a slow phone (November 2026)

A OnePlus 5T (Snapdragon 835, Android 13, 6 GB), the libre release build (R8 on, as shipped, compiled
with its baseline profile: `cmd package compile -m speed-profile`), against the dev stack through
`adb reverse`. The workspace comes from `tests/e2e/tests/suite/mobile_workspace.rs`: 12 spaces of 25
channels, 40,000 messages (10,000 in one channel), two direct messages, reactions; the phone was a
member while it was written, so its store holds all of it. "List complete" is the app's
`reportFullyDrawn` (rows and their last lines), median of five cold starts.

| What                                      | Before        | After          | Budget (mobile) |
|-------------------------------------------|---------------|----------------|-----------------|
| Cold start, first frame (`am start -W`)   | 224-237 ms    | 281-295 ms     |                 |
| Cold start, list complete                 | 2.22 s        | 1.22 s         | 2 s             |
| Chat list refresh after a change          | 1.6-2.5 s     | 0.25-0.8 s     |                 |
| Chat switch, tap until the messages show  | 1.31-1.48 s   | 0.20-0.29 s    | 150 ms          |
| Scrolling the long channel (25 flings)    | 0.26 % janky  | not changed    |                 |

The first frame moved by about 60 ms between the two runs, with a store grown by 10,000 messages in
between; it was not measured again on the same store.

What cost the time:

- **The chat list's last lines** were two reads and a JSON decode per conversation (600 reads for 300
  conversations) on every change, and each read moved the catch-up focus to that conversation, so
  the list kept taking the focus from the open chat (Android and iOS). Now one read for the whole
  list (`chat_previews`), which leaves the focus alone and skips system lines when a written message
  is close by.
- **The list of conversations** decrypted every conversation and user record once per space (13
  times with 12 spaces). Now one pass (`listed_conversations`).
- **A conversation** waited for the whole-workspace snapshot (200-380 ms to decode on this phone)
  before its messages, and every change cancelled the pass and started it again. Now the messages
  come first, the snapshot is decoded once per change and shared, and the screen starts with the one
  read last.
- **The read thread** is one queue: the conversation's reads waited 450-600 ms behind the list's.

**No loading screens (second pass).** On a smaller workspace (33 conversations, 4,000 messages,
same phone), starting showed the animated loading mark at about 280 ms and the list at 690-790 ms;
opening a chat showed an empty screen for 120-170 ms, then its messages. Now:

| What                                          | Before                            | After                                    |
|-----------------------------------------------|-----------------------------------|------------------------------------------|
| Cold start until the list shows               | 690-790 ms, loading mark first    | 600-650 ms, the list is the first frame  |
| Start cache read (sealed, on the main thread) | –                                 | 25-40 ms                                 |
| Open a chat at the top of the list            | empty screen, messages 150-230 ms | messages in the first frame (180-250 ms) |

- The app keeps the chat list it showed last, sealed like the database key (e2ee.md §9.2), and
  shows it in its first frame; the real list replaces it when it is read.
- The newest messages of the chats at the top of the list (direct messages first) are read ahead,
  through a read that leaves the catch-up focus alone, and every chat keeps what it showed.
- What is left is the first composition of a screen on this phone (about 300 ms for the whole app
  at start, 150-200 ms for a conversation), not waiting for data.

Coming back after a long absence: the core reported `Connecting` until the whole catch-up was done
(13 s for 8,000 new messages here, longer on a mobile network), so the "connecting" banner could
show while new messages were already arriving. It reports connected once the server answers
`identify` or `resume` now (test `a_long_catch_up_shows_connected_once_the_server_answered`).

### Coming back to the phone with the desktop open (November 2026)

The everyday case of a user with two devices: the desktop is open, so the server holds the phone's
notifications back and drops them once the desktop read the messages (notifications.md,
"Attention"); the phone gets nothing. After 45 minutes in the pocket the app comes out of standby
past the resume window, the user taps the direct chat at the top. `tests/suite/coming_back.rs` in
`teamcraft-core` plays it with real MLS clients on the fake server: Ann, the user's desktop and the
user's phone, which runs the `SyncRunner` as the apps do and whose UI hears only `on_change`. Each
call is checked against what the store holds at that moment. 24 channels with 4 new messages each,
30 new direct messages plus the desktop's answer, 70,000 imported messages in the direct chat, and
every member list 400 ms (a busy server on a mobile network). Tokio's paused clock, so the times are
the network's and the core's waiting, not CPU.

| What the phone's UI was told                     | Before | After  |
|--------------------------------------------------|--------|--------|
| Connected                                        | 10.4 s | 0 s    |
| The open chat's news                             | 10.4 s | 0.4 s  |
| A chat tapped 0.8 s in, during the catch-up      | 10.4 s | 1.6 s  |
| Every conversation caught up                     | 10.4 s | 2.0 s  |
| Times the chat list changed during the catch-up  | 1      | 5      |

- **Nobody told the app.** The mobile sync loop called `on_change` after the whole catch-up only;
  the conversation caught up first and the "connected" state sat in the store unread. Now the
  client counts what a sync stored for the UI (`sync_progress.rs`: the server's answer, each
  conversation caught up) and the loop tells the app at most every 250 ms. The browser read on
  its own one-second timer and wasn't affected.
- **One member list after the other.** Each pass over a conversation's MLS queue reads its member
  list to verify the entries, a round trip per conversation, one at a time. The catch-up reads them
  for a whole batch at once with the first pages, and the open chat its list and pages in one round
  trip.
- **A chat tapped during the catch-up** waited for its turn in the batch; it is taken next now, after
  the round trip on its way.
- **A resume that can only be refused.** Back after more than the resume window, the phone first
  opened a connection to resume, was told `session_invalid` and opened a second one (TLS, socket,
  `hello`: several round trips on a mobile network). `ready` names the window now, the device keeps
  when it last heard from the server, and identifies at once when that is longer ago: one connection.
- **A reply typed right after opening** went out with the end of the catch-up (2 s here); it goes out
  once its conversation is caught up now, which the open one is first.
- Android shows a thin moving line under the top bar while the connection is set up or a
  conversation on screen is still caught up (`SyncingLine`), after 300 ms, so quick ones don't flash.

What the phone doesn't have when the user taps still costs its round trips. So the server now wakes
idle phones silently for what it doesn't push them visibly (notifications.md, "Sync wake-ups"), and
they catch up in the background. The same day with that wake-up in the pocket
(`after_a_sync_wake_up_everything_is_there_before_any_connection`): the open chat, the list and the
read state are complete in the app's first frame, before it connects.

### Mobile core: start and channel switch on a large workspace

`crates/teamcraft-ffi/tests/start_budget.rs` measures through the API the apps call, on a sealed
store with 300 conversations, 100,000 messages (half in one) and 3,000 MLS values of 4 KiB; no server
is reachable. Release build on a desktop machine, not a phone: a phone is several times slower, so
read these as a comparison, and the logs (`slow read`, `slow engine job`) for the device.

| Measured                                      | Engine thread only | Read thread | Budget (mobile) |
|-----------------------------------------------|--------------------|-------------|-----------------|
| Constructor returns                           | 77 ms              | 1 ms        |                 |
| List usable (first snapshot)                  | 2165 ms            | 1474 ms     | 2000 ms         |
| List usable, a token refresh takes 3 s        | 5262 ms            | 1476 ms     | 2000 ms         |
| Snapshot again, nothing changed               | 1358 ms            | 0.2 ms      |                 |
| Same, a token refresh takes 3 s               | 4385 ms            | 0.3 ms      |                 |
| Channel switch p95                            | 0.7 ms             | 0.7 ms      | 150 ms          |
| Channel switch p95, a token refresh takes 3 s | 3001 ms            | 0.5 ms      | 150 ms          |

The first snapshot used to read and decrypt every message of every conversation for its counts
(`entry_counts`). Now each conversation keeps a sealed tally (`crates/teamcraft-core/src/entry_tally.rs`):

| List usable (read thread, 100,000 messages)                  | Result  | Budget  |
|--------------------------------------------------------------|---------|---------|
| Before, and the first start after updating (counts them all) | 875 ms  | 2000 ms |
| Next starts                                                  | 171 ms  | 2000 ms |
| Next start, a token refresh takes 3 s                        | 140 ms  | 2000 ms |

Same run, under a machine load of about 10; of those next starts about 80 ms is the start of the sync
and about 90 ms the snapshot. This workspace is the worst case for the first start: nothing was ever
read, so every message is unread. What a read costs since a tally was written:

| Since then                                    | Reads                    |
|-----------------------------------------------|--------------------------|
| Nothing                                       | No message               |
| New messages at the end                       | Those messages           |
| Read position, mark, mention rules, an edit   | The rows from the read position on |
| A row left, moved or got readable; older rows | The whole conversation, as before |

A tally is checked against a version the store writes in the group of the messages, so it is never
updated by hand and can't drift; one written before a crash means counting the difference.

### Read connections and a lane for the open chat (issues #1476, #1583)

Each read thread reads through a SQLite connection of its own; the open chat's reads (`query`,
`message_tail`, `message_window`, `message_tail_ahead`) have a thread of their own, so they don't
wait behind the chat list, a snapshot or the share sheet's targets, and nothing waits for the engine's
groups of writes. The writer stages what each commit changed, so a reader drops only that from its
caches (docs/e2ee.md §9.5). `crates/teamcraft-ffi/tests/list_reads_budget.rs`, release, 80
conversations with about 100,000 messages (`TEAMCRAFT_STORM_BIG_CHATS=1`,
`TEAMCRAFT_STORM_BIG_MESSAGES=70000`), before (main) and after:

| Measured                                                   | OnePlus 5T before | after   | Desktop before | after  |
|------------------------------------------------------------|-------------------|---------|----------------|--------|
| Opening a chat during a change storm, p50                  | 10.2 ms           | 4.5 ms  | 2.6 ms         | 1.6 ms |
| Opening a chat during a change storm, p95                  | 25.1 ms           | 7.1 ms  | 3.8 ms         | 2.3 ms |
| One chat list pass, idle, p50                              | 29.4 ms           | 14.1 ms | 4.8 ms         | 3.5 ms |
| List pass after another connection's commit, p50 (cold)    | 18.2 ms           | –       | 9.1 ms         | 7.2 ms |
| List pass on the read connection after the writer's commit | –                 | 5.4 ms  | –              | 1.2 ms |
| Same, warm caches                                          | 5.2 ms            | 5.2 ms  | 1.3 ms         | 1.2 ms |

The storm's sync writes through a connection of its own (another process to the store), so its
commits still drop every cache on both sides; the engine's own commits now cost the reader what they
changed (the last two rows), where a plain second connection would pay the cold pass every time.

### Actions and cache writes off the read threads (issue #1756)

A read thread used to run an action's local half (`ReadQueue::act`), which waits for the writer's
lock: while the engine held a group of writes (a catch-up), every read queued behind the action
waited with it, up to 21 s for 3 ms of work in the crash reports. Actions now have a thread of their
own (`teamcraft-actions`, span `queue.act`), which reads and writes through the writer inside its
group. The chat list pass keeps the sidebar's counts (`flush_entry_tallies`) only when the lock is
free (`LocalStore::try_begin_writes`); otherwise they wait for the next pass, since they are a cache.

### Files view on a large workspace (issue #1509)

`crates/teamcraft-ffi/tests/files_budget.rs` measures the Files view through the API the apps call:
a sealed store with 300 conversations and 100,000 messages (half in one), every 20th message with a
file (5,191 files). Release build on a desktop machine.

| Measured                                               | Result     |
|--------------------------------------------------------|------------|
| Before: every conversation's files, every 15 s         | 1564 ms    |
| After: first page of all files, first read             | 443 ms     |
| After: three pages, documents, media oldest, by name   | 28–34 ms   |
| After: the same page again, nothing changed            | 2 ms (p50) |

Before, the open view read and decrypted every message of every conversation on a 15-second timer.
Now the sealed catalog (version 3) marks the messages with files and with documents, the core merges
the conversations newest first and reads only the messages the page shows plus about one per
conversation, and keeps the page until a message with files is written or removed
(`LocalStore::attachments_cursor`): the view reads on every change, no timer. The first read includes
building the conversation list for the first time in the process (the sidebar has done that by the
time anyone opens Files). Sorting by name or size orders the newest files loaded so far; the view
says so. The core times the page as the step `files_page`.

The catalog's version 3 is built again once when a store of an earlier version opens
(`measure_the_catalog_rebuild` in `store/sealed.rs`, 101,801 records, release, desktop). Each batch
of the build rewrites the index nodes it touched, so the batch size decided the cost:

| Build batch                    | 100,000 messages in 300 conversations |
|--------------------------------|---------------------------------------|
| 512 records (before)           | 10.3 s (101 µs per record)            |
| 4,096 records (now)            | 2.6 s (25 µs per record)              |
| 16,384 records (not taken)     | 1.3 s (13 µs per record)              |

4,096 keeps a batch's records to a few MiB on a phone; the web build still yields between batches
and shows its progress.

### Links tab on a large workspace (issue #609)

`crates/teamcraft-core/tests/suite/links_budget.rs` measures the Files & links tab's links: a sealed
SQLite store with 300 conversations and 100,000 messages, half in one. There every 1,000th message
has a link (few links in a long history); in the others every 10th. Release build on a desktop
machine, median of 10 reads.

| Measured                                    | Before (scan) | After (index) |
|---------------------------------------------|---------------|---------------|
| Large conversation, sparse links, 1 page    | 209 ms        | 8.5 ms        |
| Large conversation, sparse links, 2 pages   | 336 ms        | 13 ms         |
| Small conversation, dense links, 1 page     | 0.98 ms       | 0.03 ms       |
| Peak memory (105 MiB after seeding)         | 111 MiB       | 109 MiB       |

Before, the tab read the conversation's messages newest first, 200 at a time, until the page was
full: in a long conversation with few links that is most of its history, decrypted. Now the sealed
catalog (version 4) marks the messages the channel shows whose text has a web link, and the core
walks the index's entries (no decryption) and opens only those messages. What is left of the 8.5 ms
is walking 50,000 entries of the large conversation's index. The catalog is built again once when a
store of version 3 opens, at the cost measured for version 3 above.

### Catching up on the page's only thread (issue #1575)

On web the core runs on the page's only thread. A catch-up after a long absence decrypts and writes
conversation after conversation, and the server's answers often come faster than the work, so the
UI's input, its reads of the core and its paints waited until the catch-up was done. The client now
hands the thread back every 30 ms (`ClientSettings::with_thread_turns`, web only): between two
conversations, welcomes, member lists, MLS queue entries and live events, and within a page of
messages. Native hosts have a thread for the engine and one for reads; there the backlog's pages
are paced instead (`RunnerSettings::background_page_gap`, at least the page's own time and 100 ms).

Measured with `tests/e2e` `perf.rs`, phase `come_back_to_many_conversations` (small scale: 22
channels with 150 new messages each, all read in the catch-up itself; the browser's CPU slowed
down 4×, `PERF_CATCH_UP_CPU_SLOWDOWN`, because this machine is faster than the reporter's), on
one machine, 2026-11-09:

| Coming back to 22 conversations with 150 new messages each | Before   | After   |
|------------------------------------------------------------|----------|---------|
| Content phase of the catch-up                              | 11.7 s   | 6.9 s   |
| Main-thread tasks over 100 ms in the content phase         | 52       | 8       |
| Longest main-thread task in the content phase              | 310 ms   | 169 ms  |
| Main-thread tasks over 50 ms, page load until caught up    | 120      | 61      |
| Their total                                                | 18.4 s   | 7.1 s   |
| Channel switch while it catches up, p95                    | 663 ms   | 311 ms  |

What is left is not the catch-up's own work: the start of the page (the module, the store) and the
UI's own reads after each wake. On a large store (the reporter's has about 97,000 records, and the
lag shows on fast machines too), the UI's snapshot updates and entry counts cost far more than here;
GlitchTip shows `snapshot_update` up to 5 s and `entry_counts` around 350 ms. Those are reads, not
the catch-up, and are tracked on their own.

### Web bundle size

Two budgets, both on the **compressed** wasm (issue #910's follow-up, 2026-10-31). The strict one
is a **guest's meeting**: somebody from outside who opens a meeting link, often once and in a
browser that never saw the app, downloads the lobby (the app, the service worker and the UI's texts
in one language), then, once let in, the meeting's split modules (`guest`, `calls`) with the shared
chunks they import, and the call module. The looser one is a ceiling on a **signed-in first visit**:
the app, the signed-in app's pack (its chunks in one file, #1465) that `init.js` fetches next to it, the texts, and the service
worker, which is installed on the first visit too; a member loads it once, then from the service worker's cache. That is what a user waits for: the server sends the wasm
with gzip or brotli, the browser compiles it while it streams (`instantiateStreaming`), and the
service worker serves it from its cache on every later visit. The uncompressed size of a Leptos app
of this scope is not a useful budget on its own; 10 MB uncompressed was reached with the features of
v1, and nothing cheap is left to cut (below).

`.github/workflows/web-size.yml` builds the release bundle on every change to the crates in it and
runs `scripts/check-web-size.py`. The script prints uncompressed, gzip -9 and (when the `brotli`
module is installed) brotli -11 sizes, the gzip delta to `main` and a guest's first visit, and lists
the chunks that load later. The limit:

| Limit                                | Value        | Above it                         |
|--------------------------------------|--------------|----------------------------------|
| A guest in a meeting: gzip -9 total  | 6,440,000 B  | fails; warns from 95 % (6.12 MB) |
| Signed-in first visit: gzip -9 total | 5,610,000 B  | fails; warns from 95 % (5.33 MB) |
| The call module: gzip -9             | 1,100,000 B  | fails                            |

Both budgets are the sizes of #1699 below plus about 5 % since 2026-11-11. From 2026-11-10 they
were 10 MB: new features (polls #1689, the local search index #1693) had pushed a signed-in first
visit to 5.39 MB (CI, PR #1692), over its 5.37 MB budget, and a guest's meeting to 6.17 MB of
6.18 MB; by #1699 they had grown on to 5.56 and 6.33 MB. Before that they were the sizes of #1462 below plus about 6 % for a signed-in first visit and about 5 %
for a guest (6 % would have raised it, as the guest's meeting had grown since #1452), so they warn
only after real growth; before that they were 6.19 and 5.39 MB (#1452, #1465; 5.65 MB before the
pack), 6.28 and 5.74 MB (#1437), 6.35 and 5.8 MB (#1413), 6.8 and 6.2 MB, the sizes of #1148 plus about 6 %. For a while they were 10 MB, because the guest's meeting had reached its budget of the time
(5.7 MB) and blocked unrelated pull requests; at 10 MB the check caught nothing. Before that the
guest budget was the guest's meeting of 2026-10-31 plus about 4 %, the signed-in ceiling the
signed-in first visit on `main` 8d653d3c plus about 10 %. The single budget before (4,700,000 B,
the signed-in first visit on `main` c55b2e58 plus about 10 %) only counted what a guest's lobby and a
member's first screen load; a guest who was let in then loaded the whole signed-in app, uncounted. gzip -9 is the measure
because every server and browser supports it and it is reproducible; brotli is printed as
information. The uncompressed size is printed but not limited: what a user waits for is the
transfer. The earlier growth ratchet on it (11.85 MB) was dropped with the split (#334): main had
outgrown it anyway, and the split moves code between files without changing what matters. Raising
the budget is a decision, not a fix.

The call module's budget was 1,000,000 B (0.86 MB with the effects' compositors, #439, plus about
15 %) until the call problem reports (#1762, 2026-11-12) took the module to 1.02 MB; it is
1,100,000 B since, a decision of the maintainer: the reports pay for themselves in field bugs found
without asking anybody for logs.

#### The snapshot's JSON and the data export out of the first visit (#1699, 2026-11-11)

Ranked as the issue describes (the release bundle as `web-size.yml` builds it, then
`teamcraft-web-bundle` again without wasm-opt, function bodies by `twiggy` per file, files grouped
into the app, the signed-in app's pack and a guest's meeting by the split loader), the largest parts
neither a guest nor a member's first screen needs were JSON again and the data export:

- **The snapshot and feed reads went through JSON in the page.** The UI read the sidebar snapshot,
  what changed in it, a feed's page and a thread with `tauri::call`, which in the browser handed
  them to the page's core as a `serde_json::Value` and read the answer back from one: the app
  carried the writers of `ChatSnapshot`, `SnapshotChanges`, `MessageWindow` and the messages, the
  signed-in app their readers. They are typed now (`remote_backend::CoreRead`, `web_core::read`), as
  the chat actions and queries are since #1413; in JSON they only travel to the desktop shell
  (`areas::shell_read`, in `shell`) and between two tabs of an account (`areas::read_in_owner_tab`
  and the owner's side in `tabs`).
- **Every answer type had two JSON readers.** `tauri::call::<T>` read the desktop shell's answer
  from text and the page core's from a `Value`, both per type, and the browser only ever used the
  second. The shell's answer is read into a `Value` first now, so each type has one reader (about
  0.14 MB of the signed-in app before wasm-opt).
- **The data export** (#211: the archive's writers, its checkpoint and the personal data's JSON,
  about 75 kB of the app before wasm-opt) runs its steps in `settings`
  (`areas::export_step`), next to its card, which the module is loaded for anyway. A step whose
  module can't load ends the export with a network failure in the card, which offers to start it
  again.

Release `trunk build` on Linux (trunk 0.22.0, wasm-opt 131, Rust 1.99.1, the CI job's commands),
`main` 11c61e40, gzip -9:

| Download                                      | Before      | After       | Delta      |
|-----------------------------------------------|-------------|-------------|------------|
| A guest in a meeting (lobby, meeting, calls)  | 6,330,576 B | 6,125,773 B | −204,803 B |
| Signed-in first visit                         | 5,555,281 B | 5,341,057 B | −214,224 B |
| The app (in both)                             | 2,962,648 B | 2,861,790 B | −100,858 B |
| Signed-in app (pack)                          | 2,099,637 B | 1,986,271 B | −113,366 B |
| `settings` (with the settings)                | 0.35 MB     | 0.41 MB     |            |
| `tabs` (when a second tab asks)               | 0.17 MB     | 0.18 MB     |            |

The service worker (427,159 B) and the texts (65,837 B) did not change. Both budgets are these
sizes plus about 5 % (above).

Measured and left, for #1699's follow-ups:

- **Polls** (#1689): the poll card, voting and closing are about 30 kB of the signed-in app before
  wasm-opt (under 10 kB gzip), and the dialog is in `dialogs` already. A card that loads would show
  a placeholder in the message list the first time a poll scrolls in, for little.
- **The member-only rounds of the engine's upkeep** (role signing, about 0.13 MB of the app before
  wasm-opt with `Upkeep::tick`'s part): only a guest would gain, and it needs `Upkeep::tick` in
  teamcraft-core to hand those rounds to the shell, which then runs them in `session`.
- **The guest's join page** (about 50 kB of the app before wasm-opt): only a signed-in first visit
  would gain; in `guest` it would make the lobby wait for the whole meeting's chunks, in a module of
  its own a guest pays a round trip before the name form.
- **The desktop-only parts of every page** (`auth::tauri` and its callers, about 0.11 MB of the
  signed-in app before wasm-opt): spread over 40 files; each needs its own split point.
- **Crash reporting** (about 0.16 MB of the app before wasm-opt): its tracing layer and panic hook
  run from the start; only the sending (`crash_reports::flush`, `refresh_config`) could wait.
- **The REST answers' readers** (`GetMeResponse`, meetings, users, announcements; about 0.11 MB
  of JSON readers from text in the signed-in app) are used by the browser and stay.

#### The console's texts load with the console (#1462, 2026-11-08)

`admin.ftl` was about a quarter of the language file every page fetches, for the few people who
open the console. It is now a file of its own per language (`i18n/<language>-admin-<hash>.ftl`,
17 kB gzip in English, 19 kB in German). `scripts/finish-loader.sh` writes it, and the console's
area fetches it before it builds its view (`I18n::load_admin_texts`, `areas::admin_console_area`).
A failed fetch shows the area's retry. Once the console was opened, a switch of the language waits
for that language's file too. A signed release lists the files as `lazy`, like the languages.

Before and after on the same tree (`trunk build --release`, then `scripts/check-web-size.py`):

| gzip -9                                      | Before      | After       | Change     |
|----------------------------------------------|-------------|-------------|------------|
| Texts of one language (German, the larger)   | 76,161 B    | 58,190 B    | −17,971 B  |
| App                                          | 2,644,388 B | 2,645,153 B | +765 B     |
| Signed-in app (pack)                         | 1,969,826 B | 1,969,721 B | −105 B     |
| Service worker                               | 401,556 B   | 401,556 B   | 0          |
| **A signed-in first visit**                  | 5,091,931 B | 5,074,620 B | −17,311 B  |
| **A guest in a meeting**                     | 5,878,190 B | 5,860,861 B | −17,329 B  |

An admin pays one more round trip the first time the console opens: the texts first, then its
chunk, because a view built before its texts arrive would keep showing keys. Everything a place
outside the console shows moved to the files of those places (the announcement's dismiss label,
the roster banners, "not available yet", the alerts badge, the three delete and owner errors, the
copy button). The test `only_the_console_names_the_keys_of_admin_ftl` keeps it so: no literal key
or built key's start from `admin.ftl` in code outside `components/admin_*` and `admin_live.rs`.

#### The service worker reads only what a notification shows (#1452, 2026-11-08)

The service worker's wasm (`teamcraft-web-sw`) had grown from 0.39 to 0.49 MB gzip since
2026-10-31, in both budgets. By `twiggy` over a build with names (`cargo build -p teamcraft-web-sw
--target wasm32-unknown-unknown --profile web-release` with `strip` off), more than a fifth of its
1.57 MB of code before wasm-opt were JSON readers of things a notification never shows:

- **Every kind of message.** A push reads one message, from the store or fetched and decrypted, to
  show its text, its first file's name or that somebody was added. It read it as the whole
  `MessageContent` (an AI notetaker's request, an integration source's change, a conversation
  event of every kind, an imported message's origin, the workspace head in a message's metadata),
  and the store read every record, an entry's too, as the `Record` enum that holds a whole
  `StoredMessage`. Both are internally tagged, so serde buffers them first.
  `push_preview::ShownContent` now reads only what a notification shows, from the same JSON (and
  from a `MessageContent` the same way, which the tests check for every kind); the store reads a
  message for a notification with `LocalStore::shown_message_by_server_id`, which the browser's
  store (`SealedStore`) answers from that part of the record, and reads an entry without the reader
  of a message's record. The plaintext is decrypted and its signature checked exactly as before
  (`decrypt_plaintext`, split from `decrypt_content`); only the reading after it is narrower.
- **The whole workspace state.** `push_content_mode` read the verified `WorkspaceState` (roster,
  every policy with its defaults, the AI models' names) for one setting. It reads the policy's
  notification settings now, with the fields the policy can't do without still required, so a state
  that didn't read before still shows nothing but "New message".

Together they take the worker's code before wasm-opt from 1.57 to 1.22 MB.

What is left in the worker is mostly its job: the encrypted store, the key bag, the wake-up's
crypto, Web Crypto, IndexedDB, and the REST client with DPoP for fetching the message, marking it
read and a rotated subscription (`reqwest`, `http`'s header table, `url`, about 70 kB before
wasm-opt; a fetch of its own would drop them, #1462). Release verification is in
`sw.js` and unchanged.

Release `trunk build` on macOS (trunk 0.22.0, wasm-opt 131, the CI job's commands), `main`
90d31f25, gzip -9:

| Download                                     | Before      | After       | Delta      |
|----------------------------------------------|-------------|-------------|------------|
| A guest in a meeting (lobby, meeting, calls) | 5,927,664 B | 5,840,596 B | −87,068 B  |
| Signed-in first visit                        | 5,418,330 B | 5,331,316 B | −87,014 B  |
| The service worker (in both)                 | 492,090 B   | 401,482 B   | −90,608 B  |
| The app (in both)                            | 2,620,026 B | 2,623,579 B | +3,553 B   |

The app is slightly larger: it carries the narrower readers next to the whole ones.

#### The UI's texts as files of their own (#1437, 2026-11-08)

Both languages' Fluent files were in the app's data (0.5 MB of text without comments, 0.14 MB gzip),
which the split doesn't divide: every page carried both, and parsed both before the first view. The
web app now embeds none. `scripts/finish-loader.sh` (the Trunk hook that already put the loading
screen's texts into `init.js`) joins each language's files into one file of the build,
`i18n/<language>-<hash>.ftl`, and names them in `init.js`. `init.js` fetches the language the app
starts in (the saved setting, else the browser's language, as `settings.rs` decides) next to the
app, and the app mounts only once it is there (`i18n::load_start_language`), so no view ever shows a
message's key; without the file the loading screen shows its error with a retry. A switch to the
other language fetches it, and the UI stays in the one it shows until it is there
(`I18n::set_locale`). Native builds (the tests) still embed every file. The signed release lists the
files as `lazy`: the service worker fetches the language a page asks for, a guest never downloads
the other one, and a member's worker fetches it in the background with the other lazy files.
`check-web-size.py` counts the larger language (German) in both budgets.

Release `trunk build` on macOS (trunk 0.22.0, wasm-opt 131, the CI job's commands), `main`
91c2166d, gzip -9:

| Download                                      | Before      | After       | Delta      |
|-----------------------------------------------|-------------|-------------|------------|
| A guest in a meeting (lobby, meeting, calls)  | 5,984,442 B | 5,923,572 B | −60,870 B  |
| Signed-in first visit                         | 5,476,064 B | 5,416,417 B | −59,647 B  |
| The app (in both)                             | 2,760,505 B | 2,619,541 B | −140,964 B |
| The texts, German (English: 66,911 B)         | in the app  | 75,689 B    |            |

With English texts both visits are 8.8 kB smaller still. What is left is #1452: the service worker
(0.49 MB gzip, in both budgets), the admin console's texts with the console (17 to 19 kB gzip of the
language file), and the rest of #1437's list.

#### The UI's JSON and the engine's member parts out of the app (#1413, 2026-11-08)

By the split's list (as below), the largest single function of the app was the JSON reader of
`ChatAction` (`Deserialize` from a `serde_json::Value`, 0.29 MB before wasm-opt), with the writer of
`ChatAnswer` (69 kB) and the readers of their fields: 0.65 MB of the app's 8.2 MB of code. The
page's UI handed every action and query to the core in the same page as JSON, as it does to the
desktop shell. It hands them over typed now (`web_core::act`, `web_core::query`); in JSON they only
travel between two tabs of an account (#744), and that code is in a module of its own (`tabs`,
loaded when another tab of the account asks something of this one). The desktop shell's JSON is in
one too (`shell`, which only the desktop app loads), and the UI's cache of answers keeps each query
beside its key instead of reading the key back as JSON. Without these two, the JSON reader of
`ChatAnswer`, the writer of `ChatAction`, the reader of `ChatQuery` and the readers of their fields
(0.8 MB before wasm-opt, 0.18 MB gzip) had only moved from the app into the chunks the signed-in app
and a guest's meeting import.

The engine's commands only a member's places of the app send (`CommandPart` in teamcraft-core
`ui_backend.rs`) run in those places' modules: spaces, channels, their categories, members, roles
and invitations in `workspace`, the organization's call backgrounds in `admin`
(`Client::run_workspace_command`, `run_admin_command`; `run_shared_command` in the app). The same
for the engine operations of a few chat actions (`web_core::EnginePart`): a new channel or member
(`workspace`), a new recovery passphrase or key (`settings`), the console's policy, roster and user
operations (`admin`). Their code was inlined into the command dispatch (`Client::run_command`, one
195 kB function). Natively nothing changed: `run_ui_command` runs every part. A chunk that can't
load fails the request as a network failure.

Also out of the app: the public download page of a share link (`share`, 37 kB before wasm-opt). And
"add to call" moved from `calls`, which a guest's meeting loads, to `dialogs`.

Release `trunk build` on macOS (trunk 0.22.0, wasm-opt 131, the CI job's commands), `main`
f3c055f8, gzip -9:

| Download                                      | Before      | After       | Delta      |
|-----------------------------------------------|-------------|-------------|------------|
| A guest in a meeting (lobby, meeting, calls)  | 6,396,260 B | 5,974,383 B | −421,877 B |
| Signed-in first visit                         | 5,830,884 B | 5,466,226 B | −364,658 B |
| The app (in both)                             | 2,986,600 B | 2,751,820 B | −234,780 B |
| `tabs` (when a second tab asks)               | –           | 337,038 B   |            |
| `shell` (the desktop app only)                | –           | 178,357 B   |            |

`tabs` and `shell` are counted with every chunk they import; most of their JSON code is in a chunk the
two share.

What was left, with where it is, was #1437: the translations (left out here, as the console's
strings changed at the same time in #1433; done in #1437, above), the member-only rounds of the engine's upkeep (role
signing), device linking, the guest's join page and the desktop-only parts of every browser page,
the snapshot reads that still go through JSON in the page, and the service worker's growth.

#### Back down from 10 MB (#1148, 2026-11-07)

While the budgets stood at 10 MB, a guest's meeting grew from 5.69 MB (CI, when they were raised) to
6.69 MB and a signed-in first visit from 5.06 to 6.12 MB; most of it is the app, which every page
loads (3.3 MB gzip). By the split's own list of what goes where (`wasm_split_cli_support` with
`verbose` and tracing at `TRACE`, function bodies before wasm-opt), the largest single part of the
app that neither a guest nor a member's first screen uses was the Public Suffix List (`psl`,
0.92 MB of the app's 9.2 MB of code): `app_link::next_step` names the owner of a host with it when a
link from outside the app asks before opening another instance (#1190). Only the desktop app
receives such links, but `AppLinks` is set up on every page, so the list was linked into the app.
That decision now runs through `areas::app_link_step`, in a module of its own (`links`, loaded
with the first such link): in `dialogs`, every member's first emoji picker or profile would have
loaded it too. It shares almost nothing with the other modules, so it adds two files, not new
combinations of shared chunks.

Release `trunk build` on macOS (trunk 0.22.0, wasm-opt 131, the CI job's commands), `main`
e978c1ef, gzip -9:

| Download                                      | Before      | After       | Delta      |
|-----------------------------------------------|-------------|-------------|------------|
| A guest in a meeting (lobby, meeting, calls)  | 6,693,694 B | 6,430,159 B | −263,535 B |
| Signed-in first visit                         | 6,124,592 B | 5,861,242 B | −263,350 B |
| The app (in both)                             | 3,299,902 B | 3,036,323 B | −263,579 B |
| `links` (with the first link from outside)    | –           | 264,157 B   |            |

Still in a guest's download, and the next candidates (#1148's follow-up):

- **The client engine in the app** (`teamcraft_core::client` 1.5 MB, `teamcraft_client_api::remote`
  0.73 MB, `teamcraft_proto::api` 0.5 MB before wasm-opt): a guest uses the meeting chat's part of
  it, but all of it is in the lobby. Splitting it means splitting the engine's own types, not views.
- **The translations** (both languages, 0.56 MB of Fluent text, in the app's data, which the split
  doesn't divide): the admin console's alone are 0.13 MB.
- **The image codecs `image_dimensions` reads headers with** (0.11 MB before wasm-opt), still in the
  app since 1.0.8.
- **What the `calls` module brings that a guest may not use**: it also holds the "add to call"
  dialog; the shared chunks `calls` imports and `guest` doesn't are 0.16 MB gzip.

#### A guest's meeting (2026-10-31)

A guest's meeting view (`GuestMeeting`, after a host let them in) was a split point of the
`session` module, the signed-in app's: a guest loaded the whole member app (sidebar, home, sign-in,
settings entry points) to see one meeting chat and its call. It is a module of its own now,
`guest`; what it shares with the signed-in app goes into shared chunks, so a guest loads those
and nothing that only members use. Release `trunk build` in the local CI image (trunk 0.22.0,
wasm-opt 131), gzip -9, every file once:

| Download                                      | Before (`8d653d3c`) | After       | Delta      |
|-----------------------------------------------|---------------------|-------------|------------|
| A guest in a meeting (lobby, meeting, calls)  | 5,911,682 B         | 5,390,863 B | −520,819 B |
| The lobby (app, service worker)               | 2,877,916 B         | 2,878,421 B | +505 B     |
| Signed-in first visit                         | 4,700,571 B         | 4,724,318 B | +23,747 B  |

The signed-in first visit grows by the headers of the new shared chunks. Still in a guest's
meeting and the next candidates: the call screens' module imports a large set of shared chunks
(1.06 MB with them), and the lobby's app carries the client core a guest without an account uses
only in part.

#### The split (#334)

The app is split into wasm chunks (`crates/teamcraft-ui/src/areas.rs`, built by
`crates/teamcraft-web-bundle` from the Trunk hook `scripts/build-app.sh`; Trunk 0.22 can't split):

- **The signed-in app** (`session`: sign-in, the shell, everything a member uses) is a chunk of its
  own. `init.js` fetches its files, as one pack since #1465, next to the app's for every page except a guest's meeting link,
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
  banner and the "open in another tab" page (`dialogs`; below); since 1.0.25 the decision about a
  link from outside the desktop app, with the Public Suffix List (`links`, #1148), and the JSON of
  the UI's actions and queries between two tabs (`tabs`) and to the desktop shell (`shell`), the
  engine's commands of the workspace dialogs and the console (with `workspace`, `admin`) and a share
  link's download page (`share`, #1413; above). `lazy_area` shows a spinner when a chunk takes a
  moment and an error with a retry when it can't load.
- Code several modules share goes into shared chunks (one per combination of modules), so each file
  is downloaded once.
- The call engine (`teamcraft-web-calls`, #330) stays a module of its own that loads in the
  background after sign-in; the budget doesn't count it.
- The background effects (#439) add their compositors (WebGPU and WebGL2) to it, about 57 kB gzip;
  pictures are decoded by the browser. The person segmentation is a module of its own,
  `teamcraft-web-segmentation` (tract, built with WebAssembly SIMD: 1.23 MB gzip), which the call
  module runs in a worker, and the model (0.56 MB, 0.27 MB gzip) a file of its own: both are only
  downloaded when someone first turns an effect on. Since #1295 the model comes as NNEF, tract's own
  format, which the module reads without tract's ONNX parser: 2.09 → 1.59 MB gzip (6.98 → 5.52 MB),
  the same masks. Since #1336 NNEF's registry of tract's core operators has only those the model
  uses (a vendored tract-nnef's `minimal_core_registry`, crates/vendor/tract-nnef/VENDORED.md):
  1.59 → 1.23 MB gzip (5.52 → 4.29 MB), FFT, LSTM, GRU, scans and the quantized operators left
  out. The rest is tract's runtime and optimizer and its kernels. A photo background's full picture (0.15 to
  0.4 MB, JPEG) is a file of the web release too, downloaded when the photo is picked. A signed
  release lists them in `on_demand`, so the service worker doesn't fetch them in the background. `check-web-size.py` keeps the call
  module under 1 MB gzip.
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
  Since #135 the web app has no image codecs at all: the engine's `PictureCodec` is the browser's
  (teamcraft-core `thumbnail/browser.rs`: an off-page `<img>` decodes, a canvas scales and
  encodes, with the native contract's sizes, steps and pixel limit), `image_dimensions` reads headers with
  `image_header.rs`, and the crop dialog crops in a canvas. The `images` chunk is gone; `image` is
  a native-only dependency of teamcraft-core.
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
| image, zune-jpeg, image-webp, png, fdeflate  | 3 %   | attachment thumbnails (`thumbnail.rs`); gone since #135 |

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
