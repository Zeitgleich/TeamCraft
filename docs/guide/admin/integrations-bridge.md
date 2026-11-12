# Integration channels and the bridge

`teamcraft-bridge` is a small headless client that puts logs, alerts and webhooks into encrypted
channels. Run it **where the data comes from** (next to the service, on a NAS), never on the VPS that
runs the server. Full reference: [docs/bridge.md](../../bridge.md).

## Setup

1. **Register a bot** (admin):

   ```sh
   curl -X POST https://chat.example.com/api/v1/admin/bots \
     -H "Authorization: Bearer $ADMIN_TOKEN" -H 'Content-Type: application/json' \
     -d '{"name": "NAS bridge"}'
   ```

   The response contains the bot's user ID and a token `tcb_...`, **shown once**. The server keeps
   only its hash.
2. **Add the bot** to the space (role `guest`) and to each channel it serves, and flag those channels
   `is_integration` (`PATCH /conversations/{id}`). Integration channels get log retention (30 days by
   default) and push only for @mentions.
3. **Configure the bridge** from
   [teamcraft-bridge.example.toml](../../../crates/teamcraft-bridge/teamcraft-bridge.example.toml):
   server URL, bot token (`bot_token_file` or `TEAMCRAFT_BRIDGE_SERVER__BOT_TOKEN`), a database key
   (`openssl rand -base64 32`) and a recovery passphrase. Sources are not in the file: a workspace
   admin adds them in the app (step 5).
4. **Start it.** The first run enrolls the bot's device. A **member then adds the bot to each
   channel from a client**: that commits the bot into the encrypted group and hands it the keys. The
   bridge never adds itself.
5. **Add a source** (workspace admin): in **Channel settings → Integrations → Sources**, choose
   **Add source**, pick webhook or log lines and a name. The app shows the URL with its token **once**; copy it into the
   producer. The bridge reads the source from the channel itself and serves it within seconds, with
   no restart. To replace a leaked URL, revoke the source and add a new one
   ([docs/bridge.md](../../bridge.md#sources)).

## Sending data

```sh
curl -d '{"text":"Deploy finished","username":"ci"}' http://127.0.0.1:8088/hooks/<token>
journalctl -f | curl -T - -H 'Content-Type: text/plain' http://127.0.0.1:8088/ingest/<token>
```

`/hooks/<token>` takes Slack or Mattermost incoming-webhook JSON; `/ingest/<token>` takes log lines
(plain text or NDJSON). The token in the URL is the credential and decides the channel. The listener
is loopback by default; if you open it, use a firewall or TLS proxy.

A bridge that producers reach on its own host name needs that address in the app once: set
**Administration → Settings → Integrations → Bridge public URL** (`https://bridge.example.com`,
`http` only for localhost). A channel's "Connect a source" section then offers it as "At the
configured address" and selects it first; see [docs/bridge.md](../../bridge.md#where-producers-reach-the-bridge).

Log lines are batched into one message per second (or 64 KB). Sources have a rate limit (600/min by
default); alert levels and `danger` webhook attachments can @mention people.

**Levels.** A line's level is found after a timestamp or host prefix too (`2026-11-12T10:00:00Z ERROR
disk full`, `[warn] …`, `level=error`); a level word inside a sentence ("no error found") does not
count. Lines without a level are `info`. For systemd, send the journal as JSON
(`journalctl -u myservice -f -o json | curl -T - -H 'Content-Type: application/x-ndjson' …`): its
numeric `PRIORITY` becomes the level, while `-o short-iso` carries none. Details:
[docs/bridge.md, "Levels"](../../bridge.md#levels).

A producer that retries webhooks should send an `Idempotency-Key` header (one random value per
message, the same with every attempt): a retry after a lost answer is then answered without posting
twice. Keys are kept for 10 minutes, in memory (docs/bridge.md, "Retries").

## Operations

| Task               | How                                                                           |
|--------------------|-------------------------------------------------------------------------------|
| Rotate a bot token | `POST /admin/bots/{id}/token`; the old token stops at once; update the bridge |
| Revoke a bot       | `DELETE /admin/bots/{id}`; devices removed, sessions ended                    |
| List bots          | `GET /admin/bots`                                                             |
| Back up            | The bridge's database file. If lost, re-enroll with the passphrase            |
| Health             | `teamcraft-bridge healthcheck`, `GET /healthz` on its port                    |

## Limits

Webhooks take text and files (`multipart/form-data`, within the bridge's `[files]` limits; see the
bridge docs, "Files"). Sources are read from the channel and need no restart.

## Log view

Integration channels appear under **Integrations** in the clients as live log views: compact rows
with level colors, a *Jump to latest* follow mode, and a list that stays fast with tens of thousands
of lines because only the visible rows are drawn. Alerts that mention a person notify them even
though the channel pushes nothing else.
