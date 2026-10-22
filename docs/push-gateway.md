# Push gateway

The iOS and Android store apps are signed and published by one party (the **publisher**). Apple and
Google tie push delivery to the app's signing identity: only the holder of the app's APNs key and FCM
project can send to its devices. A self-hosted server can't have those credentials, so it sends the
pushes for the store apps through a **push gateway** that holds them. Like Mattermost's push proxy or
Element's Sygnal, the gateway is its own project:
[Zeitgleich/teamcraft-push-gateway](https://github.com/Zeitgleich/teamcraft-push-gateway). This page is
the server's side; the gateway's design, the HTTP contract and its operation are documented there
([design.md](https://github.com/Zeitgleich/teamcraft-push-gateway/blob/main/docs/design.md),
[api.md](https://github.com/Zeitgleich/teamcraft-push-gateway/blob/main/docs/api.md),
[operations.md](https://github.com/Zeitgleich/teamcraft-push-gateway/blob/main/docs/operations.md)).
Related: [e2ee.md §7](e2ee.md), [notifications.md](notifications.md), [operations.md](operations.md).

## 1. Principles

- **No special role for the publisher's own server.** It sends through the gateway like every other
  server: same API, same limits, same signature check.
- **Direct stays possible.** Whoever builds and signs their own apps has their own APNs key and FCM
  project and configures `[push.apns]` / `[push.fcm]`; no gateway is involved.
- **Web Push and UnifiedPush never use it.** Those endpoints belong to the user's browser or distributor.
- **The gateway sees ciphertext only.** The wake-up is sealed with the device's push key before it leaves
  the server (e2ee.md §7), so the gateway handles what Apple and Google handle: a device token and
  ciphertext. Threat model: e2ee.md §7.1.

## 2. Flow

```
 device ──registers token + push key──▶ teamcraft-server ──(sealed wake-up, token; signed)──▶ push gateway ──▶ APNs / FCM ──▶ device
```

1. The app registers with the server as before (`PUT /api/v1/devices/{id}/push`): provider `apns` or
   `fcm`, the platform token, a fresh 256-bit push key. Nothing changes in the apps.
2. To wake the device the server seals the wake-up with the push key and posts it with the token to
   `POST /v1/push` of the gateway, signed with the server's gateway key.
3. The gateway checks signature and limits and forwards to Apple or Google with the publisher's
   credentials; it answers whether the token is dead (`410 token_gone`).
4. The device opens the wake-up with the push key and fetches the message from its own server.

The server picks the sender per provider: credentials under `[push.apns]` / `[push.fcm]` win; otherwise
`[push.gateway]` is used when set; otherwise the provider is not configured and registrations for it are
refused.

## 3. Server configuration

```toml
[push.gateway]
url      = "https://push-gw.zeitgleich.de"   # the publisher's gateway for the official store apps
key_path = "/var/lib/teamcraft/push-gateway.key"   # created on first start, mode 0600
```

`key` (32 bytes, base64url, prefer `TEAMCRAFT_PUSH__GATEWAY__KEY`) works instead of `key_path`. The key
is an Ed25519 key pair used only towards the gateway: its public half identifies the server there
(an opaque ID, the hash of the public key), and the gateway's quota and any block follow it. Keep it;
losing it only means registering a new identity. On start the server registers the key (and again on
the first push if the gateway doesn't know it; a gateway that lost its database is learned again the same
way). Requests are signed in `teamcraft-crypto` (`push_gateway.rs`).

The publisher of the official store apps runs its gateway at `https://push-gw.zeitgleich.de`; point
`url` there to make the official apps work with your server. The server code has no built-in host: the
URL is always configuration (the deployment playbook sets it by default).
The admin console's push provider status shows APNs and FCM as active when the gateway answers and as
failing when it refuses or is unreachable.

## 4. How answers map onto the server's push errors

| Gateway answer                                                                                   | Server                                                            |
|--------------------------------------------------------------------------------------------------|-------------------------------------------------------------------|
| `200`                                                                                            | delivered                                                         |
| `410 token_gone`                                                                                 | **forget the device's push registration**                         |
| `401 unknown_server`                                                                             | register the key (once for all waiting pushes), retry once        |
| `429 rate_limited`, `429 quota_exceeded`                                                         | dropped; no push is sent until `Retry-After` has passed           |
| `429 token_rate_limited`, `400 invalid_request`                                                  | dropped; a problem of that token, not counted against the gateway |
| `5xx`, `502 provider_unavailable`                                                                | dropped, shows in the provider health; nothing deleted            |
| `403 server_blocked`, `401 unauthorized`, `502 provider_rejected`, `501 provider_not_configured` | dropped (rejected), shows as failing; nothing deleted             |

A dropped push is not retried: wake-ups are hints, the app catches up when it runs. A bare `410`
without `token_gone` (a proxy, a wrong URL) deletes nothing. After the gateway refuses to register
the server, pushes fail at once for five minutes instead of trying again each time.

Only `token_gone` deletes a registration: a gateway that blocks, rate-limits or breaks must never
cost a server its devices.

## 5. The contract and its tests

The contract is the versioned HTTP API `/v1` (api.md in the gateway repository). Both repositories carry
the same JSON fixtures, one per request and response plus a known-answer signature; keep them
identical when the contract changes:

| Repository             | Fixtures                                              | Checked by                                                                 |
|------------------------|-------------------------------------------------------|----------------------------------------------------------------------------|
| TeamCraft              | `crates/teamcraft-proto/tests/fixtures/push_gateway/` | `teamcraft-proto` (parse and re-serialize), `teamcraft-crypto` (signature) |
| teamcraft-push-gateway | `tests/fixtures/`                                     | `tests/suite/contract.rs`                                                  |

The server's tests run the gateway client against a fake gateway that verifies signatures like the real one
(`tests/suite/push_gateway.rs`) and the whole server against it (`push_sending.rs`, `via_gateway`): message
in, sealed wake-up out, dead token dropped, blocks kept.

## 6. Running a gateway

Only the publisher of store apps runs one: see the gateway repository (container image
`ghcr.io/zeitgleich/teamcraft-push-gateway`) and the optional `push_gateway` role in the deployment
repository (`teamcraft-deploy`, off by default, on its own host).
