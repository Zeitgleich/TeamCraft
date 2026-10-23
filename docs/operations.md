# Operations

What an operator needs to run TeamCraft: a quickstart for a fresh server, the pieces, health checks,
backups, upgrades and secret rotation. Configuration reference: `teamcraft.example.toml`; images:
`containers/README.md`; releases: `docs/release.md`.

## Quickstart: a fresh server in 15 minutes

The reference deployment is the Ansible playbook in
[Zeitgleich/teamcraft-deploy](https://github.com/Zeitgleich/teamcraft-deploy). It installs TeamCraft
on one Debian 13 server with **rootless Podman and Quadlet**, each component under its own
unprivileged user, IPv4 and IPv6, with the identity provider (Keycloak) and the attachment storage
(Garage) bundled or external, secrets generated on the server, Let's Encrypt certificates, a firewall
and nightly database backups.

```sh
git clone https://github.com/Zeitgleich/teamcraft-deploy.git && cd teamcraft-deploy
mkdir -p inventory/host_vars/chat.example.com
cp examples/hosts inventory/hosts                                   # the server's address
cp examples/vars.yml inventory/host_vars/chat.example.com/vars.yml  # domain and admin e-mail
just install-all
just ensure-admin alice 'a long password'                           # first user, role owner
just self-check
```

What you need first: a Debian 13 VPS (2 vCPU, 4 GB RAM) with public IPv4 and IPv6, A and AAAA records
for `chat.example.com`, `sfu.chat.example.com` and, when bundled, `id.` and `s3.` below it, and UDP
port 4433 open for calls. The playbook's documentation covers DNS, configuration, secrets (generated
or sops + age), updating, backups and every component.

Several brands on one server (`docs/branding.md`) are `teamcraft_brands` in the playbook: it adds
the host names to Caddy and the Keycloak client.

Then point the desktop app at `https://chat.example.com` and sign in. The first device of every user
creates their keys; set up the recovery passphrase when asked.

### Running the images yourself

The rest of this guide applies to any way of running the images. Pitfalls the deployment's dry run
found:

- The server image sets `TEAMCRAFT_SERVER__LISTEN_ADDR=[::]:8080`, and environment variables win over
  the TOML file. With host networking, set the variable yourself (e.g. `127.0.0.1:8080`), or the server
  is reachable on every address of the host, bypassing the proxy.
- The images are distroless: health checks must use the exec form
  (`["/usr/local/bin/teamcraft-server", "healthcheck", "--ready"]`), there is no shell.
- Rootless containers run as a subordinate uid: bind-mounted configuration files must be readable by
  others (they hold no secrets; pass secrets as podman secrets or environment variables).
- When the identity provider runs on the same host, point `[oidc] jwks_uri` at its loopback address:
  the server then fetches keys without a round trip through the proxy (and without trusting its CA).

The playbook's dry run (`test/dry-run.sh` there) deploys to a throwaway container and uses this
repository's end-to-end harness against it (`tests/e2e/tests/suite/deployed.rs`, `E2E_DEPLOYED_URL`,
`DEV_TOKEN_ISSUER` for `dev/dev-token.py`).

## Pieces and what holds state

| Piece              | State                                                                     | Needs backup |
|--------------------|---------------------------------------------------------------------------|--------------|
| `teamcraft-server` | none of its own (calls roster and rate limits are in memory)              | config only  |
| PostgreSQL         | accounts, structure, **ciphertext** of messages and MLS queues, audit log | yes          |
| S3 bucket          | encrypted attachments and search index segments                           | yes          |
| `teamcraft-sfu`    | none (rooms live while a call lasts)                                      | config only  |
| `teamcraft-bridge` | SQLCipher database with the bot's keys and its channels' content keys     | yes          |
| Identity provider  | users, roles, sessions                                                    | its own      |

The server only holds ciphertext. A stolen database or bucket gives an attacker metadata (who is in
which conversation, when messages were sent) but no message content or attachment keys. Clients
hold the keys; see `docs/e2ee.md` for what is recoverable and how (backup key, passphrase, device
linking, organization recovery). **Losing the database loses everybody's history** unless a backup
exists; clients can still read what they have stored locally.

## Health and monitoring

- `GET /healthz` is liveness, `GET /readyz` also checks the database. The images ship
  `teamcraft-server healthcheck [--ready]`, `teamcraft-sfu healthcheck` and `teamcraft-bridge
  healthcheck` for container health checks (no curl in the images).
- Logs are structured `tracing` output on stderr (`RUST_LOG` / `[logging]`). They never contain
  tokens, keys or message content (checked by tests); IDs of public objects are logged.
- Watch: 5xx rate, 429 rate (rate limits, `docs/performance.md`), database connection saturation
  (`[database] max_connections`), disk of Postgres (message partitions), gateway connections.

## Reverse proxy and network

- TLS terminates at the proxy. It must pass WebSocket upgrades on `/api/v1/gateway` and
  `/api/v1/rendezvous` and not buffer them. Set `[security] trust_forwarded_for = true` and
  `hsts = true` once the proxy sets `X-Forwarded-For` (the last entry is used) and serves HTTPS only.
  **Without `trust_forwarded_for` behind a proxy, every client has the proxy's address**: they share one
  rate limit, and bad tokens from anybody block everybody. The server warns once when it sees
  `X-Forwarded-For` from a private address.
- Never expose `/api/internal/` to the internet: it is for the SFU and the mail inbox handler, and is
  protected by bearer tokens, but there is no reason to publish it.
- Everything facing the network is dual stack (IPv4 and IPv6). The SFU needs UDP `4433` reachable
  from clients (QUIC); its health endpoint (`8081/tcp`) is for the orchestrator only.

## Backup and restore

### PostgreSQL

Use continuous archiving for production (base backup + WAL, `pg_basebackup` or pgBackRest/Barman) so
that a restore can stop at a point in time. A logical dump is enough for small instances:

```sh
pg_dump --format=custom --file=teamcraft-$(date +%F).dump "$DATABASE_URL"
```

Notes:

- `messages` and `mls_messages` are partitioned by month. `pg_dump` handles that; a dump of a big
  instance is large. Backups of closed months never change, so incremental tools save a lot.
- Restore into an empty database with the **same or an older** server version (migrations are
  forward-only; the server migrates on startup):

  ```sh
  createdb teamcraft && pg_restore --dbname=teamcraft --no-owner teamcraft-2026-10-22.dump
  ```

  Then start the server. It creates the partitions it needs ahead of time.
- Test the restore regularly: restore to a scratch database, start a server against it, and check
  `/readyz` and a login.
- Retention deletes rows (and drops emptied partitions) permanently. A backup is the only way back,
  which is also why a backup must be deleted when the retention promise says so.

### S3

Attachments, index segments and brand images are immutable objects named by random IDs, so a plain
`rclone sync`/`mc mirror` to a second bucket, or the provider's bucket versioning and replication,
works. The database references objects by key; a restore needs the database and the bucket from
about the same time. Objects without a database row are garbage (an upload that never completed) and
harmless; rows without an object show up as failed downloads.

### Restore order after a total loss

1. Postgres from the latest backup, S3 bucket from its copy.
2. Server and SFU with the same configuration and **the same secrets** (see below), including the
   settings key (`[secrets]`; without it the mail and calendar passwords set in the console must be
   entered again), identity provider reachable.
3. Bridges: restore their database file (it holds the bot's keys) or re-enroll them with their
   passphrase (the bot's backup key is on the server, so a lost bridge database is recoverable).
4. Clients reconnect and run a full sync; devices whose rows were lost re-register.

## Upgrades

- Images are pinned by digest in the deployment; build or pull the new ones, never `latest`.
- Migrations are forward-only and run automatically when the server starts. **Take a database backup
  first.** There is no downgrade: to go back, restore the backup and start the old version.
- Run exactly **one** `teamcraft-server` instance. Gateway sessions, presence, device-linking relays,
  rate limits and the call roster live in the server's memory, and there is no bus between instances
  yet: events published on one instance never reach clients connected to another. Upgrade by
  replacing the instance (a short interruption), not by rolling several.
- Order: database backup, new `teamcraft-server`, then `teamcraft-sfu`, then bridges. The SFU
  verifies join tokens the server signs; the token format is versioned with the server. An old
  server binary refuses to start against a database migrated by a newer one.
- A restarting server drops WebSocket connections and forgets their sessions: every client
  reconnects (with backoff) and runs a full sync, so expect a burst of API traffic right after the
  restart. A reverse proxy in front must keep idle WebSockets open for at least 60 s (clients send a
  heartbeat every 25 s). Calls in progress
  live in the SFU and survive a server restart (the server's call roster is in memory and starts empty;
  it is filled by the SFU's participant reports once the SFU sends them).
- Check `docs/` and the release notes for configuration changes before upgrading. The server rejects
  unknown configuration keys at startup, so a stale key fails fast instead of being ignored.

## Push notifications

Push wakes sleeping phones and browsers. The payload is sealed with a per-device key (e2ee.md §7), so
Apple, Google and push relays only see ciphertext; the app then fetches and decrypts the message.
Without a provider configured, devices of that kind only get messages while the app is open. Every
provider is on as soon as its credentials are set; `GET /api/v1/admin/instance` and the admin console
(Instance → Push providers) show per provider whether it is configured, when it last delivered, how
many pushes failed since the server started and the last error.

| Provider    | For                       | Needs                                                                              | Configuration                                    |
|-------------|---------------------------|------------------------------------------------------------------------------------|--------------------------------------------------|
| Web Push    | browsers (PWA)            | a VAPID key pair                                                                   | `[push] vapid_private_key`, `vapid_subject`      |
| APNs        | iOS, including call rings | store app: a push gateway; own app: an Apple `.p8` key, key ID, team ID, bundle ID | `[push.gateway]` or `[push.apns]`                |
| FCM         | Android with Google       | store app: a push gateway; own app: a Firebase service account JSON key            | `[push.gateway]` or `[push.fcm]`                 |
| UnifiedPush | Android without Google    | nothing (the app hands over the distributor's endpoint and keys)                   | `[push.unified_push] is_enabled` (on by default) |

**Store apps and the push gateway.** The iOS and Android store apps are signed by their publisher, and
Apple and Google only deliver to the holder of that signing identity, so your server can't send to
them with credentials of its own. Point it at the publisher's push gateway instead
([push-gateway.md](push-gateway.md)): it forwards your sealed wake-ups and sees only device tokens and
ciphertext. Every server does this, the publisher's own included.

```toml
[push.gateway]
url      = "https://push-gw.zeitgleich.de"   # the publisher's gateway for the official store apps
key_path = "/var/lib/teamcraft/push-gateway.key"   # created on first start; your identity at the gateway
```

The APNs and FCM rows above are for **self-built, self-signed apps**: with `[push.apns]` or `[push.fcm]`
set, the server sends to that provider directly and never to the gateway. Web Push and UnifiedPush
never use the gateway. Apps seal their device token for the gateway, bound to your server, so your
server never holds a usable token and can't be used to push to other servers' users; old apps that
don't seal yet need `accept_unbound_tokens = true` as a transition (docs/push-gateway.md §3a). Running a gateway is only for whoever publishes apps (its own repository, see [push-gateway.md](push-gateway.md)).

Common settings in `[push]`: `ttl_secs` (how long a push service holds a wake-up for an offline
device), `request_timeout_secs`, `max_concurrent_sends` (per pool: APNs/FCM, and Web Push/UnifiedPush),
`max_concurrent_sends_per_user` (Web Push/UnifiedPush sends one user may have in flight, so endpoints
that never answer only hold up their owner). `allow_private_endpoints` is for development only: it lets
Web Push and UnifiedPush endpoints point at private addresses and plain http, and allows plain http
APNs/FCM base URLs for local fakes; otherwise the server refuses to start with them.

### Web Push (VAPID)

```sh
openssl rand -base64 32 | tr '+/' '-_' | tr -d '='      # the private key, 32 random bytes
```

Set it as `TEAMCRAFT_PUSH__VAPID_PRIVATE_KEY` and `vapid_subject` to a `mailto:` or `https:` contact for
push services to reach you. At startup the server logs the matching public key; browsers get it from
`GET /api/v1/push/web-key`. Changing the key means browsers must subscribe again.

### APNs (iOS)

1. In the Apple Developer account: Keys → create a key with "Apple Push Notifications service (APNs)"
   enabled and download the `.p8` file once. Note the **Key ID**; the **Team ID** is shown under
   Membership; the **bundle ID** is the iOS app's.
2. Configure the server. The key is the file's contents, as `TEAMCRAFT_PUSH__APNS__KEY` or, for a
   mounted secret file, `key_path`:

   ```toml
   [push.apns]
   key_path = "/run/secrets/apns_key.p8"
   key_id = "ABC123DEFG"
   team_id = "DEF456GHIJ"
   bundle_id = "de.zeitgleich.teamcraft"
   use_sandbox = false
   ```

Messages go to the app's topic as alerts with `mutable-content` (the notification service extension
rewrites the generic `alert_text`); calls go to `<bundle_id>.voip` as VoIP pushes that are not stored.
One key serves production and sandbox; `use_sandbox = true` points the server at Apple's sandbox for
development builds (a token only works in the environment its build was signed for). A partial setup
(for example a key without a team ID) stops the server at startup.

### FCM (Android with Google services)

1. In the Firebase console: Project settings → Service accounts → "Generate new private key" (the
   account needs the "Firebase Cloud Messaging API Admin" role or `firebase.messaging`).
2. Give the JSON file to the server as `TEAMCRAFT_PUSH__FCM__SERVICE_ACCOUNT` (the contents) or
   `[push.fcm] service_account_path`.

The server trades the account's key for short-lived OAuth tokens itself and sends data-only
high-priority messages that carry only the sealed wake-up.

### UnifiedPush (Android without Google services)

Nothing to configure on the server: the app registers the endpoint of the user's distributor (for
example a self-hosted ntfy) with its Web Push keys, in the Web Push subscription format. The server
sends exactly as for Web Push: the sealed wake-up encrypted with RFC 8291 and posted to the endpoint,
with a VAPID token when `vapid_private_key` is set (distributors may ignore it). Endpoints must be
public https URLs. Set `[push.unified_push] is_enabled = false` to refuse it.

In the admin console, failures at Web Push and UnifiedPush endpoints are counted apart: users choose
those endpoints, so they never mark the provider as failing.

### Variables in the deployment repository

The Ansible deployment passes these on (secrets as podman secrets, the rest as variables):

| Deployment name              | Kind     | Becomes                                                                          |
|------------------------------|----------|----------------------------------------------------------------------------------|
| `vapid_private_key`          | secret   | `TEAMCRAFT_PUSH__VAPID_PRIVATE_KEY`                                              |
| `apns_key`                   | secret   | `TEAMCRAFT_PUSH__APNS__KEY` (the `.p8` text)                                     |
| `fcm_service_account`        | secret   | `TEAMCRAFT_PUSH__FCM__SERVICE_ACCOUNT`                                           |
| `teamcraft_apns_key_id`      | variable | `[push.apns] key_id`                                                             |
| `teamcraft_apns_team_id`     | variable | `[push.apns] team_id`                                                            |
| `teamcraft_apns_bundle_id`   | variable | `[push.apns] bundle_id`                                                          |
| `teamcraft_apns_use_sandbox` | variable | `[push.apns] use_sandbox` (default `false`)                                      |
| `teamcraft_push_gateway_url` | variable | `[push.gateway] url` (the publisher's push gateway, for the store apps)          |
| `push_gateway_key`           | secret   | `TEAMCRAFT_PUSH__GATEWAY__KEY` (generated: the server's identity at the gateway) |
| `teamcraft_unified_push`     | variable | `[push.unified_push] is_enabled` (default `true`)                                |

The `[push.apns]` block is only rendered when `apns_key` is a secret.

## Secrets and key rotation

| Secret                                | Where                                      | Rotating it                                                                       |
|---------------------------------------|--------------------------------------------|-----------------------------------------------------------------------------------|
| Call join token signing key (Ed25519) | `[calls] signing_key_path`; SFU public key | see below                                                                         |
| SFU registration token                | `[calls] sfu_registration_token`           | the SFU does not send reports yet; once it does, change both together             |
| Bot tokens                            | `POST /api/v1/admin/bots/{id}/token`       | the old token stops working immediately; update the bridge's configuration        |
| Guest token secret                    | `[meetings] guest_token_secret`            | invalidates open invite links and guest sessions; use between meetings            |
| Mail inbox token                      | `[meetings] inbox_token`                   | change on the server and the inbox handler together                               |
| VAPID key (Web Push)                  | `[push] vapid_private_key`                 | browsers must subscribe again; plan for a short push gap                          |
| APNs key (`.p8`)                      | `[push.apns] key`                          | create a new key at Apple, switch, then revoke the old one; no gap                |
| FCM service account key               | `[push.fcm] service_account`               | create a new key in Firebase, switch, then delete the old one; no gap             |
| OIDC server client secret             | `[oidc.sync] client_secret`                | rotate in the identity provider, then restart the server                          |
| Database password, S3 keys            | `[database]`, `[storage]`                  | change at the provider, then in the configuration, then restart                   |
| SMTP and CalDAV passwords             | Administration → Mail / Calendar           | change at the provider, then enter the new one in the console; no restart         |
| Settings key                          | `[secrets] key` or `key_path`              | a new key makes the stored SMTP and CalDAV passwords unreadable: enter them again |

Prefer environment variables or files for secrets (`TEAMCRAFT_...`), not the TOML file; never commit
them.

### Rotating the call signing key

The server signs short-lived join tokens (`join_token_ttl_secs`, 60 s by default) with an Ed25519
private key; the SFU verifies them with the matching public key and the issuer. Tokens are only
checked when somebody joins, so a rotation never disturbs calls that are running.

```sh
openssl genpkey -algorithm ed25519 -out calls-signing-new.pem
openssl pkey -in calls-signing-new.pem -pubout -out calls-signing-new.pub.pem
```

1. Put the new private key where `[calls] signing_key_path` points (a new file name is fine) and the
   new public key where the SFU reads `TEAMCRAFT_SFU_JOIN_TOKEN_PUBLIC_KEY`.
2. Restart the SFU, then the server, one right after the other. Joins that happen between the two
   restarts fail with an invalid-token error and succeed on retry; a token signed with the old key
   lives at most one TTL.
3. Remove the old key files.

The SFU accepts one public key at a time, so there is a short window with failing joins; do it when
few calls start. Rotate on a schedule that fits your policy and immediately after any suspected
exposure of the private key (it lets its holder join any call as any user).

## Smoke test of the images

`dev/smoke-images.sh` builds `teamcraft-server`, `teamcraft-sfu` and `teamcraft-bridge` and starts
them with rootless podman: non-root user, read-only root filesystem, no capabilities,
`no-new-privileges`. It checks that each starts, passes its `healthcheck`, answers on IPv4 and IPv6
where it has an endpoint, and stops cleanly on SIGTERM. Run it after changing a Containerfile or a
base image digest.
