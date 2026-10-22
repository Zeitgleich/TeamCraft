# Integration bridge

`teamcraft-bridge` (ARCHITECTURE.md §4.5) puts machine output into end-to-end encrypted channels:
logs, monitoring alerts, CI results, the hotline bot. It is a small headless TeamCraft client that
runs **where the data comes from** (next to the service, on a NAS, on the hotline host), never on
the VPS that runs the server.

```
 Grafana, CI, scripts,        HTTP (loopback)         encrypted          TeamCraft
 journalctl | curl      ─────────────────────▶ bridge ─────────────▶ server (ciphertext only)
```

## What it is

- A **bot account** and device: an admin registers it (`POST /api/v1/admin/bots`), adds it to the
  channels it serves, and the bridge enrolls like any client: own identity key, own device, own MLS
  leaf. It holds the content keys of its channels and encrypts itself; the server never sees
  plaintext. See "Bots" below.
- Built on `teamcraft-core`: same sync engine, same encrypted local store (SQLCipher), same
  gateway. It keeps the gateway connection so welcomes, commits and key updates reach it.
- Two local endpoints (loopback on IPv4 and IPv6 by default, configurable; dual stack with `[::]`):

| Endpoint               | Accepts                                                                                                      |
|------------------------|--------------------------------------------------------------------------------------------------------------|
| `POST /hooks/<token>`  | Slack/Mattermost incoming-webhook JSON (`text`, `username`, `attachments`, ...) or the form `payload=<json>` |
| `POST /ingest/<token>` | Log lines: plain text, or NDJSON with `Content-Type: application/x-ndjson`                                   |
| `GET /healthz`         | `200 ok`, for the container health check                                                                     |

  `icon_url`, `channel` and blocks of the Slack format are ignored: the source's token decides the
  conversation. Slack markup (`<url|label>`, `<@user>`, `<!here>`) becomes Markdown.

```sh
curl -d '{"text":"Deploy finished","username":"ci"}' http://127.0.0.1:8088/hooks/<token>
journalctl -f | curl -T - -H 'Content-Type: text/plain' http://127.0.0.1:8088/ingest/<token>
```

## Behaviour

- **Batching:** log lines of one conversation are collected and sent as one message (a code block,
  so clients show it monospaced) one interval (default 1 s) after the first line, or at 64 KB.
  Webhooks are sent one message per request.
- **Rate limits** per source (token bucket, default 600 per minute): webhooks over the limit get
  `429` with `Retry-After`; log lines over it are dropped and reported in the channel
  ("dropped N lines from nas-logs"), the response says how many were accepted.
- **Alerts** can @mention people: a webhook with `"mention": true` (an extension), webhook
  attachments with `color: "danger"` (if the source enables `danger_is_alert`), and log lines whose
  level is in `alert_levels`. The mentioned user IDs travel as message metadata, so they are notified
  even though integration channels push nothing else.
- **Search index:** the bridge has the plaintext, so it publishes the index segments of its channels
  (closed months, e2ee.md §10) itself, once an hour by default.
- **No channel yet:** messages for a channel the bot hasn't been added to wait (up to 1000) and go
  out once it joins. A lost connection is retried with backoff; sends that fail stay in the
  encrypted outbox of the local store.
- Tokens are part of the URL, so treat the URL as the credential: unknown tokens get `404`, tokens
  are compared in constant time, and the listener is loopback unless you open it.

## Setup

1. An admin registers the bot: `POST /api/v1/admin/bots {"name": "NAS bridge"}`. The response has
   the bot's user ID and its token (`tcb_...`), **shown once**.
2. An admin adds the bot to the space (role `guest`) and flags the channels it serves
   `is_integration` (`PATCH /conversations/{id}`): they get log retention (30 days by default,
   `[integration]` in the server config), and push only for @mentions.
3. Configure the bridge (`crates/teamcraft-bridge/teamcraft-bridge.example.toml`) with the server
   URL, the bot token, a database key and a recovery passphrase, and one `[[sources]]` block per
   producer. Start it; the first run enrolls the bot's device.
4. A member adds the bot to each channel from a client: that commits the bot's device into the
   channel's encrypted group and hands it the content keys. The bridge picks the welcome up through
   its gateway connection and starts sending.

## Bots

| What            | How                                                                                                                                                        |
|-----------------|------------------------------------------------------------------------------------------------------------------------------------------------------------|
| Create          | `POST /api/v1/admin/bots` (admins); the token is returned once, only its SHA-256 is stored                                                                 |
| Authenticate    | `Authorization: Bearer tcb_...` on the REST API and in the gateway `identify`                                                                              |
| Permissions     | Those of a guest: sees only the conversations it was added to                                                                                              |
| Rotate / revoke | `POST /admin/bots/{id}/token` (old token dies at once), `DELETE /admin/bots/{id}` (deactivates the account like any user: devices removed, sessions ended) |

A token is a long random string that the server can revoke, so there is no identity provider client
per bridge (client credentials would mean one confidential Keycloak client each) and no signed
token that outlives its revocation. Everything is audited.

## Container

`containers/teamcraft-bridge.Containerfile` (see `containers/README.md`). Mount the configuration
and the secrets, keep a volume at `/var/lib/teamcraft-bridge` for the database, publish the port on
loopback only: `-p 127.0.0.1:8088:8088`.

## Limits of the current version

- The bridge sends text. File attachments of webhooks (Slack `files`) are ignored.
- Source configuration is read at start; changing a source needs a restart.
- Channel membership of the bot is managed by members/admins, as for any user; the bridge never
  adds itself.
