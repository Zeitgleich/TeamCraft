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

- `GET /healthz` is liveness, `GET /readyz` also checks the database and answers 503 once a shutdown
  has begun. The images ship
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
  from clients (QUIC); its health endpoint (`8081/tcp`) is for the orchestrator only. Calls don't
  work on networks that block outbound UDP (some corporate, hotel and guest networks); there is no
  TCP fallback yet (ARCHITECTURE.md §6.6), so name the port in the instructions for users.
- Keep the SFU's UDP port out of anything that tracks UDP "connections" by address and port (a
  load balancer without QUIC connection-ID routing, a strict stateful firewall): when a client
  changes networks its QUIC connection continues from a new address (connection migration,
  ARCHITECTURE.md §6.5), and such a box would drop or misroute those packets. A plain DNAT/port
  forward to one SFU is fine.
- A participant whose connection is lost keeps their place in the call for
  `TEAMCRAFT_SFU_RECONNECT_GRACE_SECS` (default 30) while their app rejoins; a connection is lost
  after `TEAMCRAFT_SFU_IDLE_TIMEOUT_SECS` (default 10) without packets. Longer grace periods cover
  longer outages (trains, lifts) but keep a departed participant listed as "reconnecting" longer.
- Restarting or redeploying the SFU doesn't end running calls. On SIGTERM it closes every call
  connection as "going away", waits up to `TEAMCRAFT_SFU_SHUTDOWN_DRAIN_SECS` (default 2) for the
  closes to go out, and exits. Apps rejoin with a fresh join token, spread over about a second, and
  keep trying with back-off for the reconnect grace period they learnt (default 30 s): bring the
  new SFU up at the same address within that time and the calls continue after a short gap, with a
  fresh DAVE group. A crash works the same way, only the apps notice the dead connection after
  about 5 s instead of at once. Rooms, locks and host removals live in the SFU's memory and start
  fresh; the server's meeting admission still applies to every rejoin. Desktop and mobile apps of
  version 1.0.2 and older end the call on "going away" (a browser tab rejoins).

### Company proxies, TLS inspection and captive portals (clients)

How the apps behave on networks between the user and the server (issue #157):

| Platform | Proxy                                                                                    | Certificate authorities                                                                                              |
|----------|------------------------------------------------------------------------------------------|----------------------------------------------------------------------------------------------------------------------|
| Desktop  | `HTTPS_PROXY` / `HTTP_PROXY` / `ALL_PROXY` / `NO_PROXY`, then macOS and Windows settings | The operating system's store (Keychain and MDM profiles, Windows store and group policy, the Linux system store)     |
| iOS      | The system's HTTP(S) proxy (Wi-Fi settings, MDM profile)                                 | The system's trust settings, including profiles the MDM installed                                                    |
| Android  | The network's HTTP proxy (`LinkProperties`)                                              | The system CAs; user and MDM-installed CAs only with the managed configuration `trust_user_certificates` set to true |
| Web      | The browser's                                                                            | The browser's                                                                                                        |

- REST and the WebSockets (gateway, device-link relay) use the same proxy; the WebSockets tunnel with
  HTTP `CONNECT`, including `user:password@` from a proxy URL. REST also takes `https://` proxies;
  the WebSockets tunnel through `http://` ones only and connect directly otherwise. SOCKS proxies
  are not used. PAC scripts are never evaluated: on a network that only has one, the apps connect
  directly. On Linux desktops only the environment variables count (no GNOME/KDE settings). A proxy
  change on the desktop applies after a restart of the app; on mobile it applies when the network or
  its proxy changes, and the iOS notification extension reads it whenever it opens the core.
- Android ignores CAs that users install themselves, as Android apps do by default, so a CA someone
  talked the user into installing reads nothing. An organization that inspects TLS on managed
  devices turns on `trust_user_certificates` in its MDM's managed configuration for the app; it
  applies at the next app start.
- Calls (QUIC over UDP to the SFU) never go through an HTTP proxy.
- A captive portal (a login page answering instead of the server: HTML where the API answers JSON, a
  redirect to another site, HTTP 511) and a refused certificate are shown in the connection banner
  as such ("sign in to this network", "this network may inspect encrypted traffic"); queued messages
  and uploads wait and are sent once the server answers.
- Happy Eyeballs (RFC 8305, issue #420): when the host's IPv6 route swallows packets, the apps fall
  back to IPv4 instead of staying "unreachable". REST (hyper-util, 300 ms), the gateway and relay
  WebSockets and the QUIC handshake to the SFU for calls (250 ms) race the address families. The
  Android app's own few requests (sign-in, token refresh, server check) use the platform's
  `HttpURLConnection`, which tries the next address only after the connect timeout (15 s, #433).

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

## Owners

Owners (the role `owner` in the identity provider, `just ensure-admin` above) sign the workspace's
admin roster and policy with their devices (admin.md §2.3). The server can't do it for them, and
there is no operator override: an operator with full access to the server and database still can't
make someone an owner.

- Make sure there are **at least two owners** with a set-up device and recovery passphrase. Give the
  second person the owner role in the identity provider before the first owner signs the first
  policy: the first roster takes every owner and admin the server lists then. The console
  (Administration → Security → Owners) warns when the signed roster has fewer owners than
  recommended and shows when each owner last signed.
- **Handing over:** the successor goes into the signed roster as owner first, the old owner is
  removed afterwards. A roster without an owner is refused by the apps and the server
  (`last_owner`). Both steps are made in the console (Administration → Security → Owners) by an
  owner, who signs each change on their device. Role changes in the identity provider show up there
  as a difference until an owner signs them.
- An owner's app re-signs the roster every 15 days while it is in use. If no owner opens TeamCraft
  for 30 days, member clients stop accepting the admins until an owner does; the console warns a
  week ahead.
- **If every owner is lost** (left, or lost all devices and the recovery passphrase), the workspace
  can't be administered any more: chat keeps working, admin actions don't. Restoring a backup does
  not help, and a new first owner is refused by every client. A recovery procedure is not
  available yet (issue #188); until then the only way out is a new workspace (fresh database, new
  enrollment of every user).

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
- On SIGTERM the server drains: `/readyz` answers 503, every gateway and device-linking socket is
  closed with code 1012 ("service restart"), and the process waits up to `[gateway]
  drain_timeout_secs` (default 10) for the sockets to close before it exits. Give the container a
  stop grace period above that. The new server forgets the sessions: every client reconnects (with
  jitter) and runs a full sync, so expect a burst of API traffic right after the restart. A reverse proxy in front must keep idle WebSockets open for at least 60 s (clients send a
  heartbeat every 25 s). Calls in progress
  live in the SFU and survive a server restart (the server's call roster is in memory and starts empty;
  it is filled by the SFU's participant reports once the SFU sends them).
- Check `docs/` and the release notes for configuration changes before upgrading. The server rejects
  unknown configuration keys at startup, so a stale key fails fast instead of being ignored.

## Server address in the apps

The desktop, Android and iOS apps have no server built in. On first start they ask for the server
address; users type `team.example.com`, the full URL, or paste any link into the server (an invitation,
a message link). The app shows the server's logo, name, host and identity provider for confirmation
and then signs in with the settings the server announces. "Change server" on the sign-in screen goes
back to the entry. When a host name does not resolve, the desktop app says so and offers "Check
similar addresses": only when the user presses it does it look up the same name under a few other
common top-level domains (`.com` typed for `.de`) and offer the first one that exists as "Did you
mean …?". Taking it only fills in the field; the app never connects on its own. The web client needs
none of this: it signs in to the server that served it.

After the identity provider, the browser tab of the desktop sign-in shows a page from the app's
loopback listener in the app's language and theme, with the server's logo and name. It is answered
only once the tokens are in, so it says whether the sign-in really worked; it is self-contained
(content security policy `default-src 'none'`, no scripts) and never shows the redirect's
parameters.

What the server announces, public and cacheable, at `GET /api/v1/server-info`:

| Field                            | Source                                                                    |
|----------------------------------|---------------------------------------------------------------------------|
| `sign_in.issuer`                 | `[oidc] issuer`; apps require `https` (plain `http` only on loopback)     |
| `sign_in.client_id`              | `[oidc] client_id`, the public client of the apps                         |
| `sign_in.scopes`                 | `[oidc] app_scopes`, default `openid profile email`                       |
| `sign_in.redirect_uris`          | `[oidc] app_redirect_uris`, optional (see below)                          |
| `sign_in.has_local_accounts`     | Whether local guest accounts are enabled                                  |
| `brand`                          | The brand of the requested host name, as `GET /api/v1/brand`              |
| `server_version`, `minimum_client_version` | As `GET /api/v1/client-compatibility`                           |

Nothing in it is secret. Register these redirect URIs for `client_id` at the identity provider:

| App            | Redirect URI                                                     |
|----------------|------------------------------------------------------------------|
| Desktop        | `http://127.0.0.1/callback` (any port: loopback flow, RFC 8252)  |
| Android, iOS   | `de.zeitgleich.teamcraft:/oauth/callback`                        |
| Web            | `https://<host>/` of each host name                              |

In Keycloak, `http://127.0.0.1/*` as a valid redirect URI covers the desktop app's changing port. When
`app_redirect_uris` lists them, an app built with another redirect URI (a self-build with its own app
ID) tells its user that the server is not set up for it instead of failing at the provider.

The apps reject a server that is not a TeamCraft server, that answers with an HTML page or a redirect
(a captive portal), whose version is too old (or for which the app is too old), or whose issuer is not
`https`. Tokens and local data are kept per server origin; an app never switches to a server because
a server told it to.

**Preconfigured server (managed devices).** Companies can set the server so users don't type it, and
optionally lock the apps to it:

| Platform | Where                                                                                                                                          |
|----------|------------------------------------------------------------------------------------------------------------------------------------------------|
| Desktop  | `desktop.json` with `{"server_url": "team.example.com", "lock_server": true}` in `/etc/teamcraft/` (Linux) or `/Library/Application Support/TeamCraft/` (macOS), owned by root and writable only by root (else ignored); or the environment variable `TEAMCRAFT_SERVER_URL` (not locked). Windows: the policy registry key follows (#220) |
| Android  | Managed configuration (Android Enterprise / MDM app restrictions): `server_url`, `lock_server`                                                  |
| iOS      | Managed app configuration (`com.apple.configuration.managed`): `server_url`, `lock_server`                                                      |

With a preconfigured server the app checks it on first start and goes straight to the confirmation.
`lock_server` hides "Change server" and drops an account of another server.

**The sign-in page.** Every client renders the same page from `server-info`: a prominent "Sign in
with <provider>" button, and the e-mail and password form of local accounts inline, in the order the
admin console sets (Admin → Settings → Sign-in page: SSO and local, local first, SSO only, local
only; help text and help link). The provider name and its icon are brand settings (Admin → Brands:
"Sign-in provider" and the "Sign-in provider icon" image). Local accounts need `[local_accounts]` with
a session secret in the configuration; the console switches them off and on (Settings → Local
accounts).

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
