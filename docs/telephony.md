# TeamCraft – Telephony and AI hotline

Goal: TeamCraft can run a company phone hotline itself – SIP trunk, AI voice agent, handover to staff –
and staff take escalated calls in the TeamCraft app (optionally also on their mobile phones). No
separate PBX such as Asterisk.

Status: design draft.

## 1. Scope

- Any SIP trunk provider (registration or IP-based authentication); several trunks per instance.
- Any AI voice agent provider behind a small adapter interface; the first adapter is ElevenLabs
  Conversational AI.
- Inbound calls to hotline numbers, escalation to human agents, outbound legs to mobile numbers,
  later outbound calls.
- Not in scope: desk phones or softphones registering to TeamCraft, IVR trees, multi-tenant PBX features.

Typical setups this replaces: a cloud telephony API (e.g. Twilio Media Streams) bridged to a voice AI,
or Asterisk + ARI in front of a voice AI.

## 2. Architecture

```
                    ┌──────────────────────── teamcraft-pbx ─────────────────────────┐
PSTN ─▶ SIP trunk ─▶│ SIP/RTP stack ──▶ call controller ──▶ voice agent adapter      │
        (any        │  (rsipstack,       (state machine,     (e.g. ElevenLabs:       │
         provider)  │   G.722/G.711)      queues, transfer)   WebSocket, tools)      │
                    │                         │                                      │
                    │                         ▼ escalation                           │
                    │              DAVE participant in a TeamCraft call ─────────────┼──▶ teamcraft-sfu ◀── agents' apps
                    └─────────────────────────┼──────────────────────────────────────┘
                                              ▼
                                   teamcraft-server: ring agents, #hotline thread
```

`teamcraft-pbx` is a TeamCraft service (own binary in the workspace and docker-compose; can run on the
same host as the server).

| Part                    | Purpose                                                                                                                                                             |
|-------------------------|---------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| **SIP trunks**          | Register with / accept calls from one or more providers (§3)                                                                                                        |
| **Voice agent adapter** | Streams caller audio to the AI agent and back, handles tool calls such as "transfer to staff" (§4)                                                                  |
| **Hotline config**      | Per hotline number: trunk, agent, knowledge base / context, queue, fallback numbers; editable in the admin panel, optionally synced from an external system via API |
| **Call controller**     | Queues, ringing agents in the app, hold, transfer, timeouts, announcements                                                                                          |
| **Media**               | Transcodes G.711/G.722/L16 ⇄ Opus, joins the TeamCraft call as a participant                                                                                        |
| **Notifications**       | `#hotline` threads (§6)                                                                                                                                             |

## 3. SIP trunks (provider-agnostic)

Configured per trunk, so any standards-compliant provider works:

| Setting         | Options                                                                                                                 |
|-----------------|-------------------------------------------------------------------------------------------------------------------------|
| Authentication  | registration (username/password, registrar) or IP-based (allowed source IPs)                                            |
| Transport       | UDP, TCP, TLS; SRTP optional / required                                                                                 |
| Codecs          | G.722, G.711 PCMA/PCMU, priority order                                                                                  |
| Number handling | where to read the called number (Request-URI, `To`, `P-Called-Party-ID`), E.164 normalization, number block             |
| Caller ID       | number to present on outbound legs; pass-through of the original caller if the provider allows it ("CLIP no screening") |
| DTMF            | RFC 4733 (default), SIP INFO as fallback                                                                                |
| NAT             | public address, RTP port range                                                                                          |

Protocol needs:

- `REGISTER`, inbound and outbound `INVITE`, `BYE`, `CANCEL`, re-INVITE for hold, `OPTIONS` keepalive
- SDP offer/answer, RTP/RTCP, SRTP
- Called number may arrive in different headers depending on provider and trunk setup – configurable,
  with a test tool in the admin panel that shows the headers of a test call

**Library:** [`rsipstack`](https://crates.io/crates/rsipstack) (Rust SIP stack, actively maintained,
transaction + dialog layers) as first choice; `ezk-sip-ua` + `ezk-rtp` as alternative. Media (RTP,
jitter buffer, G.722/G.711 codecs, resampling to 48 kHz Opus) lives in `teamcraft-media`, shared with
the SFU code.

**First step: spike** against the first provider in use – register, take an inbound call, play audio,
receive DTMF, place an outbound call. Then a provider compatibility checklist in the docs, extended
whenever a new provider is tested.

## 4. Voice agent adapters

Trait-based, one implementation per provider:

- `start_session(hotline, caller) -> session` with context variables (caller number, caller profile,
  knowledge base)
- bidirectional audio stream (provider-specific format, converted by `teamcraft-media`)
- events: transcript chunks, tool calls, end of conversation, provider-side summary
- tool call **`transfer_to_staff`** (optional `note` parameter for the agents). The tool name is
  configurable per agent, so existing agents keep working without renaming their tools.

First adapter: ElevenLabs Conversational AI (WebSocket). Others (OpenAI Realtime, self-hosted
pipelines) can be added behind the same trait.

## 5. Escalation in the app

1. The voice agent calls `transfer_to_staff` (optional `note`).
2. The agent's last sentence plays out (short drain timeout), the caller hears hold music or an
   announcement.
3. `teamcraft-pbx` creates a TeamCraft call and asks `teamcraft-server` to ring the queue:
   everyone who is **available for hotline** (separate toggle, auto-busy while in a call, respects DND).
4. Ringing screen on desktop, web and phone (CallKit on iOS, Telecom on Android): caller number,
   hotline/location, caller profile, AI note, transcript so far.
5. The first agent to accept is connected; others can still join. Agents can add colleagues or
   transfer to another queue.
6. **Forwarding to mobile numbers** (per queue, configurable):
   - *parallel*: mobile numbers ring together with the app from the start,
   - *fallback*: only after N seconds without an answer in the app,
   - *off*.
   Mobile legs are outbound calls through the trunk. The person answering has to press **1** before
   being connected, so a voicemail can't take the call. Mobile and app agents can be in the same call.
7. Nobody answers within the max wait (configurable): goodbye announcement, hang up, "call back" task
   in the thread.

## 6. `#hotline` channels

The `#hotline` channels are **integration channels** (ARCHITECTURE.md §4.5): E2EE, with
`teamcraft-pbx` acting as their bridge – it posts the call threads and builds their search index.

One channel per hotline or one shared channel, configurable. **One thread per call**:

1. Call start: caller, hotline/location, caller profile
2. Escalation: AI note, "Take call" button
3. Who answered, duration, transfers, missed → "Call back" button
4. Summary at the end (from the voice agent provider or a configurable LLM) + agent wrap-up note

## 7. Encryption and where it runs

- The phone network isn't end-to-end encrypted, and the SIP provider and the voice agent provider see
  call audio anyway. `teamcraft-pbx` necessarily handles plaintext audio and transcripts.
- Inside TeamCraft the call is still DAVE-encrypted: `teamcraft-pbx` is a participant with its own bot
  device key, allowed only in hotline calls and `#hotline` channels.
- Consequence: **whoever controls the host running `teamcraft-pbx` can listen to hotline calls and read
  `#hotline` channels** – nothing else. Running it on the same VPS as `teamcraft-server` is acceptable
  for that reason; the UI marks hotline calls as "phone call – not end-to-end encrypted".
- Hotline content should stay in `#hotline`; sensitive follow-ups belong in normal channels.

## 8. Features

| Feature                                                                         | Prio        |
|---------------------------------------------------------------------------------|-------------|
| SIP spike against the first provider (register, inbound, outbound, DTMF, G.722) | v1.x, first |
| Provider-agnostic SIP trunks (registration / IP auth, several trunks)           | v1.x        |
| Voice agent adapter trait + ElevenLabs adapter                                  | v1.x        |
| Hotline config per number in the admin panel, optional sync API                 | v1.x        |
| `#hotline` threads (start, escalation, summary)                                 | v1.x        |
| Queues, "available for hotline", ring all / round robin / longest idle          | v1.x        |
| Take calls in the app on all platforms, hold, transfer, add colleague           | v1.x        |
| Forwarding to mobile numbers (parallel / fallback / off) with "press 1"         | v1.x        |
| Goodbye announcement after max wait                                             | v1.x        |
| Trunk test tool (show headers / codecs of a test call)                          | v1.x        |
| Outbound calls (callback from thread, click-to-call)                            | later       |
| Additional voice agent adapters                                                 | later       |
| Queue dashboard, wrap-up categories, stats                                      | later       |
| Call recording (needs caller consent)                                           | later       |

## 9. Open questions

- Caller ID on mobile legs: hotline number or the original caller's number (provider-dependent)?
- Migration path from an existing setup: move numbers to the new trunk one hotline at a time?
