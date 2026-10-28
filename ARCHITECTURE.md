# TeamCraft – Architecture

Self-hosted team chat with voice calls and meetings, written in Rust.
Target: one company plus external guests, hosted on an untrusted VPS. ~10 people today; no fixed user
limit – see §1.1 for scale targets.

> Status: design draft. Nothing here is implemented yet.

## 1. Goals and non-goals

**Goals**

- **End-to-end encryption for all message content, attachments, search indexes and calls.** The VPS
  operator (and anyone who compromises it) must not be able to read messages or listen to calls.
- **E2EE that users don't notice** (WhatsApp/Threema-level usability)
- Spaces (projects) with channels, DMs, group DMs, threads
- Presence and status (online, away, DND, in a call, in a meeting, out of office, custom status)
- 1:1 calls, group calls, scheduled meetings with e-mail invitations (iMIP) via CalCraft (CalDAV)
- Login via OIDC (reference setup: Keycloak); optional local accounts
- External guests who only see the spaces/channels they were invited to and cannot search the directory
- Clients: macOS, Linux, web, iOS, Android
- Optional: AI phone hotline on SIP trunks with handover to staff (docs/telephony.md)

### 1.1 Scale targets

Team size is not the hard part; **channel size** is. Channels that receive log or bot output grow to
several GB. The design must handle:

| Dimension                     | Target                                                                                   |
|-------------------------------|------------------------------------------------------------------------------------------|
| Users per instance            | hundreds (no hard limit in the design)                                                   |
| Messages per channel          | tens of millions, multi-GB channels (log/bot channels)                                   |
| Message rate into one channel | sustained bot traffic (hundreds per minute) without slowing other channels               |
| Call participants             | 50+ in one call (SFU forwards top-N speakers and simulcast layers, so cost grows slowly) |
| Group DM / channel members    | hundreds (MLS TreeKEM scales logarithmically)                                            |

Consequences:

- Clients never load a whole channel: history is paged, index segments are fetched on demand, phones
  don't prefetch huge channels.
- Postgres `messages` is partitioned by time; deleting old data (retention) drops/prunes partitions
  instead of row-by-row deletes.
- Per-channel **retention** (keep last N days or last N messages) is a v1 feature (PLAN.md §1).
- Bot/log channels can be marked as **integration channels** (§4.5).

### 1.2 Network requirements

- **Dual stack (IPv4 + IPv6) everywhere facing the outside:** API/gateway, SFU (QUIC), share-link
  downloads, SIP trunk signalling and RTP, push providers, S3, OIDC provider. Listening sockets bind
  `[::]` as dual-stack sockets (IPv4-mapped) or both families explicitly; clients use Happy Eyeballs
  (RFC 8305) when connecting.
- Internal loopback-only services may stay IPv4-only.
- Address handling never assumes IPv4: store and log addresses as `IpAddr`, format URLs with brackets
  for IPv6, ICE-free QUIC needs no NAT64 special-casing.

**Non-goals (for now)**

- Federation
- Server-side recording (E2EE; recording is client-side only)
- General file sharing (use a dedicated file service; TeamCraft only offers per-file share links)
- Plugin system / marketplace
- Public communities

## 2. Threat model

The server is **honest-but-curious, possibly compromised**:

| Server can see                                                                  | Server cannot see                           |
|---------------------------------------------------------------------------------|---------------------------------------------|
| Who exists, who is in which channel/call, when messages are sent, message sizes | Message text, attachments, reactions, edits |
| Encrypted blobs, RTP headers, audio levels (for speaker detection)              | Audio/video frames                          |
| Calendar data in CalCraft, invitation e-mails                                   | — (e-mail and CalDAV are not E2EE)          |

A malicious server could try to inject its own device keys for a user (MITM). Mitigation:
device cross-signing by the user's identity key, identity-key change warnings and optional
verification codes (see [docs/e2ee.md](docs/e2ee.md) §3).
Key transparency is a later option.

**Server entry and token relay.** The native apps take the server address from the user (or the
device's management) and the sign-in settings from that server (docs/operations.md "Server address
in the apps"). A server the user is lured to (a typo domain) can announce a real instance's issuer
and client ID; with a live SSO session the identity provider then hands an access token for the real
instance to the app, which sends it to the lookalike, and the lookalike can replay it at the real
server (bearer tokens are not bound to a client). Mitigations today: the confirmation screen shows
the identity provider's host and warns when it lies outside the server's domain, tokens are stored
per server origin and issuer, and E2EE keeps message content out of reach (the relayed token gives
API access, not keys). The real fix is sender-constrained tokens with DPoP (RFC 9449, issue #120).

Push goes through APNs/FCM, optionally via a **push gateway** operated by the publisher of the store
apps. Wake-ups are sealed to the device's push key before they leave the server, so neither Apple,
Google nor a gateway can read or forge them; a malicious gateway can only drop, delay or replay
(e2ee.md §7.1).

## 3. System overview

```
                         OIDC provider (separate server)
                                  ▲
┌──────────────┐  HTTPS / WSS  ┌──┴─────────────────┐  CalDAV  ┌──────────┐
│ Desktop      │──────────────▶│  teamcraft-server  │─────────▶│ CalCraft │
│ (Tauri)      │               │  REST API, gateway │          └──────────┘
├──────────────┤               │  auth, permissions │  SMTP
│ Web (WASM)   │               │  MLS delivery, push│─────────▶ e-mail (iMIP invites)
├──────────────┤               └──┬──────────┬──────┘
│ iOS, Android │                  │          │ call roster, join tokens
└──────┬───────┘                  ▼          ▼
       │                      Postgres   ┌────────────────────┐
       │  WebTransport / QUIC            │   teamcraft-sfu    │  (one or more nodes)
       └────────────────────────────────▶│                    │
          RTP in QUIC datagrams          └────────────────────┘

Clients ⇄ S3 (any provider) via presigned URLs: encrypted attachments, key bags, index segments
```

Binaries:

- **`teamcraft-server`** – REST API, real-time gateway (WebSocket), auth, permissions, MLS delivery service, CalCraft/SMTP integration. Starts as a single process (no Redis/NATS); stateless enough to run several instances behind a load balancer later.
- **`teamcraft-bridge`** – optional, runs next to log/webhook sources: encrypts their output into integration channels.
- **`teamcraft-pbx`** – optional telephony module: AI hotline on any SIP trunk (`rsipstack`), pluggable voice agent providers (first: ElevenLabs), queues, handover to agents in the app. No Asterisk.
- **`teamcraft-sfu`** – selective forwarding unit for calls. Separate so it can run on a box with good network and be restarted without dropping chat. Registers with the server using a registration token.
- **`teamcraft-push-gateway`** – its own repository ([Zeitgleich/teamcraft-push-gateway](https://github.com/Zeitgleich/teamcraft-push-gateway)): holds the APNs and FCM credentials of the published store apps and forwards the sealed wake-ups of any server to them, so self-hosted servers work with the store apps. Run by the app's publisher on a separate host; sees device tokens and ciphertext only ([docs/push-gateway.md](docs/push-gateway.md)).
- **`teamcraft-vault`** – optional, runs on company hardware (e.g. a NAS or on-prem server): compliance archive, file text extraction.

## 4. Chat

Feature scope (threads, status, notifications, files, search …): [PLAN.md](PLAN.md).

### 4.1 Data model (server, Postgres)

- `users` (id, oidc_sub or local credentials, display name, kind: `member` | `guest`)
- `identity_keys`, `devices` (public keys + cross-signatures, push token + push key, last_seen)
- `spaces` (id, name, icon, join policy) and `space_members` (space_id, user_id, role)
- `conversations` (id, space_id (null for DMs), kind: `dm` | `group` | `channel`, category, visibility, history policy)
- `memberships` (conversation_id, user_id, role, notification level, read position)
- `messages` (id, conversation_id, thread_id, sender device, content_key_id, mentioned user IDs, ciphertext)
- `mls_messages` (conversation_id, epoch, commits / welcomes / application messages for key distribution)
- `key_packages` (device_id, MLS KeyPackage)
- `key_bags` (user_id, encrypted key bag, wrapped backup keys)
- `attachments` (id, size, S3 key) – encrypted blobs; key lives inside the message
- `index_segments` (conversation_id, time range, S3 key, size) – encrypted search index segments
- `presence`, `custom_status`, `threads_followed`, `user_groups`, `audit_log`
- IDs: Snowflake-style, time-sortable, assigned by the server (total order per conversation)

### 4.2 Storage

- **Postgres** for metadata and message ciphertext. `messages` is partitioned by month, so retention and
  huge channels stay cheap; ciphertext above a size threshold goes to S3 with only a reference in the row.
- **S3** for attachments, avatars, key bag blobs and search index segments. Any S3-compatible provider works (Hetzner Object
  Storage, Backblaze B2, AWS, or self-hosted Garage). Since everything is encrypted client-side, the
  S3 provider is untrusted like the VPS. Uploads/downloads go directly client ↔ S3 with presigned URLs,
  so large files don't pass through the server.

### 4.3 Encryption

Summary; full design in [docs/e2ee.md](docs/e2ee.md):

- MLS (`openmls`) distributes keys; per-conversation symmetric content keys encrypt messages, so every
  device and every new member can read history from the server.
- Identity key per user cross-signs all devices → no "unverified session" prompts.
- New devices are approved from an existing device (prompt or QR code) or recovered with a passkey
  (WebAuthn PRF); optional organization recovery with two admins.
- Threema-style encrypted push: Apple/Google see nothing; the app (iOS Notification Service Extension)
  fetches and decrypts the message itself.

### 4.4 Consequences of E2EE

- **Search:** full history on every device via encrypted, shared `tantivy` index segments per
  conversation, synced through S3 and searched locally (e2ee.md §10).
- **Link previews:** generated by the sender's client; domain list enforced by clients (e2ee.md §12).
- **Compliance archive:** designed into the protocol, off by default (e2ee.md §11).
- **Admins cannot read channels** (except via the optional org recovery, see e2ee.md §6.3).
- **Bots** must be MLS members, so they run as clients on trusted machines.

### 4.5 Integration channels

Channels that receive machine output (logs, monitoring, CI, the hotline bot). They are **E2EE like
every other channel** – the VPS is untrusted no matter where the content comes from.

**Integration bridge (`teamcraft-bridge`)**

- A small headless TeamCraft client (container or single binary) that runs **where the data comes
  from** – next to the service producing logs, on a NAS or on-prem server, on the hotline host – never
  on the VPS.
- It is a bot device and member of the integration channels it serves; it holds their content keys.
- It exposes a **local** incoming-webhook endpoint compatible with the Slack/Mattermost webhook JSON
  format, plus a plain-text/NDJSON endpoint for logs, so existing tools (Grafana, Uptime Kuma, CI,
  scripts, `journalctl | curl`) work unchanged.
- It encrypts and sends. For high volume it **batches** lines into one message (e.g. per second or
  per 64 KB) and can rate-limit or drop noisy sources.
- Because it already has the plaintext, it **builds the search index segments** for its channels
  itself (e2ee.md §10) – no member client has to index gigabytes of logs.
- One bridge can serve many channels; each channel can also have its own bridge.

**Integration channel behaviour**

- Retention defaults suited to logs (e.g. 30 days), set per channel.
- No push notifications by default; alerts can still @mention people (e.g. monitoring "down" events).
- Compact log view (monospace, collapsible batches, level colouring), "follow" mode like `tail -f`.
- Phones never prefetch their history or index segments.

**Optional: server-readable mode**

For sources where running a bridge isn't possible, an admin can allow channels whose messages are
stored server-readable (webhooks post straight to the server, server-side search). Off by default,
clearly marked "Not end-to-end encrypted", only admins can create them, and an E2EE channel can never be
switched to it.

## 5. Identity, auth, permissions

### 5.1 Accounts

- **Members and guests:** both come from the company's OIDC provider (reference setup: one Keycloak
  realm on a separate server). Whether someone is a member or a guest in TeamCraft is a **client
  role**, not a separate realm.
- **Local accounts (optional):** for externals who shouldn't get a Keycloak account (Argon2id password +
  TOTP or passkey, created from an invitation link). Can reuse CalCraft's auth code.
- **Meeting-only guests:** no account; signed one-time join token from the invitation e-mail.
- Login only proves account ownership. Reading anything requires device keys (e2ee.md §3).

### 5.1.1 Keycloak setup

| Client             | Type                              | Used by                    | Purpose                                                                                                                                           |
|--------------------|-----------------------------------|----------------------------|---------------------------------------------------------------------------------------------------------------------------------------------------|
| `teamcraft`        | public, authorization code + PKCE | desktop, web, iOS, Android | User login. Redirects: web origin, desktop loopback `http://127.0.0.1:<port>/callback` (RFC 8252), mobile `com.example.teamcraft:/oauth/callback` |
| `teamcraft-server` | confidential, service account     | `teamcraft-server`         | Admin API access (read users, groups, roles, enabled state), OIDC back-channel logout endpoint                                                    |

**Client roles on `teamcraft`** (assigned to users or, better, to Keycloak groups):

| Role     | Meaning in TeamCraft                                        |
|----------|-------------------------------------------------------------|
| `owner`  | instance owner                                              |
| `admin`  | server admin                                                |
| `member` | employee                                                    |
| `guest`  | external person: only invited spaces/channels, no directory |

Client IDs and role names are configurable, so existing names work (example: client `chat`, role
`external` for guests):

```toml
[oidc]
issuer        = "https://auth.example.com/realms/company"
client_id     = "chat"                 # public client
server_client = "chat-server"          # confidential client (service account)

[oidc.roles]                           # TeamCraft role = Keycloak client role
owner  = "owner"
admin  = "admin"
member = "member"
guest  = "external"
```

- A user with several roles gets the highest one (owner > admin > member > guest); admin and owner
  include everything a member can do.
- Users of the realm **without** any of these roles can't use TeamCraft – the server rejects their tokens,
  so the rest of the realm (e.g. customers of other apps using the same realm) stays out.
- Mappers on `teamcraft`: client roles → `resource_access.teamcraft.roles`, group membership →
  `groups` (optional: Keycloak groups like `/project-a` → automatic membership in space "Project A"), user
  attribute `tc_identity_key` → ID token claim (e2ee.md §3.1).
- User profile attribute `tc_identity_key`: editable by the user (written by the client through the
  account API at enrollment), viewable by admins, not editable by the TeamCraft service account.
- **Deprovisioning:** Keycloak calls `teamcraft-server` via OIDC back-channel logout when sessions end
  (a logout token with `sid` ends that device's session only, so the user's other devices stay signed in);
  the service account additionally polls for disabled users and role changes (every few minutes) and
  triggers removal from all groups (docs/admin.md §3.2).
- **Roles and the admin roster:** Keycloak roles decide who *should* be admin; the cryptographic admin
  roster (docs/admin.md §2.3) follows automatically – an owner's client signs roster changes that match
  the Keycloak-signed role claims. A compromised VPS can't promote anyone without a matching
  Keycloak-signed token.

### 5.2 Permissions

- Instance roles: `owner`, `admin`, `member`, `guest`.
- Space roles: `space_admin`, `member`, `guest`; channel permissions inherit from the space.
- Server admins manage all rooms without reading them: signed admin actions, MLS external proposals,
  admin roster rooted in the owner's identity key ([docs/admin.md](docs/admin.md)).
- Permission schemes at instance, space and channel scope (Mattermost model): a channel can only
  restrict permissions, never grant new ones. Permission list: PLAN.md §13.
- Guests:
  - see only conversations they are members of
  - directory search returns only people they share a conversation with
  - cannot create channels; start no DM or group DM on their own by default (policy `guests.dm_start`: `nobody`, `shared`, `anyone`); members, admins and owners can always message them, and guests can answer
  - cannot invite others
- The server enforces visibility; MLS enforces it cryptographically as well, because a guest's devices
  are only ever in the groups of their conversations.

### 5.3 Presence

- Each device reports `active` / `idle` over the gateway (desktop and web also report *attention*:
  focused, recent input, not idle, unlocked; on change and every 30 s, e2ee.md §7); the server
  aggregates per user
  (online > away > offline) and merges manual states (DND, away, appear offline, in a call, in a
  meeting). An automatic away (idle device, workspace away timeout) ends with the next activity; a
  manually chosen away stays until the user picks another status (docs/gateway.md, "Presence").
- "In a meeting" comes from CalCraft free/busy; "in a call" from the SFU roster.
- Presence also drives push routing (no phone push while the desktop is active).
- **Last seen**: the server stores when a user's devices last stopped being active (`users.last_seen_at`)
  and returns it in `GET /presence` only while the user is offline, never for somebody who appears
  offline or chose `nobody` (`PUT /me/privacy`), and never when the signed policy has
  `presence.is_last_seen_enabled = false`. It is one timestamp of metadata the server has anyway.

## 6. Calls and meetings

### 6.1 Transport: no WebRTC

The same approach Fluxer announced for its new voice system (see https://fluxer.app/blog/roadmap-2026) and Discord's native clients use:

- **QUIC via WebTransport (HTTP/3)**, crate `wtransport` (built on `quinn`). Native clients use the same protocol, so the SFU has a single endpoint.
- **RTP packets inside QUIC unreliable datagrams.** QUIC handles the transport encryption and connection setup (no ICE/DTLS/SDP).
- Signaling (join, leave, subscribe, layer selection) on a reliable QUIC stream of the same session.
- Browser: WebTransport + **WebCodecs** for encoding/decoding; `getUserMedia` for capture (gives the browser's echo cancellation).
  The calls (call state, ringtone, engine) are a wasm module of their own (`teamcraft-web-calls`)
  that the page imports in the background after sign-in, so it isn't part of the app's first
  download; the page keeps the client core and the device key, forwards the gateway's call events,
  and signs and verifies the call key bindings for it (`teamcraft_client_api::page_calls`). Calls run in the
  tab that runs the client core (e2ee.md §9.4); background effects are desktop-only for now.
- Desktop: `cpal` for audio I/O, `opus`, **`webrtc-audio-processing`** (AEC, noise suppression, AGC) – the only WebRTC component used.
  The media runs in the shell; for each tile on screen the shell sends the decrypted track to the
  webview over a Tauri channel (`call_watch_video`: H.264 that WebCodecs decodes, or JPEG images
  where the webview can't decode H.264), and the own camera as sent (with effects) as small JPEG
  images (`call_watch_camera`). The UI draws both with the same `LiveVideo` as the browser.
  Before a join, the pre-join screen's check opens the camera and microphone on the device only:
  the camera through the same effects into the self view, the microphone into a level meter
  (`call_watch_level`; in the browser the call module's `preview_level`). Nothing is encoded or
  sent; leaving or hiding the screen releases both, and the join takes them over.
- Browser support (MDN browser-compat-data, checked 2026-10): WebTransport incl. datagrams in Chrome 97,
  Firefox 114, **Safari / iOS Safari 26.4**; WebCodecs `VideoEncoder`/`VideoDecoder` in Chrome 94,
  Firefox 130, Safari 16.4; `AudioEncoder`/`AudioDecoder` in Safari 26. No WebRTC fallback planned;
  networks that block UDP: §6.6.
- `MediaStreamTrackProcessor` (raw frames from the camera/mic track) is missing in Firefox: fall back to
  an `AudioWorklet` for audio and `<video>` → `VideoFrame` for video there. Codec availability (e.g.
  Opus in `AudioEncoder`) is checked at runtime with `isConfigSupported()`; fallback is a WASM Opus
  encoder.

### 6.2 E2EE: DAVE

- Discord's **DAVE** protocol (MLS-based group key agreement + per-frame encryption, reviewed by Trail of Bits).
- Rust implementation: crate **`davey`** (built on `openmls`, so it shares the MLS stack with chat). Reference: Discord's `libdave` (C++, MIT).
- Same code runs natively and compiled to WASM in the browser.
- No unencrypted fallback. Verification codes per call and per participant.
- The SFU sees RTP headers and an unencrypted audio-level header extension, never media payload.

### 6.3 SFU

- Forwards encrypted frames, never decodes.
- **Audio:** forward the 3–5 loudest speakers based on the audio-level header extension; in large calls nobody receives dozens of audio streams.
- **Video:** simulcast (3 layers) from each sender; the SFU picks the layer per receiver from its bandwidth estimate and viewport size ("last-N" for gallery view).
- **Keyframes:** the SFU asks the publisher for one when a receiver switches layers, (re)subscribes,
  or sends `request_keyframe` because it can't decode until the next one (a lost fragment, a frame
  that didn't decrypt, a failed decoder). Requests are rate-limited per receiver and track (1 s) and
  per track and layer (500 ms, `KeyframeRequestConfig`). A sender whose capture delivers nothing
  (a still screen) answers a keyframe request with its last picture after 150 ms, and screen
  shares send their last picture again about once a second while the screen doesn't change
  (`VideoSendConfig::maybe_idle_refresh`).
- **Layers that are sent (#921):** a publisher's uplink budget pauses its top layers; it tells the
  SFU which layers it carries (`uplink_layers`), and the SFU allocates no higher one. A layer that
  stops arriving while a lower one keeps coming (an older publisher, a failing encoder) is
  replaced for its receivers by the best lower layer that flows (`layer_stalled_after`, 1.5 s),
  until it is back. A publisher back from a lost connection hears `layers_needed` again, and
  sends every layer its new uplink carries until then. A viewer's tile whose video got no new
  picture for 3 s says so ("Video paused: weak connection").
- **Screen share:** separate session/stream, so a heavy share cannot degrade the call.
- **Screen share quality** (`teamcraft_media::video::screen_layers`): two content modes, chosen per
  share (remembered): *detail* (default) keeps text sharp, up to 1440p at 30 fps; *motion* keeps
  smooth video, up to 1080p at 60 fps. The top layer's bitrate follows the pixel rate (about 0.06
  bit per pixel, 2 to 8 Mbit/s: 1080p30 about 4, 1440p30 about 6.6); a second layer at half size and
  a quarter of the bitrate serves weak viewers, and the uplink budget drops the big layer first.
  A smaller source is never enlarged. Hardware encoders and the screen-content tuning of the encoders
  apply as for cameras. Not yet: 4K, AV1/VP9 screen-content tools, battery and CPU budgets per
  mode (to be measured, docs/performance.md).
- **Screen share sound** (opt-in): the computer's playback without the call itself, as a second
  Opus stream per device (music mode, 64 kbit/s). The SFU forwards it to everyone regardless of
  the speaker selection. It is encrypted with the device's DAVE audio key, like the microphone.
  Capture: PipeWire on Linux (other applications' playback streams linked into our capture,
  ours and those of processes we started, like the WebView's helpers, left out), WASAPI process
  loopback excluding our process tree on Windows, and ScreenCaptureKit with
  `excludesCurrentProcessAudio` on macOS. The desktop WebView may play no media at all
  (`media-src 'none'`), so its helper processes have nothing of the call to play. The Flatpak
  needs the PipeWire socket for this; apps/desktop/packaging/linux/README.md explains what that
  grants. "Mute all" by a meeting host also stops everyone's shared sound, at the SFU. The sender's echo canceller also
  gets the shared sound as far-end signal, so the microphone doesn't send it a second time.
- **Video effects** (background blur, background images, light correction) run on the sending device
  before encoding – the SFU only ever sees encrypted frames. Pipeline per platform: [PLAN.md](PLAN.md) §8.
- Bandwidth estimation: transport-wide feedback over the signaling stream + QUIC congestion signals.
- Codecs: Opus (audio, 48 kHz, DTX, in-band FEC planned for the measured uplink loss, §6.5); video AV1 or VP9 where hardware supports it, H.264 as fallback.
- The server tells the SFU directly who may join which call (roster + short-lived join tokens); clients never decide that.

### 6.4 Meetings with CalCraft

1. User schedules a meeting in TeamCraft (or in a CalDAV client).
2. `teamcraft-server` creates the event in the organizer's CalCraft calendar via CalDAV, with the join link in `LOCATION`/`URL`/description.
3. Invitations go out by e-mail as iMIP (RFC 6047): `text/calendar; method=REQUEST` + `.ics` attachment + join link. SMTP via `lettre`.
4. External invitees get a signed join token in their link → browser client → lobby → host admits → joins the DAVE group as "unverified guest".
5. RSVPs (`method=REPLY`) are processed by CalCraft / an inbox handler and shown in TeamCraft.

The meeting title, time and invitee list are **not** E2EE (they live in CalDAV and e-mail). Meeting chat is.

### 6.5 Mobile networks and lost connections

A call must survive a phone moving from Wi-Fi to LTE, a laptop changing networks and the
short outages of a train ride. Two layers do that; the call only ends when both fail.

**1. Connection migration (native clients).** The client's UDP socket is bound to the wildcard
address, so after a network change its packets leave through the new route; quinn clients don't
pin a source address. On a reported network change (below) the client also moves the connection
to a fresh wildcard socket of the same family (`Endpoint::rebind`; IPv6 sockets are dual-stack):
quinn switches to a new connection ID and probes at once, which covers sockets the old network
took with it (an OS that invalidates them with their interface, a VPN coming up). `wtransport`
0.7.2 has no client rebind, so it is vendored with one (`crates/vendor/wtransport/VENDORED.md`).
The SFU allows migration (`allow_migration`) and validates the new path (QUIC `PATH_CHALLENGE`),
then sends there. The QUIC connection, the DAVE group, the RTP numbering and the jitter buffers
carry on; the call loses what was in flight during the switch. When the new network can't reach
the SFU at all from this connection, layer 2 takes over.

**Noticing a dead path.** Clients and the SFU send QUIC keep-alives every 2 s (mobile carriers drop
idle UDP NAT mappings after about 30 s, some after 20 s), with a 10 s idle timeout. The native
client watches the connection's received packet count (keep-alive answers count, so quiet calls
are covered): nothing for 5 s, or nothing within 2 s after a network change, means the path is
dead. Network changes come from the same signal the gateway reconnects on (the core's
`NetworkChanges`, one listener per platform): Android's default-network `NetworkCallback`, iOS's
`NWPathMonitor`, the desktop's resume from suspend and the webview's `online`/visibility events.
The desktop driver also looks up the local address the system would use toward the SFU once a
second, which catches a network switch without a wake-up.

**2. Rejoin.** The client opens a new connection with a fresh join token from `teamcraft-server`
and joins again. The SFU tells a lost connection (idle timeout, reset) from a deliberate close (the
client leaving, a host control) and keeps a lost participant's slot – audio stream id, published
tracks, subscriptions – for a grace period (default 30 s, `TEAMCRAFT_SFU_RECONNECT_GRACE_SECS`,
announced to clients in `joined`). Meanwhile the others see `participant_reconnecting`, not "left",
and the server's roster (and ringing) doesn't change. A join of the same device takes the slot back
(`participant_reconnected`), also over a connection the SFU still thinks alive, which is told
`connection_replaced` and closed. Each connection has its own session id, so the cleanup of an old
connection never touches the slot a newer one took over. DAVE re-keys: the group removes the lost
device at once and adds it again with a new key package (e2ee.md §8). The client keeps its RTP
numbering, own tracks, subscriptions and mute state, and reconciles everything else with the new
`joined`. After the grace period the slot is released as a normal leave and the client ends the
call ("connection lost"). Refused joins, removal or end by the host and `connection_replaced` are
final: they are never answered with a rejoin. A locked meeting still lets a device of its roster
back in.

**Several devices of one account.** A call's participants are devices, not users: the SFU's slots,
the server's roster and the DAVE group (one leaf per device, e2ee.md §8) are keyed by device, so a
phone can join while the laptop is in the call and leaving on one keeps the other in. Each device
has its own key package and call key binding, so no media key is shared between devices. A device
doesn't play the microphone of another device of its own account (the user is in the room with
it; playing it would echo), and a second device joins muted by default ("Join here too"). "Move
call here" joins and then sends `move_here`: the SFU closes the account's *other* devices in that
call with `connection_replaced`, which they treat as a final leave; it never touches another
user's device. The server's ringing already stops for everybody once any device of a callee joins.

**Mute indicator.** A device reports its microphone state with `set_muted` (after joining if it
starts muted, on every change, and again after a rejoin whenever it could differ); the SFU keeps it
per device and tells the others with `participant_muted`, and sends one for each muted device right
after `joined`. It is metadata for the tiles only: the SFU forwards audio exactly as before (a muted
client sends none). A person shows as muted when all their devices in the call are. Older clients
ignore the new messages.

Rejoin attempts back off exponentially (250 ms doubling to 2 s) with jitter (each pause between
half and all of its interval), so clients that lost the SFU together don't come back in lockstep.

**SFU restarts.** Rooms live in the SFU's memory. On SIGTERM (a restart, a rolling deploy) the SFU
ends every WebTransport session with a `CLOSE_WEBTRANSPORT_SESSION` capsule carrying
`SFU_GOING_AWAY_CLOSE_CODE` (what a browser reads as `closeCode`), then closes the QUIC connection
with the same application code, and exits after a
short drain (`TEAMCRAFT_SFU_SHUTDOWN_DRAIN_SECS`, default 2 s). Unlike its other closes, clients
answer this one with a rejoin, the first attempt after a random part of a second, then with the
back-off above within the grace period they learnt. The restarted SFU (at the same address)
creates the room again on the first join, and DAVE founds a new group as the others join. While
going away the SFU reports no leaves, so `teamcraft-server` keeps the call, its roster and its
ringing state; the new SFU's join reports change nothing there. A crash sends nothing: clients
notice the dead path after the stall timeout (5 s) and rejoin the same way. Host controls kept in
the SFU (locks, removals) start fresh; the server's meeting admission still applies to every join
ticket.

**Roster reconciliation** (#684). Join and leave reports alone can leave a device in the server's
roster for good: a report given up, a leave never sent while going away by a client that didn't
come back. So the SFU also reports its whole roster (`POST /api/internal/v1/calls/roster`) at
start, every `TEAMCRAFT_SFU_ROSTER_INTERVAL_SECS` (30 s) and right after a call moved between a
user's devices, in the same ordered queue as the join and leave reports. The server drops the
devices it lacks – at once, or, while the SFU has run for less than `[calls] roster_grace_secs`
(60 s, so it is fresh from a restart and its devices are rejoining), once they were missing that
long – ends calls left with nobody, and takes devices it never heard join as joins. Once an SFU has
sent a roster, none for `roster_timeout_secs` (120 s) ends every call. A restart of
`teamcraft-server` loses its in-memory calls but not the calls themselves: join tokens carry the
conversation id (signed), the SFU names it per call in its roster, and the server takes over the
calls it doesn't know – at start it reads the roster from the SFU (`GET
{sfu_control_url}/internal/v1/calls/roster`) before it serves, otherwise from the next pushed
roster. Nothing rings again, and the history entry written at the call's start is the one closed at
its end. Calls that ended here within `roster_grace_secs` are not taken over again. A failed read
at start is retried in the background (1, 2, 4, 8, 15, 30 s, until a roster came in), and the
first join token of a conversation without a call asks the SFU first (at most 1 s). Should a
token still name a new call id for a conversation the SFU runs a call for, the SFU puts the
device into the running room (one conversation, one room); its `joined` event names the call id
it is in, and the reports tell the server. A user can also clear their
own devices from a call at once ("Leave on all devices", `POST
/api/v1/conversations/{id}/call/leave`): the SFU closes them with `connection_replaced`, so they
don't rejoin, and the server drops them whatever the SFU answered.

**Browsers** can't migrate a WebTransport session to another network; when it closes, the
browser driver rejoins as above, spreading the first attempt when the session's close code
(`closed`'s `closeCode`) is `SFU_GOING_AWAY_CLOSE_CODE`. Where the browser reports the session's
received packets (`getStats()`), the SFU's keep-alives (every 2 s) prove the path alive. The page
can't see those keep-alives itself, and Safari's counts stay zero, so in every browser the
datagrams and signaling frames the page reads count too, and a session nothing arrived on for a
second sends `ping`, which the SFU answers with `pong` (outside the event numbering; older SFUs
ignore it, and a session whose SFU never answered is watched by counts only, so a quiet call isn't
taken for dead). Media alone never decides: a muted microphone, a camera that is off or a still
screen share are quiet, the answers keep them alive. Silence for the stall timeout, or no fresh
proof (a count after the baseline, an answer to a ping sent after the change) for 3 s after the
page reported a network change (the core's `online` / `visibilitychange` / `pageshow` signal),
counts as lost without waiting for QUIC's idle timeout; silence the page slept through (a timer
gap of 2 s or more) doesn't count. A network change while rejoining makes the next attempt due at
once. Closing or reloading the
tab closes the sessions in the `pagehide` handler: a leave, not a lost connection.

Every rejoin connects with what its own fresh join ticket names: the SFU's URL and, for a
self-signed certificate, its SHA-256s (`[calls] sfu_certificate_sha256`), so an SFU that restarts
with a new certificate is reached again once the server names it.

**Status sounds.** The user hears the state, like in phone calls: while this device rejoins, a soft
falling two-tone "du-dun" repeats every two seconds; a short rising two-tone says it is back, three
falling tones that the call was lost; a quieter "du-dun" plays once when someone else drops into
"reconnecting". The tones are synthesized in `teamcraft-media` (`call_cues`, no sound file, so the
same sound everywhere and nothing to license), peak at about −18 dBFS, below speech, and are mixed
into the call's own playback after the jitter buffers: they are never sent, they play even when
muted or deafened, and they take the call's audio route on every platform (Android: the call's
output stream and Telecom route – earpiece, speaker, Bluetooth; iOS: the active `playAndRecord`
session; desktop: the selected speakers; browsers: the call's AudioWorklet). The echo canceller gets
them as far-end signal, so the microphone doesn't pick them up. A device setting chooses all
sounds, only the own connection's, or none (desktop and web: Settings → Calls; Android and iOS:
Settings → Chats).

**Media on bad links.**

- Audio first: own video layers follow the uplink estimate (screen shares before cameras), keeping
  48 kbit/s for the microphone (more while the screen's sound is shared); a layer comes back with a
  15 % margin. The start estimate is a guess, so limits apply only after the estimator cut the rate.
  A new own track (a camera turned on mid-call) starts the same way: the uplink is measured afresh
  from at least the start estimate, as an estimate made while only sound went out says nothing
  about video (#935: one lossy moment had cut it below every camera layer, and nothing was sent).
  A track whose layers are all held back still proves its encoder with one unsent picture, so the
  start check doesn't take held-back pictures for a failing encoder.
- Estimates start fast: from the start value (and after measuring afresh) they triple per second
  until the first overuse or loss, then grow 8 %/s. The SFU measures a receiver's downlink afresh
  when it subscribes to a new track, so a viewer of a share started after a call of sound only gets
  the sharp layer within about five seconds instead of minutes (#941). A still screen sends little
  but keyframe bursts: while a sender sends less than half its estimate, the queue of its own
  burst doesn't end the fast start, and a screen share keeps its layers through a dip shorter
  than three seconds, at the SFU and on the presenter's uplink.
- Cameras on limited and changing links (#939): the ladder and its rules below, the same estimator
  on the uplink (client) and on each viewer's downlink (SFU).

  | Camera layer | Desktop, browser (720p30, 1.5 Mbit/s) | Phones (2 layers, 800 kbit/s) |
  |--------------|---------------------------------------|-------------------------------|
  | 0            | 320×180, 94 kbit/s                    | half size, 200 kbit/s         |
  | 1            | 640×360, 375 kbit/s                   | full size, 800 kbit/s         |
  | 2            | 1280×720, 1.5 Mbit/s                  | –                             |

  Bitrates are the encoders' targets; a busy camera picture comes out at about 80 % of them, and
  every layer keeps the camera's frame rate (resolution goes before frame rate).

  *Estimator* (`teamcraft_media::bwe`): a queue building up (delay trend) cuts the estimate a
  step, to 85 % of itself; a queue that lasts 500 ms (the window the delivered rate is measured
  over) cuts it to 85 % of what got through, as does loss above 10 % (to what got through less
  half the loss; only loss of packets sent after the last such cut counts); loss of 2–10 % holds
  it. GCC cuts to what got through at once, but a camera doesn't fill its estimate (layers in
  steps, keyframes): the queue of one keyframe cut it below the middle layer. Growth is 8 %/s,
  and 50 %/s once the path showed no congestion for 2 s: simulcast grows in steps, so the next
  step is the probe. A probe that meets congestion doubles that wait (up to 32 s); once 10 % more
  than at the last congestion gets through, it is 2 s again. Before, a camera cut to its middle
  layer by a 500 kbit/s limit needed about 20 s back to 720p once the limit was gone.

  *Sender* (`uplink_layers`, cameras the call encodes itself: desktop and phones): after 48 kbit/s
  for the microphone, the top sent layer may be encoded at 50, 70 or 100 % of its bitrate; lower
  layers keep theirs (other viewers on weak links take them). A layer stops only when even half
  its bitrate doesn't fit, and the next lower one becomes the top one, at once. A lower share
  waits 1 s, a higher one 2 s (each change rebuilds the layer's encoder, which starts with a
  keyframe), and the way back up goes one step at a time, with a 15 % margin until the estimate
  covers everything. Before, the top layer stopped as soon as its whole bitrate didn't fit: a
  500 kbit/s uplink carried 320×180 at about 80 kbit/s, a 2 Mbit/s one 640×360 at 300 kbit/s.
  Browsers' cameras and screen shares send a layer whole or not at all, as before.

  *SFU* (`layers`): per viewer, its downlink estimate less 120 kbit/s for sound is the budget;
  the layer is the highest whose bitrate fits, at most the one the tile asks for (#927). Down at
  once, up one layer at a time once the budget covered it with 15 % headroom for 2 s; a paused
  camera is tried again at its lowest layer every 5 s. Other viewers of the same camera are not
  affected.

  Measured (`bandwidth_limits` in the SFU's test suite: real SFU, a 720p camera with sensor noise,
  a bottleneck with a 250 ms queue; OpenH264, laptop, release build):

  | Scenario                              | Before (3 runs)                           | After (6 runs)                     |
  |---------------------------------------|-------------------------------------------|------------------------------------|
  | Uplink 500 kbit/s                     | 320×180, ~75 kbit/s, 33.8 dB              | 640×360, 200–350 kbit/s, 35.8 dB   |
  | Uplink 2 Mbit/s, after 500 kbit/s     | 320×180 or 640×360, at most 300 kbit/s    | 720p 2–9 s after the step          |
  | Uplink limit lifted                   | 720p 6 s later, or not within 10 s        | 720p already                       |
  | Uplink drops to 500 kbit/s            | longest stall 0.2–0.3 s                   | longest stall 0.3–0.9 s            |
  | Viewer's downlink drops to 500 kbit/s | longest stall 2.2–3.4 s, then 320×180     | 0.6–1.0 s, then 320×180 or 640×360 |
  | Viewer's downlink limit lifted        | 640×360 after 1–2 s, 720p not within 14 s | 720p after 2–9 s                   |
  | The other viewer                      | 720p throughout                           | 720p throughout                    |

  (dB: luma PSNR of the picture as a large tile shows it, a lower layer scaled up to 720p.) A
  sudden drop stalls a little longer than before: the top layer keeps its share for the hold
  until the estimator's cut to what got through. While a limit stays, the next layer is tried
  now and then (the probe: a few seconds apart at first, then about every 15 s), which can hold
  the picture up for up to a second. Not yet: the SFU's QUIC send buffer keeps up to
  1 MiB of datagrams, so after a sudden drop it still delivers the old layer's queue before the
  lower one; browsers' cameras don't adapt their bitrate (WebCodecs would need a reconfigured
  encoder per change); the shares aren't told to the SFU, which allocates by the layers' full
  bitrates.
- Large frames: a frame of more than 255 fragments (a 1440p keyframe of text is about 450 KB)
  carries the fragment extension's long form (16-bit index and count, up to 4096 fragments); the
  short form stays for every other frame. Before, such keyframes could not be sent, and the top
  layer of a text-heavy share never arrived (#941).
- Screen content in OpenH264 uses timestamp-based rate control planned for 5 pictures a second: a
  screen changes a few times a second, and planned for 30, keyframes of text starved the pictures
  after them.
- Opus in-band FEC is planned for the measured uplink loss (5 % floor, 30 % cap; the native encoder
  and WebCodecs' `packetlossperc`). Native receivers recover a lost packet from the next packet's FEC
  data; WebCodecs can't, browsers conceal.
- Jitter buffer: an outage drains it and it re-buffers to its target depth; a backlog that arrives
  in a burst after a handover is skipped down to the target once it persists for half a second,
  instead of adding its length as latency for the rest of the call.

| Platform | Network change                             | Migration | Rejoin            | UI while rejoining               |
|----------|--------------------------------------------|-----------|-------------------|----------------------------------|
| Android  | core signal (`NetworkCallback`)            | yes       | yes, engine token | "Connection lost, reconnecting…" |
| iOS      | core signal (`NWPathMonitor`)              | yes       | yes, engine token | "Connection lost, reconnecting…" |
| Desktop  | core signal (wake, webview) + route lookup | yes       | yes, core token   | header status, tiles dimmed      |
| Browser  | core signal (`online`, visibility) + stats | no        | yes, page's token | header status, tiles dimmed      |

Tests: deterministic ones drive the SFU's rooms and real call engines in memory (lost connection,
grace expiry, takeover, a lossy uplink) and the native client over loopback through a UDP relay
that changes its address (migration) or drops everything (rejoin). Real radios:
[docs/manual-tests/call-handover.md](docs/manual-tests/call-handover.md).

### 6.6 Networks that block UDP

Calls need UDP to the SFU's QUIC port (default 3478: the STUN/TURN port Teams, Zoom and WebRTC
services use, so restrictive networks often allow it already; 443/udp is taken by the proxy's
HTTP/3). Some corporate, hotel and guest networks block UDP or everything but 80/443 over TCP; there
the WebTransport handshake fails and joining a call fails after the connection timeout. Chat is
unaffected (HTTPS and WebSocket over TCP). There is no TCP fallback today.

Options for a fallback, by fit with §6.1:

- **WebTransport over HTTP/2** (IETF draft): the same API over TCP/TLS. Browser support is the
  question – check the current state before relying on it; `wtransport` doesn't implement it.
- **A TCP/TLS relay on 443 next to the SFU**: the client carries the signaling stream and the media
  datagrams (length-prefixed frames) over one TLS connection – WebSocket in browsers, plain TLS in
  native clients – and the relay hands them to the SFU as if they came over QUIC. Works everywhere
  HTTPS works; costs head-of-line blocking on loss, so the relay drops media frames that wait too
  long instead of queuing them. The SFU, DAVE and the engine stay as they are.
- **TURN over TCP/TLS**: needs ICE, i.e. WebRTC – ruled out by §6.1.

Recommendation: the TLS/WebSocket relay, tried by the client after QUIC fails or doesn't connect
within a few seconds (happy-eyeballs style), with the relay's port behind the same reverse proxy as
the server. Until then, operators document that calls need outbound UDP 3478 (docs/operations.md).

## 7. Clients

| Platform              | Shell                                           | Notes                                                                                                                                                                                                                                                         |
|-----------------------|-------------------------------------------------|---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| Windows, macOS, Linux | Tauri 2 + Leptos                                | Media engine and crypto in the Rust core, not the webview (WebKitGTK media on Linux is unreliable). Windows runs on WebView2 with Credential Manager, Media Foundation / Windows Graphics Capture and WASAPI (docs: apps/desktop/packaging/windows/README.md) |
| Web                   | Leptos → WASM, served by `teamcraft-server`     | Members and guests; WebTransport + WebCodecs                                                                                                                                                                                                                  |
| Android               | Kotlin (Jetpack Compose) + Rust core via UniFFI | FCM or UnifiedPush; Jetpack Telecom for calls → Android Auto calling + messaging                                                                                                                                                                              |
| iOS                   | Swift (SwiftUI) + Rust core via UniFFI          | Notification Service Extension, CallKit (→ CarPlay), shared App Group keychain                                                                                                                                                                                |

All logic (sync, crypto, storage, search, media) lives in the shared Rust core. Mobile uses native shells
instead of Tauri mobile because the Notification Service Extension, CallKit and Android's Telecom /
Car App integration need native targets.

## 8. Repository layout

Monorepo. Deployment configuration lives in a separate private `teamcraft-deploy` repository.
Crates that exist today: `proto`, `crypto`, `media`, `core`, `server`, `sfu`. The others are added
when their milestone starts.

| Path                      | Kind   | WASM | Purpose                                                                                                                                       |
|---------------------------|--------|------|-----------------------------------------------------------------------------------------------------------------------------------------------|
| `crates/teamcraft-proto`  | lib    | yes  | API, gateway and SFU signaling types (serde)                                                                                                  |
| `crates/teamcraft-crypto` | lib    | yes  | MLS (openmls), identity/device keys, content keys, key bag, rendezvous, attachment crypto                                                     |
| `crates/teamcraft-media`  | lib    | yes  | RTP packetization, jitter buffer, DAVE (davey), simulcast layer logic                                                                         |
| `crates/teamcraft-db`     | lib    | no   | Postgres access (sqlx, compile-time checked queries), migrations                                                                              |
| `crates/teamcraft-ops`    | lib    | no   | Helpers of the server binaries: container health probe (`healthcheck` subcommand), graceful shutdown signal                                   |
| `crates/teamcraft-core`   | lib    | yes  | Client state, sync, local store (SQLCipher), search (tantivy + shared encrypted segments), notification rules                                 |
| `crates/teamcraft-ffi`    | lib    | no   | UniFFI bindings of `teamcraft-core` for Swift and Kotlin                                                                                      |
| `crates/teamcraft-server` | bin    | no   | REST API, gateway, auth, permissions, presence, MLS delivery, push, CalCraft/SMTP                                                             |
| `crates/teamcraft-sfu`    | bin    | no   | WebTransport SFU                                                                                                                              |
| `crates/teamcraft-vault`  | bin    | no   | Optional trusted vault node: compliance archive, file text extraction                                                                         |
| `crates/teamcraft-bridge` | bin    | no   | Integration bridge: local webhooks/log ingestion → E2EE messages, builds index segments                                                       |
| `crates/teamcraft-pbx`    | bin    | no   | Telephony + AI hotline: provider-agnostic SIP trunks, voice agent adapters, queues, DAVE participant ([docs/telephony.md](docs/telephony.md)) |
| `crates/teamcraft-ui`     | lib    | yes  | Leptos UI (desktop + web)                                                                                                                     |
| `apps/desktop`            | Tauri  | no   | macOS + Linux shell, native audio                                                                                                             |
| `apps/ios`                | Xcode  | no   | App + Notification Service Extension                                                                                                          |
| `apps/android`            | Gradle | no   | App                                                                                                                                           |

## 9. Milestones

See [PLAN.md](PLAN.md#roadmap).

## 10. Decisions and open questions

Decided:

- Business chat in the Mattermost / Slack / Teams mould; no federation
- License AGPL-3.0-or-later (as CalCraft and Fluxer); reuse of AGPL code (e.g. Threema's
  AGPL-3.0-only rendezvous protocol) is allowed
- S3-compatible storage for blobs; Postgres for metadata and ciphertext
- Desktop for macOS + Linux only; iOS and Android are part of v1
- Search via encrypted shared index segments (e2ee.md §10)
- OIDC provider on a separate server attests identity keys (e2ee.md §3.1)
- Telephony directly on SIP trunks of any provider; no Asterisk, no Twilio
- Monorepo + separate private deploy repository
- Browser calls without WebRTC: WebTransport + WebCodecs are available in all current browsers incl.
  Safari 26.4 (§6.1)

Open:

- Further E2EE questions: [docs/e2ee.md](docs/e2ee.md) §14
