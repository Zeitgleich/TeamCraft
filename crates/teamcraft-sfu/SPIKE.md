# Spike: QUIC datagram SFU with DAVE

Goal: check that the call design in ARCHITECTURE.md §6 holds up in code – RTP in WebTransport
datagrams, DAVE end-to-end encryption via `davey`, an SFU that forwards without decrypting, and
browser access over WebTransport.

## What was built

| Piece                                    | Where                                    | Status                               |
|------------------------------------------|------------------------------------------|--------------------------------------|
| RTP (de)serialization, audio level       | `teamcraft-media/src/rtp.rs`             | RFC 3550 + RFC 8285 one-byte + 6464  |
| TeamCraft datagram framing               | `teamcraft-media/src/datagram.rs`        | 5-byte header: version, layer, id    |
| Jitter buffer                            | `teamcraft-media/src/jitter.rs`          | Packet-count based, reorder + gaps   |
| DAVE wrapper (participant + ext. sender) | `teamcraft-media/src/dave.rs`            | 2–3 party group, add/remove, codes   |
| WebTransport SFU                         | `teamcraft-sfu/src/server.rs`, `room.rs` | Join with token, datagram forwarding |
| Top-N speaker selection                  | `teamcraft-sfu/src/speakers.rs`          | Smoothed level, silence never wins   |
| DAVE membership coordinator              | `teamcraft-sfu/src/dave_coordinator.rs`  | External sender, batching, leaves    |
| Join token trait + dev stub              | `teamcraft-sfu/src/auth.rs`              | `dev:<call>:<participant>`           |
| End-to-end test (4 native clients)       | `teamcraft-sfu/tests/dave_call.rs`       | Passes, prints latency/overhead      |
| Browser test page                        | `teamcraft-sfu/static/dev.html`          | Transport only, no DAVE in browser   |

`teamcraft-media` builds for `wasm32-unknown-unknown` with and without the `dave` feature.

### Wire layout of one audio datagram

```text
QUIC DATAGRAM frame (HTTP/3 quarter stream id prefix, added by wtransport)
└─ TeamCraft header (5 B): version | layer | stream id
   └─ RTP header (12 B) + one-byte extension block with audio level (8 B)
      └─ DAVE frame: encrypted Opus + supplemental data (tag, nonce, size, 0xFAFA marker)
```

The SFU assigns `stream id` on join and only forwards datagrams whose stream id belongs to the
sending session, so a client can't inject media as someone else. It reads the RTP header for the
audio level and never touches the DAVE payload.

### DAVE flow

The SFU plays the role of Discord's voice gateway (MLS external sender):

1. On join the SFU sends its `ExternalSender` package; the client creates a pending group and
   answers with a key package. The SFU checks that the key package's credential is the joining
   participant.
2. The first two participants found the group: the earlier one keeps its epoch-0 group and
   commits the other's Add proposal.
3. Later joins and leaves become Add/Remove proposals for the current epoch, sent to all members.
   Every member commits; the first commit wins and is announced to everyone, joiners get the
   welcome. Changes arriving while a transition is in flight are batched into the next one.
4. For Remove proposals the coordinator mirrors the ratchet tree's leaf assignment (leftmost blank
   leaf, RFC 9420 §7.7).

## Measurements

Loopback, one machine, 4 participants (3 speaking, 1 silent), 20 ms frames with a 60-byte payload:

| Path                                            | p50     | p99     | max     | Loss |
|-------------------------------------------------|---------|---------|---------|------|
| Native, debug build (encrypt → SFU → decrypt)   | 0.58 ms | 0.90 ms | 1.1 ms  | 0    |
| Native, release build                           | 0.15 ms | 0.35 ms | 0.41 ms | 0    |
| Chromium, two tabs, no DAVE (dev page, avg/max) | ~0.7 ms | –       | 5 ms    | 0    |

Per-packet overhead for a 60-byte Opus frame:

| Layer                                                               | Bytes  |
|---------------------------------------------------------------------|--------|
| DAVE supplemental data                                              | 12     |
| RTP header + audio level extension block                            | 20     |
| TeamCraft datagram header                                           | 5      |
| **Datagram payload total**                                          | **97** |
| QUIC short header + AEAD tag + frame + quarter stream id (estimate) | ~30    |
| UDP + IPv4                                                          | 28     |

≈155 bytes on the wire per 20 ms frame, i.e. ~62 kbit/s for a ~24 kbit/s Opus stream. That is in
the same range as SRTP over UDP (no DTLS/ICE overhead, but QUIC's tag and header instead of SRTP's).
Chrome reported `maxDatagramSize = 1024` at connection start.

## Problems and findings

1. **`davey` panicked in the browser** because it used `std::time::Instant`. Fixed in the vendored
   copy with `web-time` (M6, see `crates/vendor/davey/VENDORED.md`); a `wasm-bindgen-test` runs a
   DAVE round trip in Node and Chromium. The WASM build also needs `getrandom` 0.2 with `js`
   (a target dependency of `teamcraft-media`).
2. **`davey` maturity.** Version 0.1.x, one maintainer, few tests, no audit of its own (DAVE and
   OpenMLS are reviewed, this binding isn't). It panics on some unexpected inputs instead of
   returning errors (`process_proposals` when a welcome is missing, `expect` in `reset`). Plan a
   review and upstream fixes before relying on it.
3. **The external sender can add anyone.** MLS external proposals are only as trustworthy as the
   external sender. `davey` lets members pass `expected_user_ids`; in the spike the SFU supplies
   that list itself, which protects nothing. The roster must come signed from `teamcraft-server`
   (or the chat MLS group) and be verified by clients (e2ee.md §3). Related: DAVE credentials are
   bare 8-byte user ids (MLS `BasicCredential`) without a binding to TeamCraft device identity
   keys; only the out-of-band verification codes (`DaveParticipant::verification_code`) cover that.
4. **Leaf tracking is mirrored, not observed.** The coordinator replicates MLS leaf placement to
   build Remove proposals. It works in the tests (including leaf reuse), but a cleaner option is
   to parse the public commits (DAVE uses plaintext wire format) to track the tree.
5. **No recovery path yet.** If no member commits (all hang) or a commit/welcome is invalid
   (DAVE `invalid_commit_welcome`), the transition stays in flight. Needs a timeout and re-add.
6. **Signaling encoding.** Moved to `teamcraft-proto` (M6) with base64 instead of JSON number
   arrays (~1.33× instead of ~3×) and per-connection event sequence numbers. A binary encoding is
   still an option if signaling volume matters.
7. **Dual stack.** The SFU binds `[::]` with `IPV6_V6ONLY` off by default (ARCHITECTURE.md §1.2)
   and accepts IPv4 clients as IPv4-mapped addresses; the end-to-end test runs over IPv4, IPv6 and
   a mixed call, and two browser tabs (`http://[::1]:…` and `http://127.0.0.1:…`) talked through one
   SFU. Plain-HTTP pages are only a secure context on loopback, so testing from another machine
   needs the page served over HTTPS (or a real certificate on the SFU).
8. **No UDP, no call.** WebTransport needs UDP (HTTP/3). There is no TCP/TLS fallback without
   WebRTC/TURN, so restrictive networks can't join. Needs a decision (ARCHITECTURE.md §10).
9. **Not tried here:** Firefox (also check its `serverCertificateHashes` support) and Safari 26.4.
   Start the SFU in dev mode and open the page in each (see below).

## Trying it

```sh
TEAMCRAFT_SFU_BIND='[::1]:4433' TEAMCRAFT_SFU_DEV_HTTP='[::1]:8080' TEAMCRAFT_SFU_INSECURE_DEV_TOKENS=1 cargo run -p teamcraft-sfu   # SFU on [::1]:4433
# open http://127.0.0.1:8080/ and/or http://[::1]:8080/ in two tabs or browsers, click Connect
cargo test -p teamcraft-sfu --test suite dave_call -- --nocapture   # native DAVE call + numbers
```

The page shows browser feature support and sends 20 ms Opus frames (WebCodecs `AudioEncoder`) from
the microphone or a test tone, captured with `MediaStreamTrackProcessor` where available and an
`AudioWorklet` otherwise (the Firefox path). It computes the RFC 6464 level per frame, decodes and
plays received streams (`AudioDecoder`) and reports packets, loss, decoded frames, level and
one-way latency (from a development-only send-time header extension, id 14). "Synthetic bytes"
keeps the old codec-free mode with a fixed level. Checked in Chromium (both capture paths):
~0.3–0.8 ms average one-way latency between two tabs, no loss. Media is not DAVE-encrypted on this
page yet.

With real join tokens instead of dev tokens, configure `[calls]` on the server (see
`teamcraft.example.toml`) and give the SFU the matching public key:

```sh
TEAMCRAFT_SFU_JOIN_TOKEN_PUBLIC_KEY=call-signing.pub.pem \
TEAMCRAFT_SFU_JOIN_TOKEN_ISSUER=https://chat.example.com cargo run -p teamcraft-sfu
```

## M6 progress

Done on `feat/m6-calls`:

- Vendored `davey` uses `web-time`; DAVE verified in Node and Chromium via `wasm-bindgen-test`.
- Call signaling in `teamcraft-proto` (`calls` module): `CallClientMessage`, `CallServerMessage`
  with event sequence numbers, `CallEvent`, `CallId`, call error codes.
- Join tokens: `POST /api/v1/conversations/{id}/call/join-token` on the server (members only, own
  active device, 60 s EdDSA JWT); the SFU verifies signature, issuer, audience, times and a maximum
  lifetime. Dev tokens only in dev mode.
- Opus encode/decode for native clients (`teamcraft-media`, feature `native`) with FEC/PLC for
  jitter-buffer gaps; RFC 6464 levels from PCM.
- Top-N forwarding with cached selection, hysteresis and `active_speakers` events; routing logic in
  `forwarding.rs`, unit tested without network.
- Test page sends real Opus via WebCodecs and plays what it receives.

Done on `feat/m6-client`:

- **Call engine** (`teamcraft-media::call_engine`): platform-neutral, sans I/O (also wasm32). Joins
  with a join token and media capabilities, runs the DAVE member side, sends encrypted Opus with
  the audio level, decrypts received audio, sends transport feedback, mute/deafen, active
  speakers. Proposals are checked against a roster the app trusts (`set_trusted_roster`).
- **Native driver and client** (`call_native`, feature `native`): tokio + wtransport, cpal devices
  on an audio thread, Opus, jitter buffer and mixing; `teamcraft-call` joins headlessly with a test
  tone or the microphone. An SFU test runs three native clients end to end.
- **Audio processing**: WebRTC AEC3, noise suppression, AGC2 via `webrtc-audio-processing` 2.1,
  switched on at build time (`--cfg teamcraft_audio_processing`, see `crates/teamcraft-media/README.md`);
  without it audio passes through.
- **DAVE keys bound to devices** (e2ee.md §8): a fresh P-256 key per call, signed by the device key
  (`teamcraft-crypto::call_binding`); members verify bindings against device certificates and every
  member's leaf key before committing and after each epoch.
- **Bandwidth estimation** (`bwe`): transport sequence numbers in the datagram header, feedback over
  signaling both ways, a simplified GCC (delay trendline over 5 ms packet groups, loss, demand-based
  probing) tested over a simulated bottleneck.
- **Simulcast layers in the SFU**: publish/subscribe video tracks, per-receiver layer allocation
  with hysteresis and resume probes, keyframe-gated switches, keyframe and needed-layer requests to
  publishers; simulated receivers at 0.4/0.9/3 Mbit/s settle on layers 0/1/2.
- **Video skeleton**: I420 and encoded frame types, simulcast layer plans, codec choice, generic
  frame fragmentation with an unencrypted fragment extension, capability negotiation in `join`.

Open: video capture/encoding and the browser binding of the call engine are not wired; apps still
have to feed the server's call roster and verified device certificates into the engine
(`set_trusted_roster`, `DeviceCallKeyAuthenticator`), e.g. from `teamcraft-core`.

## Recommended next steps for M6

1. Build `teamcraft-media` with `wasm-bindgen` for the page/web client and add DAVE to the browser
   pipeline: WebCodecs Opus → DAVE encrypt (WASM) → datagram; receive → jitter buffer (WASM) →
   decode → `AudioWorklet` (the page still schedules decoded audio without a jitter buffer).
2. Signed roster from `teamcraft-server` for `expected_participants`; decide where the DAVE
   coordinator lives (SFU vs. server). The SFU should report ended calls so the server stops
   reusing the call id (today it expires after `call_idle_secs`).
3. A call permission in the permission scheme (today: every member of the conversation may join).
4. Coordinator robustness: commit timeout, invalid commit/welcome handling, membership tracking
   from public commits, tests for message loss and reordering.
5. Video: codec packetization within ~1000-byte datagrams, simulcast layer selection, keyframe
   requests over signaling, bandwidth estimation (transport-wide feedback + QUIC congestion state).
6. Audio quality: adaptive (time-based) jitter buffer, Opus FEC/PLC on `Playout::Missing`, native
   pipeline with `cpal` + `opus` + `webrtc-audio-processing`.
7. SFU performance: forwarding without the per-room mutex and without a copy per receiver for audio,
   load test (rooms × participants, CPU per forwarded packet).
8. Fallback decision for UDP-blocked networks.

## M6 design notes

Proposals, not decisions. Each one names what still has to be measured or checked.

### Bandwidth estimation

There are two legs, and each is estimated where its sender sits:

- **Uplink (client → SFU):** the client's own QUIC congestion controller limits what it can send.
  The client sets encoder bitrates from its send rate and from the layers the SFU says anyone
  needs (see "Sender pausing" below).
- **Downlink (SFU → each receiver):** the SFU estimates per receiver and decides what to forward.
  This is where most of the adaptation happens.

QUIC datagrams are congestion controlled (RFC 9221) but never retransmitted. When the congestion
window is full, quinn drops datagrams from its send buffer instead of queueing them forever. The
estimator therefore has these inputs, cheapest first:

1. **QUIC state per connection:** RTT, congestion window, lost packets, congestion events (quinn
   connection stats), plus datagrams dropped locally because the send buffer was full. Enough for
   a loss- and window-based estimate in the first version.
2. **Transport-wide feedback:** receivers report arrival times of a per-connection transport
   sequence number (an extra unencrypted header extension the SFU stamps when forwarding) about
   every 100 ms over signaling. This feeds a delay-based estimator (GCC-style trendline) that
   sees queues building before loss happens. Add it once the loss-based version is measured.
3. **Browser side:** `WebTransport.getStats()` where available (RTT; send-rate estimates are not
   implemented everywhere yet), used for the uplink.

Congestion controller: CUBIC fills buffers and adds latency. Compare quinn's BBR with CUBIC on a
lossy, shaped link (netem) before choosing; this is a configuration setting, not hard-coded.

**Allocation per receiver** (budget = downlink estimate):

1. Audio of the top-N speakers (fixed, about 3 × 40 kbit/s).
2. Screen share, if subscribed (its own session per ARCHITECTURE.md §6.3).
3. Video by priority: pinned, then active speaker, then the rest of last-N, each up to the layer
   its viewport needs.

Switch up only after the estimate stays 10–20 % above the next layer's bitrate for 2–3 s; switch
down at once. To probe before switching up, briefly forward the next layer (it is useful data) and
roll back on loss or rising delay. Padding-only datagrams are the fallback probe.

### Simulcast and layer selection

- **Layers:** each camera track sends up to 3 spatial layers (e.g. 180p / 360p / 720p at roughly
  150 / 500 / 1500 kbit/s; configurable). Same `stream id` for the track, spatial layer in the
  4-bit `layer` field of the datagram header.
- **Temporal layers:** with VP9/AV1 SVC (`L1T3`) the SFU can drop temporal layers instantly
  without a keyframe, which is the fastest relief under congestion. The SFU needs to know which
  frames are droppable without decrypting them, so the dependency information (AV1 dependency
  descriptor or a small TeamCraft equivalent) travels in an unencrypted header extension, like
  the audio level. It reveals frame structure, not content.
- **Switching spatial layers:** only on a keyframe of the target layer. The SFU sends the sender a
  `request_keyframe { stream_id, layer }` over signaling (rate-limited, e.g. at most one per layer
  per 500 ms). Under congestion it drops temporal layers first, then switches down at the next
  keyframe of the lower layer.
- **No RTP rewriting:** receivers keep one sequence-number space per (stream id, layer) and reset
  the depacketizer at a layer switch, which always starts with a keyframe. The SFU then forwards
  the same bytes to every receiver (zero-copy fan-out), and the `layer` field earns its place in
  the header. Rewriting sequence numbers per receiver, as many WebRTC SFUs do, would cost a copy
  per receiver and per packet.
- **Inputs:** receivers subscribe per track with the largest layer they can show
  (`subscribe { stream_id, max_layer }` from viewport size, visibility, pinning). The SFU forwards
  `min(requested, affordable)`. Gallery view is last-N: only N video tracks are forwarded at all.
- **Sender pausing:** the SFU tells each sender which of its layers have at least one subscriber
  (`layers_needed`). The sender turns off unused encoders, saving CPU and uplink.
- **Packetization:** video frames are larger than one datagram (Chrome starts at 1024 bytes). DAVE
  encrypts whole frames, so the frame is split after encryption. Since there is no interoperability
  with WebRTC to keep, use one generic fragmentation format for all codecs (frame number, fragment
  index/count, keyframe flag, layer) in an unencrypted extension, instead of a separate RTP payload
  format per codec. To check: DAVE's unencrypted codec ranges (e.g. H.264 NAL headers) must still
  line up after fragmentation, and fragment loss must drop the whole frame.

### Browser capture and playout

Everything media-related runs in a dedicated worker: WebTransport, WebCodecs and the WASM build of
`teamcraft-media` (DAVE, jitter buffer) all work in workers. The main thread only renders UI.
Blocker first: `davey` must stop using `std::time::Instant` (see findings above).

**Audio capture**

- **With `MediaStreamTrackProcessor`** (Chromium; check Safari per version, it may be worker-only):
  transfer the track to the worker, read `AudioData` frames, feed `AudioEncoder` (Opus, 48 kHz,
  20 ms).
- **Firefox (no `MediaStreamTrackProcessor`):** `getUserMedia` (with the browser's echo
  cancellation) → `MediaStreamAudioSourceNode` → `AudioWorkletNode`. The worklet copies its
  128-sample blocks into a ring buffer shared with the media worker, which cuts 960-sample (20 ms)
  chunks, wraps them in `AudioData` and feeds `AudioEncoder` (Firefox 130+). Create the
  `AudioContext` with `sampleRate: 48000` so nothing resamples.
  - A `SharedArrayBuffer` ring needs cross-origin isolation (COOP/COEP headers on the web app).
    Plan for these headers from the start; without them use `postMessage` with transferred
    buffers (more latency and jitter).
- **No Opus in `AudioEncoder`** (checked with `isConfigSupported()`): WASM Opus encoder. libopus
  is C, so this needs a C-to-WASM build step; check for a maintained pure-Rust option first.

**Video capture**

- **With `MediaStreamTrackProcessor`:** `VideoFrame`s straight into effects (WebGPU/WebGL) and
  `VideoEncoder` in the worker.
- **Firefox:** play the camera track in a hidden `<video>` and create `new VideoFrame(video)` per
  frame, triggered by `requestVideoFrameCallback` (check Firefox version support; otherwise a timer
  at the target frame rate). `VideoFrame` is transferable, so the main thread only grabs frames and
  hands them to the worker. Expect slightly higher CPU and one extra frame of latency.
- Background effects run on the `VideoFrame` before encoding in both paths (PLAN.md §8).

**Playout (all browsers)**

- **Audio:** decoded `AudioData` → ring buffer → `AudioWorklet` output. The jitter buffer (WASM)
  decides when to play, PLC or FEC.
- **Video:** `VideoDecoder` → `OffscreenCanvas` in the worker. Use this everywhere rather than
  `MediaStreamTrackGenerator`, which is Chromium-only.
- **Feature detection order:** `MediaStreamTrackProcessor` in the worker, then on the main thread,
  then the worklet / `<video>` fallback. Report the chosen path in call diagnostics.
