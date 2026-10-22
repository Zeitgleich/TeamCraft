# Configuration

One TOML file plus environment overrides.

```sh
teamcraft-server --config teamcraft.toml     # or TEAMCRAFT_CONFIG=/etc/teamcraft/teamcraft.toml
```

[teamcraft.example.toml](../../../teamcraft.example.toml) documents **every option with its
default**. The server rejects unknown keys at startup, so a typo fails fast.

## Environment overrides

Prefix `TEAMCRAFT_`, nesting with a double underscore, upper case:

| TOML                            | Environment variable                     |
|---------------------------------|------------------------------------------|
| `[database] url`                | `TEAMCRAFT_DATABASE__URL`                |
| `[oidc.roles] guest`            | `TEAMCRAFT_OIDC__ROLES__GUEST`           |
| `[oidc.sync] client_secret`     | `TEAMCRAFT_OIDC__SYNC__CLIENT_SECRET`    |
| `[server] listen_addr`          | `TEAMCRAFT_SERVER__LISTEN_ADDR`          |
| `[push] vapid_private_key`      | `TEAMCRAFT_PUSH__VAPID_PRIVATE_KEY`      |
| `[meetings] guest_token_secret` | `TEAMCRAFT_MEETINGS__GUEST_TOKEN_SECRET` |

Keep secrets in the environment or in files mounted by the orchestrator, not in the TOML file.

## Sections

| Section                  | Purpose                                                                          | Needed       |
|--------------------------|----------------------------------------------------------------------------------|--------------|
| `[server]`               | Listen address (default `[::]:8080`), instance `worker_id` (unique per instance) | yes          |
| `[database]`             | Postgres URL, pool size                                                          | yes          |
| `[oidc]`, `[oidc.roles]` | Issuer, client IDs, role name mapping, accepted algorithms                       | yes          |
| `[oidc.sync]`            | Deprovisioning poll with the service account (off by default)                    | recommended  |
| `[logging]`              | `tracing` filter, `pretty` or `json`                                             | no           |
| `[policy]`               | Guest DM scope, invite e-mail verification                                       | no           |
| `[limits]`               | Message, page, MLS and key bag sizes                                             | no           |
| `[gateway]`              | Heartbeat, resume window, frame size                                             | no           |
| `[rendezvous]`           | Device-linking relay limits                                                      | no           |
| `[storage]`              | S3 endpoint, bucket, keys, attachment size limit (default 100 MiB)               | for files    |
| `[retention]`            | Background job that enforces retention                                           | no           |
| `[notifications]`        | Read receipt default, urgent message repeat                                      | no           |
| `[integration]`          | Default retention (days) of integration channels                                 | no           |
| `[security]`             | Request size, HSTS, `X-Forwarded-For`, per-minute rate limits                    | no           |
| `[web]`                  | Directory of the web client and its CSP                                          | for web      |
| `[meetings]`             | Public URL, guest token secret, CalDAV, SMTP, mail texts                         | for meetings |
| `[push]`                 | VAPID key for Web Push                                                           | for push     |
| `[calls]`                | SFU URL, join token signing key                                                  | for calls    |

Sections that are optional features (`[storage]`, `[meetings]`, `[calls]`) make their endpoints
answer **503** when absent.

## Behind a reverse proxy

- Terminate TLS at the proxy; pass WebSocket upgrades on `/api/v1/gateway` and `/api/v1/rendezvous`
  without buffering.
- Set `[security] trust_forwarded_for = true` only when the proxy sets `X-Forwarded-For` (the last
  entry is used) and clients cannot reach the server directly. Set `hsts = true` for HTTPS-only.
- Do not publish `/api/internal/`: it is for the SFU and the mail inbox handler.

## Policies that are not in the file

Instance policies that admins change at runtime (history default, retention, `admin_self_join`,
organization recovery) are a **signed policy** edited in Administration, not TOML
([Security policy](security-policy.md)). `[policy]` in the file holds only the two server-enforced
guest rules.

## Policy knobs in `[policy]`

| Key                                        | Default | Effect                                                                     |
|--------------------------------------------|---------|----------------------------------------------------------------------------|
| `guests_dm_only_with_shared_conversations` | `true`  | Guests only open DMs with people they share a conversation with            |
| `invites_require_verified_email`           | `true`  | A pending space invite only applies when the provider verified the address |
