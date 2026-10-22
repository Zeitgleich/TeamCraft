# Push gateway

The iOS and Android store apps are signed and published by one party (the **publisher**). Apple and
Google tie push delivery to the app's signing identity: only the holder of the app's APNs key and
FCM project can send to its devices. A self-hosted server can't have those credentials, so without
help the store apps would never get a push from any server but the publisher's own.

The push gateway is that help: a small service that holds the publisher's APNs and FCM credentials and
forwards **sealed wake-ups** (e2ee.md §7) from any TeamCraft server to Apple and Google. It sees device
tokens and ciphertext, never users, devices, keys or content. Related: [e2ee.md §7](e2ee.md),
[notifications.md](notifications.md), [operations.md](operations.md).

## 1. Principles

- **No special role for the publisher's own server.** It is a customer of the gateway like every other
  server: same API, same limits, same signature check.
- **Direct stays possible.** Whoever builds and signs their own apps has their own APNs key and FCM
  project and configures `[push.apns]` / `[push.fcm]` on the server; the gateway is not involved.
- **Web Push and UnifiedPush never use it.** Those endpoints belong to the user's browser or
  distributor, not to the publisher.
- **Vendor-neutral protocol.** The gateway API doesn't mention APNs or FCM internals beyond a
  provider name (`apns`, `fcm`) and a notification kind; another operator can run a gateway for their
  own apps with the same binary.

## 2. Overview

```
 device ──registers token + push key──▶ teamcraft-server ──(sealed wake-up, token)──▶ push gateway ──▶ APNs / FCM ──▶ device
                                         seals the wake-up         signed request          adds the publisher's
                                         with the device's         (server key pair)       credentials, enforces limits
                                         push key
```

1. The app registers with the server as before (`PUT /api/v1/devices/{id}/push`): provider `apns` or
   `fcm`, the platform token, a fresh 256-bit push key. Nothing changes in the apps.
2. When the server wants to wake the device it seals the wake-up with the push key
   (XChaCha20-Poly1305, e2ee.md §7) and, for `apns`/`fcm` registrations without credentials of its own,
   posts it to the gateway together with the token.
3. The gateway checks the server's signature and limits, builds the APNs or FCM request with its
   credentials, and forwards it. The answer tells the server whether the token is dead.
4. The device (notification service extension, FCM service) opens the sealed wake-up with the push
   key and fetches the message from its own server.

The server picks the sender per provider: credentials under `[push.apns]` / `[push.fcm]` win; otherwise
`[push.gateway]` is used when set; otherwise the provider is not configured and registrations for it
are refused, as before.

## 3. API

JSON over HTTPS, versioned under `/api/v1/`. Types: `teamcraft-proto` (`api/push_gateway.rs`).
Error bodies are `{ "error_code": "...", "message": "...", "retry_after_secs": n }`; the codes are
stable (unknown ones must be treated as errors of that HTTP status class).

| Endpoint                   | Auth      | Purpose                                                                                      |
|----------------------------|-----------|----------------------------------------------------------------------------------------------|
| `POST /api/v1/servers`     | signature | Register a server's public key. Idempotent. Returns `server_id` and the limits               |
| `POST /api/v1/push`        | signature | Forward one sealed wake-up to one device token                                               |
| `GET /api/v1/info`         | none      | Version, providers with credentials, whether registration is open, default limits            |
| `GET /healthz`, `/metrics` | none      | On a separate listener (`server.health_bind`) for the operator's network, never the internet |

### 3.1 Server identity and signatures

Each TeamCraft server owns an **Ed25519 key pair for the gateway** (`[push.gateway] key` or `key_path`,
generated on first start when the file doesn't exist). The gateway knows a server by `server_id`, 128 bits
of a domain-separated SHA-256 of the public key (32 hex characters). The ID is an opaque handle: it
says nothing about the server's name, address or users, and the gateway never asks for any of them.

Every request carries three headers:

| Header                  | Content                                                                     |
|-------------------------|-----------------------------------------------------------------------------|
| `x-teamcraft-server`    | `server_id` (not on registration: the key is in the body)                   |
| `x-teamcraft-timestamp` | sender clock, milliseconds since the Unix epoch; accepted within ±5 minutes |
| `x-teamcraft-signature` | base64url (no padding) Ed25519 signature                                    |

The signature covers `"POST" \n path \n timestamp \n SHA-256(body)` under the label
`TeamCraft-v1 push gateway request` (`teamcraft-crypto`, `push_gateway.rs`). It binds the method,
path, time and body, so a request can't be redirected or edited. Registration is signed with the key it
registers (proof of possession), so nobody can register or impersonate somebody else's key.

Replay protection: a push carries a random 16-byte `request_id`; the gateway refuses an ID it has seen
within the timestamp window (in memory; a restart forgets them, and the window then still bounds a
replay to the few minutes before the restart).

### 3.2 Push request

```json
{
  "request_id": "<16 bytes, base64>",
  "provider": "apns",
  "kind": "message",
  "urgency": "high",
  "ttl_secs": 3600,
  "token": "<APNs device token (hex) or FCM registration token>",
  "sealed": "<sealed wake-up, base64>"
}
```

Byte fields use standard base64 like the rest of the API. `kind` (`message`, `mention`, `call`) only
selects the channel (APNs: VoIP topic for calls); `urgency` the priority; `ttl_secs` is capped
(`limits.max_ttl_secs`). The visible alert text of the APNs notification is fixed by the gateway
(`apns.alert_text`), so a server can't put its own words on a lock screen. The gateway validates sizes
and field values and rejects everything else (`provider` other than `apns`/`fcm`, unknown kinds).

### 3.3 Answers

| HTTP | `error_code`                                           | Meaning for the server                                                                                           |
|------|--------------------------------------------------------|------------------------------------------------------------------------------------------------------------------|
| 200  | –                                                      | The push service accepted the wake-up                                                                            |
| 410  | `token_gone`                                           | APNs (410, `BadDeviceToken`, `DeviceTokenNotForTopic`) or FCM (`UNREGISTERED`, 404): **forget the registration** |
| 401  | `unknown_server`                                       | Register the key, then retry once (the client does this itself)                                                  |
| 401  | `unauthorized`                                         | Bad or stale signature, missing headers; check the server's clock                                                |
| 403  | `server_blocked`, `registration_closed`                | The operator refuses this server; never delete devices because of it                                             |
| 409  | `replayed_request`                                     | Request ID seen before                                                                                           |
| 413  | `payload_too_large`                                    | Body or wake-up over the limits                                                                                  |
| 429  | `rate_limited`, `quota_exceeded`, `token_rate_limited` | Retry after `Retry-After`; the wake-up is dropped for now, nothing deleted                                       |
| 501  | `provider_not_configured`                              | The gateway has no credentials for this provider                                                                 |
| 502  | `provider_unavailable`, `provider_rejected`            | Apple/Google down, or the gateway's own credentials refused; retry later                                         |

The server maps these onto its existing push errors: only `token_gone` makes it delete a device's
push registration; everything else is a failed or retried send and shows up in the admin console's
provider health. Provider internals (Apple's `reason`, Google's status) are logged by the gateway,
not passed on.

## 4. Threat model

What a gateway **sees**: the device token (an opaque handle at Apple/Google), the opaque `server_id`, the
time and size of each wake-up and its kind, the connecting IP address of the server, and, because it
forwards, the same things Apple or Google see anyway. It does not see user, device or conversation
IDs, the server's name, push keys or any plaintext: the payload is sealed to the device's push key,
which only the device and its server know.

A **malicious or compromised gateway** can:

- **drop or delay** pushes, or refuse a server (availability; the app still catches up the next time
  it runs or reconnects, so no message is lost, only the prompt notification);
- **replay** a sealed wake-up it has seen. The device opens it, fetches the referenced message from its
  server and shows a duplicate notification at worst; wake-ups name a message but carry no
  instruction;
- **correlate** tokens with servers and traffic patterns over time (metadata).

It **cannot** read a wake-up, forge one (it has no push key; a forged ciphertext fails authentication in
the app and is discarded), learn who the users are, or influence the content of any message. Its
alert text is generic and replaced by the notification service extension after decryption.

A **malicious or compromised TeamCraft server** can use the gateway's capacity: flood it with pushes
to its own devices or to tokens it learned. That is bounded by per-server rate, burst and daily quota,
a per-token limit (a device token receives at most `pushes_per_token_per_minute` wake-ups per minute
no matter how many servers send to it), body and field limits, and the **block list** (`servers block`).
A server can't send arbitrary text (alert text is fixed) or choose topics (the gateway's bundle ID).
A server that knows a device token can wake that device; tokens are only ever handed to the server by the
device itself.

A **network attacker** needs the server's private key to send, and can't alter or redirect a signed
request. TLS protects tokens in transit (the server requires an `https://` URL unless
`push.allow_private_endpoints` is set for development).

Registration is open by default (`registration.mode = "open"`) because any self-hosted server must be
able to join without asking. Abuse is handled by limits per address and in total, the global server
cap, and the block list; operators who prefer an allow list set `mode = "closed"` and add keys with
`servers allow`.

## 5. Limits, quotas, block list

All values are settings (`[limits]`, `[registration]` in the gateway's configuration) with defaults, and
a per-server override (`servers limits`).

| Limit                               | Default  | Scope                                |
|-------------------------------------|----------|--------------------------------------|
| `pushes_per_second` / `burst`       | 50 / 200 | per server (token bucket)            |
| `pushes_per_day`                    | 200,000  | per server and UTC day               |
| `pushes_per_token_per_minute`       | 30       | per device token, across all servers |
| `max_body_bytes`                    | 16 KiB   | per request                          |
| `max_sealed_bytes`                  | 2 KiB    | per wake-up                          |
| `max_ttl_secs`                      | 7 days   | per wake-up                          |
| `timestamp_window_secs`             | 300      | per request                          |
| `registration.per_address_per_hour` | 10       | per client address (IPv6 per /64)    |
| `registration.global_per_hour`      | 600      | all addresses                        |
| `registration.max_servers`          | 100,000  | total                                |

Per-token windows are kept as a keyed 64-bit hash that only lives in memory; tokens themselves are
never stored. Block and limit changes made through the CLI take effect within
`database.server_cache_secs`.

## 6. State store: SQLite

The gateway keeps: servers (ID, public key, created and last seen, block state and reason, limit
overrides) and **daily counters** per server (accepted, token gone, failed). Everything else — rate
limit buckets, token windows, replay IDs — is in memory and rebuilt from nothing after a restart.
No tokens, wake-ups, IP addresses or payloads are stored or logged.

**SQLite, not Postgres,** because the data is a few kilobytes per server in two tables, written by one
process on one host, and the gateway should be trivial to run: a binary or container plus a volume.
Postgres would add a second service to operate (and to secure) in a place where the publisher wants
the smallest possible attack surface, for no capability the gateway uses (no cross-process
transactions, no multi-writer scaling). WAL mode lets the operator CLI read and edit the block list
while the gateway runs. The usage counters are flushed periodically (default 10 s) and on shutdown,
so a crash loses at most that interval of quota accounting. The trade-off is that a gateway is a single
instance: running several would need the block list and quotas in shared storage. A single instance
handles far more than the pushes of the expected number of servers (the bottleneck is Apple's and
Google's API latency, not the gateway); revisit when a gateway needs high availability.

The store lives in `crates/teamcraft-push-gateway` (`rusqlite`, bundled SQLite) and not in
`teamcraft-db`, which is the Postgres crate; the "SQL only in the DB crate" rule is about the server's
database. The queries are plain parameterized statements.

## 7. Operating a gateway

Only the publisher of store apps (or someone who builds their own apps for a group of servers) runs one.
The binary is `teamcraft-push-gateway`, shipped as a container image next to the server and the SFU
(`ghcr.io/zeitgleich/teamcraft-push-gateway`, signed with a build provenance attestation like the others);
see `crates/teamcraft-push-gateway/teamcraft-push-gateway.example.toml` for every setting.

- **Credentials:** `[apns]` (the `.p8` key, key ID, team ID, bundle ID) and `[fcm]` (the Firebase service
  account key), as files or `TEAMCRAFT_PUSH_GATEWAY__APNS__KEY` / `__FCM__SERVICE_ACCOUNT`. A partial setup
  stops the start. The gateway is the only place these credentials live.
- **Where to run it:** on its own host, separate from every chat server, behind a TLS-terminating
  reverse proxy (`server.client_ip_header` for the registration limit). The deployment role is
  `push_gateway` in the deployment repository, off by default.
- **CLI** (works on the database file while the gateway runs):

  ```sh
  teamcraft-push-gateway servers list
  teamcraft-push-gateway servers block <server-id> <reason...>
  teamcraft-push-gateway servers unblock <server-id>
  teamcraft-push-gateway servers limits <server-id> <per-second|default> <burst|default> <per-day|default>
  teamcraft-push-gateway servers allow <public-key-base64url>      # for registration.mode = "closed"
  ```

- **Monitoring:** `/healthz` (database reachable) and `/metrics` (Prometheus text) on `server.health_bind`:
  `teamcraft_push_gateway_pushes_total{provider,result}`, `..._refused_total{reason}`,
  `..._registrations_total`, `..._servers`. No server ID, token or payload is ever a label or a log field.
  Alert on `result="rejected"` (credentials refused) and `"unavailable"`.
- **Logging:** `tracing` at `info`: start-up, configuration problems and provider refusals with their
  status codes. No payloads, tokens or server IDs per request.
- **APNs sandbox vs production:** a token only works in the environment its build was signed for. One
  gateway serves one (`apns.use_sandbox`); run a second one for development builds.

## 8. Server configuration

```toml
[push.gateway]
url      = "https://gateway.example.com"
key_path = "/var/lib/teamcraft/push-gateway.key"   # created on first start, mode 0600
```

`key` (32 bytes, base64url, prefer `TEAMCRAFT_PUSH__GATEWAY__KEY`) works instead of `key_path`. Keep
the key: it is the server's identity at the gateway, and its quota and any block follow it. On start
the server registers the key (and again on the first push if the gateway doesn't know it). A server with
its own `[push.apns]` or `[push.fcm]` credentials sends to that provider directly.

The release builds of the server are meant to ship the publisher's gateway as the default URL, so a
server works with the store apps without extra configuration; until the publisher's gateway is public
the URL is empty and must be set. The admin console's push provider status shows APNs and FCM as
active when the gateway answers and as failing when it refuses or is unreachable.

## 9. Decisions

- **Per request, not a long-lived stream.** One signed POST per wake-up keeps the gateway stateless
  per connection and trivially load-balanceable. Batching is a later optimization if a server sends
  more pushes than a connection pool handles.
- **Server key pair instead of accounts or API keys.** A server proves itself with a key it generated;
  no registration form, no secret the gateway stores (only the public key), and a leaked gateway
  database doesn't let anyone impersonate a server.
- **Token limits across servers.** A device token is the thing worth protecting: limiting per token
  caps what any set of servers can do to one device.
- **The gateway uses the same sender code as direct mode** (`teamcraft-push-providers`): APNs and
  FCM behaviour (topics, priorities, dead-token classification) is identical either way.
