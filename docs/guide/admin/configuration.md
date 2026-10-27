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

| Section                        | Purpose                                                                          | Needed          |
|--------------------------------|----------------------------------------------------------------------------------|-----------------|
| `[server]`                     | Listen address (default `[::]:8080`), instance `worker_id` (unique per instance) | yes             |
| `[database]`                   | Postgres URL, pool size                                                          | yes             |
| `[oidc]`, `[oidc.roles]`       | Issuer, client IDs, role name mapping, accepted algorithms                       | yes             |
| `[oidc.sync]`                  | Deprovisioning poll with the service account (off by default)                    | recommended     |
| `[logging]`                    | `tracing` filter, `pretty` or `json`                                             | no              |
| `[policy]`                     | Guest DM scope, invite e-mail verification                                       | no              |
| `[limits]`                     | Message, page, MLS and key bag sizes                                             | no              |
| `[gateway]`                    | Heartbeat, resume window, frame size                                             | no              |
| `[rendezvous]`                 | Device-linking relay limits                                                      | no              |
| `[storage]`                    | S3 endpoint, bucket, keys, attachment size limit (default 100 MiB)               | for files       |
| `[retention]`                  | Background job that enforces retention                                           | no              |
| `[notifications]`              | Read receipt default, urgent message repeat                                      | no              |
| `[integration]`                | Default retention (days) of integration channels                                 | no              |
| `[guests]`                     | Default guest expiry (90 days), allowed e-mail domains, expiry check interval    | no              |
| `[local_accounts]`             | Guests with their own password and authenticator code (off by default)           | for such guests |
| `[mail]`, `[mail.smtp]`        | Invitation and activation e-mail, public URL for links                           | for invitations |
| `[notifications.email_digest]` | Content-free e-mail digest of missed mentions (off by default)                   | no              |
| `[presence]`                   | "In a meeting" / "in a call" statuses, calendar polling                          | no              |
| `[shares]`                     | Share links for external download: expiry, download cap, public URL              | no              |
| `[security]`                   | Request size, HSTS, `X-Forwarded-For`, per-minute rate limits                    | no              |
| `[web]`                        | Directory of the web client and its CSP                                          | for web         |
| `[branding]`, `[[brands]]`     | Several host names, each with its own look and mail sender                       | for brands      |
| `[meetings]`                   | Public URL, guest token secret, CalDAV, SMTP, mail texts                         | for meetings    |
| `[push]`                       | VAPID key for Web Push                                                           | for push        |
| `[calls]`                      | SFU URL, join token signing key                                                  | for calls       |

Sections that are optional features (`[storage]`, `[meetings]`, `[calls]`) make their endpoints
answer **503** when absent.

## Several brands on one server

One server can answer under several host names, each with its own name, logo, colors, login text,
support and legal links, and e-mail sender (for affiliated companies that share one workspace). It is
presentation only: users, spaces and permissions are the same behind every host name. Configure
`[branding]` (where the image files are, what happens to unknown host names) and one `[[brands]]` entry per
brand; `teamcraft.example.toml` documents every key and [docs/branding.md](../../branding.md) explains the
design. Rules worth knowing:

- Exactly one brand is the default. A request is matched to a brand by its host name, exactly; unknown
  host names are answered with `421` (`unknown_host = "reject"`, the default) or get the default brand
  (`unknown_host = "default"`). Brands are not tenants: everything behind them is shared.
- Invitation and meeting e-mail goes out as the brand that lists the recipient's domain
  (`email_domains`), otherwise as the inviter's own brand (their e-mail domain).
- A share link works only on the host names of the brand that made it.
- Every host name must be listed at the identity provider as a redirect URI
  ([Keycloak](keycloak.md)) and be served by the reverse proxy with a certificate.
- `[oidc] organization_claim` names a token claim that is shown as the person's organization.

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

| Key                                        | Default    | Effect                                                                         |
|--------------------------------------------|------------|--------------------------------------------------------------------------------|
| `guests_dm_start`                          | `"nobody"` | Whom a guest may start a DM or group DM with: `nobody`, `shared` or `anyone`   |
| `guests_dm_only_with_shared_conversations` | unset      | Deprecated (1.0.9): `true` means `shared`, `false` means `anyone`; wins if set |
| `invites_require_verified_email`           | `true`     | A pending space invite only applies when the provider verified the address     |

`guests_dm_start` only concerns what a **guest** starts. Members, admins and owners can always open a DM
or group DM with a guest, and a guest can always answer in a conversation somebody started with them.
The signed policy (Administration → Security → "Guests may start direct messages with") wins over the file.

## E-mail the server sends

Three kinds of mail, each with its own settings:

| Mail                                      | Settings                                            | Needs                                      |
|-------------------------------------------|-----------------------------------------------------|--------------------------------------------|
| Space invitations, guest activation links | `[mail]` texts and `public_base_url`, `[mail.smtp]` | SMTP                                       |
| Meeting invitations                       | `[meetings.smtp]`, `[meetings.mail_texts]`          | SMTP (can be the same)                     |
| Digest of missed mentions                 | `[notifications.email_digest]`                      | `[mail.smtp]` and `[mail] public_base_url` |

The **digest** is content-free: it says only how many mentions are still unread ("3 new mentions")
and links to the app, because the server cannot read messages. People opt in themselves
(`GET`/`PUT /me/email-digest`; there is no settings switch in the apps yet). Keys:
`is_enabled` (default `false`), `default_interval_secs` (3600), `min_interval_secs` (900), `grace_secs`
(300; a mention counts as missed once it is this old and unread), `scheduler_interval_secs` (60) and
the subject and body texts with `{count}` and `{url}`. A mail that fails is retried after five
minutes without losing the count.

`[notifications] mobile_push_delay_secs` (120) holds pushes to a person's phones while one of their
desktops is active and sends them only if the message is still unread; `0` pushes at once.
