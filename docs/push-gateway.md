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

1. The app asks its server how it reaches the gateway (`GET /api/v1/push/gateway`: the gateway URL, the
   server's ID there, the providers sent through it), gets the gateway's token key from the gateway
   itself, **seals** its APNs or FCM token for that gateway bound to that server ID and an expiry (§3a), and
   registers the sealed blob (`tcgw1:...`) with the server (`PUT /api/v1/devices/{id}/push`) together
   with a fresh 256-bit push key. The server never holds a usable device token.
2. To wake the device the server seals the wake-up with the push key and posts it with the sealed token to
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

`accept_unbound_tokens = false` (default) is the transition setting described in §3a.
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

### 3a. Bound device tokens

An APNs or FCM token is a bearer secret. If the app gave the raw token to every server it signs in to,
any of them (or a copy of its database) could push to the device through any gateway identity. So:

- **The app seals** `{provider, server_id, expires_at, token}` to the gateway's token key (HPKE; format
  and known answer in the gateway repository's api.md §6 and the shared fixtures). The key comes from
  `GET /v1/info` of the gateway itself; a store app uses the gateway it was built for
  (`push_gateway_url` of the app config: Android `-Pteamcraft.pushGatewayUrl`, iOS
  `TEAMCRAFT_PUSH_GATEWAY_URL`) and only falls back to the one the server names. A store app never hands
  a raw APNs/FCM token to a server: if the server names no gateway (or none for that provider), the app
  doesn't register at all, and an existing sealed registration is kept rather than replaced by a raw
  token. Self-built apps without a built-in gateway take the server's gateway (a server operator who
  builds the app trusts their own server) or, without one, register the raw token for direct sending.
- **The server** stores and forwards the blob (`tcgw1:` + base64url) as the token of the registration.
  It accepts only sealed tokens for providers it sends through a gateway, and refuses them for providers it
  sends directly. A registration made before sealing existed (a raw token) is dead weight: the gateway
  refuses it (`410`) and the server forgets it, and the app registers again, sealed.
- **The gateway** opens the blob and checks that the authenticated server is the one named in it, that it
  has not expired and has not been revoked.
- **Renewal.** The app seals again after a week (it picks up a rotated gateway key; the token lives 30
  days, or less when the gateway's `max_token_lifetime_secs` is lower; then it seals again after a
  quarter of that), when the server's ID or gateway changes, and on every start (`refresh_push`); the old blob is
  revoked. When the user **leaves or removes the server** (sign-out, `unregister_push`), the app revokes
  the blob at the gateway (`POST /v1/tokens/revoke`), so the server can't push any more.

`[push.gateway] accept_unbound_tokens = true` is a **transition** for old apps that don't seal yet: the
server then accepts and sends raw tokens (the gateway has to allow them, `tokens.accept_unbound`). It is
off by default; with it on, a raw token can be used by any server that learns it. A raw token a server
ever held stays usable to it even after the transition: the gateway's key is public, so the server can
seal that token for itself. Keep the transition short; Android apps get a fresh FCM token when they switch
to sealing, iOS tokens change only with a reinstall.

## 4. How answers map onto the server's push errors

| Gateway answer                                                                                                | Server                                                                      |
|---------------------------------------------------------------------------------------------------------------|-----------------------------------------------------------------------------|
| `200`                                                                                                         | delivered                                                                   |
| `410 token_gone`, `token_unbound`, `token_expired`, `token_invalid`, `token_revoked`, `token_server_mismatch` | **forget the device's push registration** (the app registers again, sealed) |
| `401 unknown_server`                                                                                          | register the key (once for all waiting pushes), retry once                  |
| `429 rate_limited`, `429 quota_exceeded`                                                                      | dropped; no push is sent until `Retry-After` has passed                     |
| `429 token_rate_limited`, `400 invalid_request`                                                               | dropped; a problem of that token, not counted against the gateway           |
| `5xx`, `502 provider_unavailable`                                                                             | dropped, shows in the provider health; nothing deleted                      |
| `403 server_blocked`, `401 unauthorized`, `502 provider_rejected`, `501 provider_not_configured`              | dropped (rejected), shows as failing; nothing deleted                       |

A dropped push is not retried: wake-ups are hints, the app catches up when it runs. A bare `410`
without one of these codes (a proxy, a wrong URL) deletes nothing. After the gateway refuses to register
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
