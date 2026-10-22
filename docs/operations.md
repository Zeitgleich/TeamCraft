# Operations

What an operator needs to run TeamCraft: the pieces, health checks, backups, upgrades and secret
rotation. Configuration reference: `teamcraft.example.toml`; images: `containers/README.md`.

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
