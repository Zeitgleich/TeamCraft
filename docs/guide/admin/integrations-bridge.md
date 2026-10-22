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
   (`openssl rand -base64 32`), a recovery passphrase, and one `[[sources]]` block per producer with
   its token and target `conversation_id`.
4. **Start it.** The first run enrolls the bot's device. A **member then adds the bot to each
   channel from a client**: that commits the bot into the encrypted group and hands it the keys. The
   bridge never adds itself.

## Sending data

```sh
curl -d '{"text":"Deploy finished","username":"ci"}' http://127.0.0.1:8088/hooks/<token>
journalctl -f | curl -T - -H 'Content-Type: text/plain' http://127.0.0.1:8088/ingest/<token>
```

`/hooks/<token>` takes Slack or Mattermost incoming-webhook JSON; `/ingest/<token>` takes log lines
(plain text or NDJSON). The token in the URL is the credential and decides the channel. The listener
is loopback by default; if you open it, use a firewall or TLS proxy.

Log lines are batched into one message per second (or 64 KB). Sources have a rate limit (600/min by
default); alert levels and `danger` webhook attachments can @mention people.

## Operations

| Task               | How                                                                           |
|--------------------|-------------------------------------------------------------------------------|
| Rotate a bot token | `POST /admin/bots/{id}/token`; the old token stops at once; update the bridge |
| Revoke a bot       | `DELETE /admin/bots/{id}`; devices removed, sessions ended                    |
| List bots          | `GET /admin/bots`                                                             |
| Back up            | The bridge's database file. If lost, re-enroll with the passphrase            |
| Health             | `teamcraft-bridge healthcheck`, `GET /healthz` on its port                    |

## Limits

Text only (webhook file attachments are ignored); source configuration is read at start; the UI shows
integration channels as live log views in the **Integrations** section.
