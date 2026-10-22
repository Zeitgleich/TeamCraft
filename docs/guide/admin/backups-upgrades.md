# Backups and upgrades

The full runbook is [docs/operations.md](../../operations.md). The short version:

## What to back up

| Piece              | Back up                              | Notes                                                         |
|--------------------|--------------------------------------|---------------------------------------------------------------|
| PostgreSQL         | Yes: base backup + WAL, or `pg_dump` | Holds the ciphertext. **Losing it loses everybody's history** |
| S3 bucket          | Yes: mirror or versioning            | Attachments and search index segments                         |
| `teamcraft-bridge` | Its database file                    | Or re-enroll with the passphrase                              |
| Server, SFU        | Configuration and secrets only       | Keep the same secrets after a restore                         |
| Identity provider  | Its own backup                       | Users, roles, sessions                                        |

Retention deletes data for good; a backup is the only way back, and it must not outlive your
retention promise.

## Restore

Restore Postgres and the bucket from about the same time, start the server with the same
configuration and secrets, bring up the SFU and bridges, and let clients reconnect. Test it
regularly with a scratch database and `/readyz`.

## Upgrade

1. Take a database backup (migrations are forward-only, there is no downgrade).
2. Pull new images by digest, never `latest`.
3. Order: server (one instance first), then SFU, then bridges.
4. Read the release notes: the server **rejects unknown configuration keys**, so stale keys fail at
   startup.

Clients resume within `[gateway] resume_window_secs` (120 s), otherwise they sync fully. User
devices keep their local history even if the server is lost, but cannot sync it back without a
backup.

## User key recovery is separate

The server backup does not restore users' keys. Users recover with a linked device, their recovery
passphrase, or (if enabled) an organization recovery ([Security policy](security-policy.md)).
