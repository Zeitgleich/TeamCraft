# TeamCraft

Self-hosted, end-to-end encrypted business team chat with calls, meetings and an AI phone hotline,
written in Rust. License: AGPL-3.0-or-later.

- Architecture, threat model and data model: [ARCHITECTURE.md](ARCHITECTURE.md)
- Feature catalog and roadmap: [PLAN.md](PLAN.md)
- User and administrator guide: [docs/guide/index.md](docs/guide/index.md)
- Run your own server: [Zeitgleich/teamcraft-deploy](https://github.com/Zeitgleich/teamcraft-deploy)
  (Ansible playbook, rootless Podman) and [docs/operations.md](docs/operations.md)
- Releases and changes: [CHANGELOG.md](CHANGELOG.md), [docs/release.md](docs/release.md)
- Rules for contributors and coding agents: [AGENTS.md](AGENTS.md)

## Development setup

Requirements: current stable Rust (`rust-toolchain.toml`), Docker with Compose, and `python3` (only
for the token helper). For changing SQL you also need
`cargo install sqlx-cli --no-default-features --features rustls,postgres`.

### 1. Start the dev stack

```sh
docker compose -f docker-compose.dev.yml up -d
./dev/init-garage.sh        # Garage layout, bucket and access key (idempotent)
```

| Service  | Image                      | Address                                       |
|----------|----------------------------|-----------------------------------------------|
| Postgres | `postgres:18.6-alpine3.24` | `127.0.0.1:5432`                              |
| Keycloak | `keycloak/keycloak:26.8.1` | `http://127.0.0.1:8080`                       |
| Garage   | `dxflrs/garage:v2.4.1`     | S3 `http://127.0.0.1:3900`, admin API `:3903` |

All ports are bound to loopback on both IPv4 and IPv6 (`127.0.0.1` and `[::1]`). **Every credential below is public and for development only.**
`docker compose -f docker-compose.dev.yml down -v` throws away all dev state.

| Service  | Credentials                                                                  |
|----------|------------------------------------------------------------------------------|
| Postgres | user `teamcraft`, password `teamcraft-dev-only`, database `teamcraft`        |
| Keycloak | admin console `admin` / `admin-dev-only`                                     |
| Garage   | region `garage`, bucket `teamcraft-dev`, access key `GKdeadbeef0000000000000000`, secret key `deadbeef` repeated 8 times; admin token `teamcraft-dev-admin-token` |

**Keycloak** imports [dev/keycloak/teamcraft-dev-realm.json](dev/keycloak/teamcraft-dev-realm.json)
when the container is created (ARCHITECTURE.md §5.1.1):

| What                | Value                                                                     |
|---------------------|---------------------------------------------------------------------------|
| Realm               | `teamcraft-dev`, issuer `http://127.0.0.1:8080/realms/teamcraft-dev`       |
| Public client       | `teamcraft`: authorization code + PKCE (S256)                              |
| Redirect URIs       | `http://127.0.0.1/callback` (any port, RFC 8252), `http://[::1]/callback`, `http://localhost:8082/*`, `http://127.0.0.1:8082/*`, `com.example.teamcraft:/oauth/callback` |
| Confidential client | `teamcraft-server` with service account, secret `teamcraft-server-dev-secret` |
| Client roles        | `owner`, `admin`, `member`, `guest` on `teamcraft`                         |

| User    | Password         | Role in TeamCraft                                  |
|---------|------------------|----------------------------------------------------|
| `alice` | `alice-dev-pass` | owner                                              |
| `bob`   | `bob-dev-pass`   | admin                                              |
| `carol` | `carol-dev-pass` | member, through the Keycloak group `/project-a`    |
| `dave`  | `dave-dev-pass`  | guest                                              |
| `erin`  | `erin-dev-pass`  | guest and member roles: the highest wins (member)  |
| `frank` | `frank-dev-pass` | no TeamCraft role: the server rejects his tokens   |

**Changing the realm.** Keycloak imports the realm file only when the realm doesn't exist yet, so after
editing `dev/keycloak/teamcraft-dev-realm.json` recreate the container (all Keycloak dev state is reset, the
realm is imported again):

```sh
docker compose -f docker-compose.dev.yml up -d --force-recreate keycloak
```

Users who were created in TeamCraft before get new Keycloak IDs then; use a fresh database or let them be
deactivated by the sync. `./dev/verify-realm.py` checks the identity key attestation setup: the app client's
tokens work with the account API (audience `account`), a user can write the profile attribute `tc_identity_key`
(visible to admins, not editable by the service account) and gets it back as an ID token claim
(`./dev/dev-token.py alice id`).

### 2. Run the server

```sh
cp teamcraft.example.toml teamcraft.toml     # the defaults match the dev stack
# Keycloak already uses port 8080 on loopback, so the dev server listens on 8081 (dual stack).
TEAMCRAFT_SERVER__LISTEN_ADDR="[::]:8081" cargo run -p teamcraft-server -- --config teamcraft.toml
```

The server applies the migrations, creates the monthly message partitions and listens on
port 8081 on both IPv4 and IPv6 (the default `listen_addr` is `[::]:8080`). Try it:

```sh
curl http://127.0.0.1:8081/healthz
curl http://[::1]:8081/healthz
curl -H "Authorization: Bearer $(./dev/dev-token.py alice)" http://127.0.0.1:8081/api/v1/me
```

`dev/dev-token.py` runs the real authorization-code + PKCE flow for a dev user and prints the
access token. [teamcraft.example.toml](teamcraft.example.toml) documents every option; each can be
overridden by a `TEAMCRAFT_<SECTION>__<KEY>` environment variable.

### API

REST under `/api/v1` (OpenAPI document at `/api/v1/openapi.json`, generated from the handlers) and the realtime
WebSocket `/api/v1/gateway` ([docs/gateway.md](docs/gateway.md)). Messages are opaque ciphertext blobs the
server stores and fans out; it never sees plaintext. Permissions are decided in one place,
`teamcraft_proto::is_allowed` (PLAN.md §13).

E2EE support (milestone 2): identity keys with OIDC attestation, device certificates, MLS key packages
(atomic claiming), the MLS delivery queue with first-commit-wins epochs, versioned key bag storage, push
registration, the device-linking relay and presigned attachment upload/download. The server stores and
orders opaque bytes; it makes no trust decisions. Attachments need the `[storage]` section (the dev
Garage from `./dev/init-garage.sh` works as configured in `teamcraft.example.toml`).

Deprovisioning: Keycloak ends sessions through back-channel logout (`POST /api/v1/auth/backchannel-logout`,
registered at the `teamcraft` client in the dev realm; Keycloak reaches the host as `host.docker.internal:8081`),
and `[oidc.sync]` polls the admin API with the service account (disabled users, role changes). Enable it with
`TEAMCRAFT_OIDC__SYNC__IS_ENABLED=true TEAMCRAFT_OIDC__SYNC__CLIENT_SECRET=teamcraft-server-dev-secret`.

Push: messages wake devices with an encrypted payload sealed with the device's push key (e2ee.md §7). Web Push
(VAPID, RFC 8291) needs `[push] vapid_private_key`; APNs (`[push.apns]`), FCM (`[push.fcm]`) and UnifiedPush are implemented
behind the same trait and on once configured (docs/operations.md). Routing honours mute, mentions, do-not-disturb and active devices.

### Tests against the whole stack

`scripts/e2e.sh` starts the dev stack, builds the server, SFU and bridge and runs the end-to-end suite
(real clients, calls, meetings with mail, the web client in headless Chromium): see
[tests/e2e/README.md](tests/e2e/README.md).

### 3. Checks before committing

```sh
cargo fmt --all
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
cargo deny check
```

`--all-features` includes the `integration` tests of `teamcraft-db` and `teamcraft-server` (REST and gateway
against the real router). They create throwaway databases on the server named by `DATABASE_URL` and skip
without it:

```sh
export DATABASE_URL=postgres://teamcraft:teamcraft-dev-only@127.0.0.1:5432/teamcraft
```

Plain `cargo test --workspace` runs without any services.

### Changing SQL

SQL lives only in `crates/teamcraft-db` and is checked at compile time. The offline query data in
`.sqlx/` is committed, so builds work without a database. After changing a query or adding a
migration (migrations are forward-only and never edited after merge):

```sh
export DATABASE_URL=postgres://teamcraft:teamcraft-dev-only@127.0.0.1:5432/teamcraft
sqlx migrate run --source crates/teamcraft-db/migrations
cargo sqlx prepare --workspace -- -p teamcraft-db -p teamcraft-server --all-targets --all-features
```

## License

TeamCraft is licensed under the GNU Affero General Public License v3.0 or later, see
[LICENSE](LICENSE). Third-party material is listed in [NOTICE](NOTICE). Security reports:
[SECURITY.md](SECURITY.md). Contributing: [CONTRIBUTING.md](CONTRIBUTING.md), [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md).
