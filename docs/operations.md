# Operations

What an operator needs to run TeamCraft: a quickstart for a fresh server, the pieces, health checks,
backups, upgrades and secret rotation. Configuration reference: `teamcraft.example.toml`; images:
`containers/README.md`; releases: `docs/release.md`.

## Quickstart: a fresh server in 15 minutes

The reference deployment is the Ansible playbook in [`deploy/`](../deploy/README.md), which installs
TeamCraft on one Debian VPS with **rootless Podman and Quadlet**, each service under its own
unprivileged user:

| Service             | User        | Listens on                                                |
|---------------------|-------------|-----------------------------------------------------------|
| Caddy (TLS, HTTP/3) | `tc-proxy`  | `80/tcp`, `443/tcp+udp`, IPv4 and IPv6                    |
| `teamcraft-server`  | `tc-server` | `127.0.0.1:8080` (behind Caddy)                           |
| `teamcraft-sfu`     | `tc-sfu`    | `4433/udp` (QUIC) IPv4 and IPv6; `127.0.0.1:8081` control |
| PostgreSQL          | `tc-db`     | `127.0.0.1:5432`                                          |

Not on the VPS: the OIDC provider (e.g. Keycloak) and the S3 storage.

### What you need

- A VPS with **Debian 13 (trixie)**, 2 vCPU, 4 GB RAM, 20 GB disk, a public IPv4 **and** IPv6 address,
  SSH access as a user with `sudo`.
- Two DNS names with A and AAAA records pointing at it: `chat.example.com` (API, web client) and
  `sfu.chat.example.com` (calls).
- Firewall open for `80/tcp`, `443/tcp`, `443/udp`, `4433/udp`, both address families. Nothing else.
- An **OIDC provider** with a realm for TeamCraft: a public client `teamcraft` (code flow with PKCE;
  redirect URIs of the apps), client roles `owner`, `admin`, `member`, `guest` and users who have
  one of them. The Keycloak realm in `dev/keycloak/teamcraft-dev-realm.json` is a complete example.
- An **S3 bucket** with an access key; clients upload to it directly, so it must be reachable from
  them and allow CORS from `https://chat.example.com` for the web client.
- On your machine: Ansible ≥ 2.16, `sops` and `age`, and the collections of
  `ansible/requirements.yml`.

### Steps

1. **Inventory.** Copy `deploy/ansible/inventories/example` to a directory of your own, outside this
   checkout (ideally a private repository, `deploy/README.md`, "Your own inventory"), and set the host
   (`hosts.yml`) and `group_vars/all.yml`: the two names, an ACME e-mail, the OIDC issuer, the S3
   endpoint, region and bucket, and the image digests from `images.txt` of the release.
2. **Secrets.** Generate them (commands in `deploy/secrets/README.md`), put your age key into the
   inventory's `.sops.yaml` and write them into the encrypted file next to the inventory:

   ```sh
   cd ~/my-teamcraft-instance && sops secrets.enc.yaml
   ```

   Needed: `postgres_password`, `s3_access_key`, `s3_secret_key`, `sfu_registration_token`,
   `call_signing_key` + `call_signing_public_key`, `meetings_guest_token_secret`.
3. **Deploy.**

   ```sh
   ansible-galaxy collection install -r deploy/ansible/requirements.yml
   ansible-playbook -i ~/my-teamcraft-instance deploy/ansible/site.yml
   ```

   The first run installs Podman, creates the four service users, pulls the images, gets the
   certificates from Let's Encrypt and starts everything (about 5 minutes in the dry run, most of it
   package installs and image pulls). A second run must report `changed=0`. The SFU certificate is
   copied from Caddy hourly; the SFU restarts by itself when it changes (renewals).
4. **Check.**

   ```sh
   curl -fsS https://chat.example.com/readyz                    # server and database
   curl -fsS --http3-only https://chat.example.com/healthz      # HTTP/3 through Caddy
   curl -sS -o /dev/null -w '%{http_version}\n' --http3-only https://sfu.chat.example.com:4433/  # 3: SFU reachable
   curl -s -o /dev/null -w '%{http_code}\n' -X POST https://chat.example.com/api/internal/v1/calls/participants/joined  # 404
   ```

   Repeat with `-4` and `-6`. On the server, `sudo -u tc-server XDG_RUNTIME_DIR=/run/user/$(id -u
   tc-server) systemctl --user status teamcraft-server` and `journalctl --user` work per service user.
5. **Sign in.** Point the desktop app at `https://chat.example.com` and sign in with an `owner`
   user. The first device of every user creates their keys; set up the recovery passphrase when
   asked. With the web client archive deployed (`teamcraft_web_archive`), the same works in the
   browser at `https://chat.example.com`.

Then read "Backup and restore" below and set up the database backup before inviting people.

### Trying it locally first

`deploy/test/dry-run.sh all` runs the whole playbook against a throwaway
Debian trixie container (systemd, rootless Podman inside, its own dual-stack Docker network), with
images built from this repository, generated secrets, Caddy's internal CA and the dev Keycloak and
Garage of `docker-compose.dev.yml`. It then checks the health endpoints over IPv4 and IPv6, that
`/api/internal/` is refused, the SFU's QUIC handshake over both families, and sends encrypted messages
between two dev users through the deployment with the end-to-end harness
(`tests/e2e/tests/suite/deployed.rs`, `E2E_DEPLOYED_URL`; `SSL_CERT_FILE` points the clients at Caddy's
local root). `deploy/test/dry-run.sh down` removes it again.

Pitfalls the dry run found, worth knowing for any other way of running the images:

- The server image sets `TEAMCRAFT_SERVER__LISTEN_ADDR=[::]:8080`, and environment variables win over
  the TOML file. With host networking, set the variable yourself (e.g. `127.0.0.1:8080`), or the server
  is reachable on every address of the host, bypassing the proxy.
- The images are distroless: health checks must use the exec form
  (`HealthCmd=["/usr/local/bin/teamcraft-server", "healthcheck", "--ready"]`), there is no shell.
- Rootless containers run as a subordinate uid: bind-mounted configuration files must be readable by
  others (they hold no secrets; secrets go in as podman secrets).

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

Attachments and index segments are immutable objects named by random IDs, so a plain
`rclone sync`/`mc mirror` to a second bucket, or the provider's bucket versioning and replication,
works. The database references objects by key; a restore needs the database and the bucket from
about the same time. Objects without a database row are garbage (an upload that never completed) and
harmless; rows without an object show up as failed downloads.

### Restore order after a total loss

1. Postgres from the latest backup, S3 bucket from its copy.
2. Server and SFU with the same configuration and **the same secrets** (see below), identity
   provider reachable.
3. Bridges: restore their database file (it holds the bot's keys) or re-enroll them with their
   passphrase (the bot's backup key is on the server, so a lost bridge database is recoverable).
4. Clients reconnect and resume; devices whose rows were lost re-register.

## Upgrades

- Images are pinned by digest in the deployment; build or pull the new ones, never `latest`.
- Migrations are forward-only and run automatically when the server starts. **Take a database backup
  first.** There is no downgrade: to go back, restore the backup and start the old version.
- Order: database backup, new `teamcraft-server` (one instance first, then the rest; instances may
  run two adjacent versions side by side while rolling), then `teamcraft-sfu`, then bridges. The
  SFU verifies join tokens the server signs; the token format is versioned with the server.
- A restarting server drops WebSocket connections; clients resume with their session and sequence
  number within `[gateway] resume_window_secs`, otherwise they run a full sync. Calls in progress
  live in the SFU and survive a server restart (the server's call roster is in memory and starts empty;
  it is filled by the SFU's participant reports once the SFU sends them).
- Check `docs/` and the release notes for configuration changes before upgrading. The server rejects
  unknown configuration keys at startup, so a stale key fails fast instead of being ignored.

## Secrets and key rotation

| Secret                                      | Where                                      | Rotating it                                                                |
|---------------------------------------------|--------------------------------------------|----------------------------------------------------------------------------|
| Call join token signing key (Ed25519)       | `[calls] signing_key_path`; SFU public key | see below                                                                  |
| SFU registration token                      | `[calls] sfu_registration_token`           | the SFU does not send reports yet; once it does, change both together      |
| Bot tokens                                  | `POST /api/v1/admin/bots/{id}/token`       | the old token stops working immediately; update the bridge's configuration |
| Guest token secret                          | `[meetings] guest_token_secret`            | invalidates open invite links and guest sessions; use between meetings     |
| Mail inbox token                            | `[meetings] inbox_token`                   | change on the server and the inbox handler together                        |
| VAPID key (Web Push)                        | `[push] vapid_private_key`                 | browsers must subscribe again; plan for a short push gap                   |
| OIDC server client secret                   | `[oidc.sync] client_secret`                | rotate in the identity provider, then restart the server                   |
| Database password, S3 keys, SMTP and CalDAV | `[database]`, `[storage]`, `[meetings.*]`  | change at the provider, then in the configuration, then restart            |

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
