# Calls and the SFU

Calls run through `teamcraft-sfu`, a media server that forwards end-to-end encrypted (DAVE) RTP
packets over QUIC datagrams (WebTransport). It cannot decrypt media.

## How it fits together

1. The client asks the server for a **join token** (`POST /conversations/{id}/call/join-token`).
2. The server signs the token (Ed25519) and returns the SFU URL.
3. The client connects to the SFU, which verifies the token with the public key.
4. The SFU reports joins and leaves to the server (`/api/internal/v1/calls/participants/...`, bearer
   token), which keeps the roster and sends `call_*` events and rings DMs.

## Configure the server

```sh
openssl genpkey -algorithm ed25519 -out call-signing.pem
openssl pkey -in call-signing.pem -pubout -out call-signing.pub.pem     # for the SFU
```

```toml
[calls]
sfu_url = "https://sfu.example.com:4433/call"
issuer = "https://chat.example.com"
signing_key_path = "/etc/teamcraft/call-signing.pem"
# sfu_control_url = "http://sfu.internal:8081"   # host controls; the registration token authenticates
# join_token_ttl_secs = 60, call_idle_secs = 43200, ring_timeout_secs = 30
# sfu_registration_token via TEAMCRAFT_CALLS__SFU_REGISTRATION_TOKEN
```

Without `[calls]` the join-token endpoint answers 503 and clients cannot call.

## Configure the SFU (environment only)

| Variable                              | Meaning                                                            |
|---------------------------------------|--------------------------------------------------------------------|
| `TEAMCRAFT_SFU_BIND`                  | UDP address, default `[::]:4433`                                   |
| `TEAMCRAFT_SFU_JOIN_TOKEN_PUBLIC_KEY` | PEM of the server's public key                                     |
| `TEAMCRAFT_SFU_JOIN_TOKEN_ISSUER`     | Equals the server's `[calls] issuer`; required with the public key |
| `TEAMCRAFT_SFU_CERT` / `_KEY`         | TLS certificate and key (PEM) for the public name in `sfu_url`     |
| `TEAMCRAFT_SFU_SERVER_URL`            | Base URL of the server, for participant reports                    |
| `TEAMCRAFT_SFU_REGISTRATION_TOKEN`    | Equals the server's `sfu_registration_token`                       |
| `TEAMCRAFT_SFU_HEALTH_BIND`           | Health endpoint, default `[::]:8081` (orchestrator only)           |

Without a certificate the SFU makes a short-lived self-signed one: only for tests. Do **not** set
`TEAMCRAFT_SFU_DEV_HTTP` or `TEAMCRAFT_SFU_INSECURE_DEV_TOKENS` in production: unsigned dev tokens are
accepted only with `TEAMCRAFT_SFU_INSECURE_DEV_TOKENS=1`, no public key, and loopback
`TEAMCRAFT_SFU_BIND` and `TEAMCRAFT_SFU_DEV_HTTP`; otherwise all joins are rejected.

Network: open `4433/udp` from clients (IPv4 and IPv6). The SFU holds no state; scale by running more
and pointing `sfu_url` at one (single SFU per instance today).

## Calls in the browser

The web client connects to the SFU straight from the page (WebTransport), so:

- The SFU needs a certificate **browsers trust** for the host name in `sfu_url` (a public CA, or your
  company CA on managed machines). The short-lived self-signed one works for desktop and mobile
  apps in tests only; browsers refuse it.
- The page's Content-Security-Policy must allow the SFU in `connect-src`. The default allows any
  `https:` origin; a stricter custom policy has to list `sfu_url`'s origin.
- The web root must contain `teamcraft-web-calls.js`, `teamcraft-web-calls_bg.wasm` and
  `teamcraft-call-worklet.js` (they ship with the web client): the call engine loads only when a
  call starts, so it doesn't slow down the first visit.
- Browsers: current Chrome, Edge, Firefox and Safari (26.4 or later). Older ones show why they
  can't call.

## Rotating the signing key

See [docs/operations.md](../../operations.md#rotating-the-call-signing-key): new key pair, restart the
SFU then the server; joins between the restarts fail and succeed on retry.

## Status

Desktop, Android and the web client make real calls (audio, H.264 camera with simulcast, screen
share). Background effects are desktop-only. iOS calls, recording and captions are coming soon. Telephony (SIP trunk, AI hotline) is a design draft only
([docs/telephony.md](../../telephony.md)).
