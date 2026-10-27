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
  conversation. Slack markup (`<url|label>`, `<@user>`, `<!here>`) becomes Markdown. A source added
  in the app opens one of the two endpoints (its kind); a token on the other one gets `404`.

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
- Tokens are part of the URL, so treat the URL as the credential: unknown tokens get `404`, the
  bridge knows only their SHA-256 and compares it in constant time, and the listener is loopback
  unless you open it.

## Setup

In the app:

1. **Admin console → Integrations:** an admin creates the bot (a name). The bot's token (`tcb_...`)
   is **shown once** with a ready-to-paste `teamcraft-bridge.toml` snippet; "New token" replaces the
   token at once, "Revoke" deactivates the bot. (API: `POST /api/v1/admin/bots`.)
2. **Channel settings → Integrations** (channel admins): add the bot to the channel. If the bot is not
   in the channel's space yet it joins it as a guest first (space admins), then its device is
   committed into the channel's encryption group from the client and handed the content keys. A bot
   can be added to **any** channel; a bot's messages carry a bot badge.
3. **Channel settings → Integrations → Sources** (workspace admins): "Add source" with a name and a
   kind (webhook or log lines) shows the URL once, to point Grafana, CI or `journalctl` at; the
   bridge serves it within a second, with no configuration change and no restart (see "Sources"
   below).
4. **Channel settings → Integrations → Mode** (or "Channel type" in the new-channel dialog): "Normal"
   or "Log channel" (`is_integration`: log view, log retention, 30 days by default, `[integration]`
   in the server config, and push only for @mentions). Log channels are private; switch an existing
   private channel at any time.
5. Configure the bridge once (`crates/teamcraft-bridge/teamcraft-bridge.example.toml`) with the
   server URL, the bot token, a database key and a recovery passphrase. Start it; the first run
   enrolls the bot's device and it picks the welcomes of its channels up through its gateway
   connection. Sources need no entry here.

The bridge never adds itself to a channel, and the app never shows a bot's or a source's token again
after the first display (only their hashes are kept).

## Sources

A source is a token that posts into one channel. Channel admins manage them in the app; the bridge
reads them from the channel itself (e2ee.md §13.1):

1. "Add source" (workspace admins) generates the token in the app (256 random bits, `tcs_...`) and sends an encrypted,
   hidden control message into the channel: source ID, name, kind, the token's SHA-256 and the
   options (webhooks: attachments with `color: "danger"` mention people; log lines: the default alert
   levels; 600 per minute). The token and its URL are shown once, with a copy button.
2. The bridge is a member of the channel, so it decrypts the message like every member device. It
   keeps the sources in its encrypted store and swaps its table of tokens live (checked once a
   second). A restart serves them from the store at once.
3. "Revoke" sends another control message; the token stops working as soon as the bridge read it,
   and a revoked source never comes back. The list shows the sources in force with who added them
   and when; to replace a leaked URL, revoke the source and add a new one.

Who may: **workspace admins** of the verified roster (admin.md §2.3). Channel roles aren't signed
yet (issue #380), so the bridge doesn't take the server's word for them, and the app offers "Add
source" only to workspace admins. The creator must also be a member of the channel, and stay an
admin: a creator who loses the role or leaves takes their sources out of force. Admins and the
source's creator may revoke; changes from anybody else are ignored before they are stored. The
server only carries ciphertext, so it can neither add nor change a source. It can withhold
messages, though: a bridge with a new database may miss a revocation the server leaves out of the
history; one that read it keeps it (e2ee.md §13.1).

Limits: at most 50 sources in force per channel (the oldest), each with its rate limit, and all
sources together at most `listen.total_rate_limit_per_minute` (12000) requests and lines a minute.

The bridge learns of sources through the channel, so the bot must be in the channel before a source
is added (the app offers "Add source" only then): a bot added later reads history from its join on.
When its database is new or was lost, the bridge reads the channels' older history once
(`[app_sources] history_pages`, 200 pages of 50 messages) for older announcements; sources older than
that have to be added again.

**Sources in the file.** A bridge on its own host may still list `[[sources]]` (token, conversation,
mentions) in its configuration, read at start; they serve both endpoints. `[app_sources] enabled =
false` turns app-managed sources off for such a bridge.

### Where producers reach the bridge

The channel section's "Where the bridge runs" switch picks the base of the URLs it shows:

| Choice             | URLs                                                       | When                                                                                                                          |
|--------------------|------------------------------------------------------------|-------------------------------------------------------------------------------------------------------------------------------|
| On this server     | `https://<instance host>/hooks/<token>`, `/ingest/<token>` | The deployment runs the bridge (teamcraft-deploy role `teamcraft_bridge`); its proxy routes both paths to the loopback bridge |
| On its own machine | `http://127.0.0.1:8088/hooks/<token>`, `/ingest/<token>`   | The bridge runs next to the producers on its default listener; use its own address instead if you opened the listener         |

The first is the default. The app does not check whether a bridge answers there: on an instance
without the bridge role the instance's host name has no `/hooks/` route.

## Bots

| What            | How                                                                                                                                                        |
|-----------------|------------------------------------------------------------------------------------------------------------------------------------------------------------|
| Create          | `POST /api/v1/admin/bots` (admins); the token is returned once, only its SHA-256 is stored                                                                 |
| Authenticate    | `Authorization: Bearer tcb_...` on the REST API and in the gateway `identify`                                                                              |
| Permissions     | Those of a guest: sees only the conversations it was added to                                                                                              |
| Rotate / revoke | `POST /admin/bots/{id}/token` (old token dies at once), `DELETE /admin/bots/{id}` (deactivates the account and removes it from all chats: devices removed, sessions ended) |

A token is a long random string that the server can revoke, so there is no identity provider client
per bridge (client credentials would mean one confidential Keycloak client each) and no signed
token that outlives its revocation. Everything is audited.

## Container

Each release publishes `ghcr.io/zeitgleich/teamcraft-bridge:X.Y.Z` (multi-arch, signed, like the
server image; docs/release.md), built from `containers/teamcraft-bridge.Containerfile` (see
`containers/README.md`). The deployment playbook runs it with `teamcraft_bridge_enabled`. Mount the configuration
and the secrets, keep a volume at `/var/lib/teamcraft-bridge` for the database, publish the port on
loopback only: `-p 127.0.0.1:8088:8088`.

## Limits of the current version

- The bridge sends text. File attachments of webhooks (Slack `files`) are ignored.
- `[[sources]]` in the configuration file are read at start; changing one needs a restart. Sources
  added in the app don't.
- Sources added in the app have no alert mentions yet (the file's `mentions` has no app equivalent)
  and no rename in the UI (the control message exists).
- Channel membership of the bot is managed by members/admins, as for any user; the bridge never
  adds itself.
- The bot list shows no last-activity time and bots have no picture yet.
