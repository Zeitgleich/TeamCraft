# Installation overview

TeamCraft is three binaries plus services they depend on. Container images, health checks and
hardening are in [containers/README.md](../../../containers/README.md); day-two operations are in
[docs/operations.md](../../operations.md).

## Pieces

| Piece              | Required  | Role                                                                               |
|--------------------|-----------|------------------------------------------------------------------------------------|
| `teamcraft-server` | yes       | REST API, WebSocket gateway, serves the web client. Stores only ciphertext         |
| PostgreSQL         | yes       | Accounts, structure, ciphertext of messages, audit log. Migrates on startup        |
| Identity provider  | yes       | OIDC login and roles. Keycloak is the reference ([Keycloak setup](keycloak.md))    |
| Reverse proxy      | yes       | TLS, WebSocket upgrades, rate limits on top of the server's                        |
| S3 bucket          | for files | Encrypted attachments and search index segments (Garage, MinIO, any S3 API)        |
| `teamcraft-sfu`    | for calls | Media forwarding over QUIC/WebTransport ([Calls and SFU](calls-sfu.md))            |
| CalDAV + SMTP      | optional  | Meeting events and invitation e-mail ([Meetings](meetings.md))                     |
| `teamcraft-bridge` | optional  | Logs and webhooks into encrypted channels ([Integrations](integrations-bridge.md)) |

The server runs on an untrusted host by design: a stolen database or bucket gives metadata but no
message content.

## Steps

1. **Postgres:** an empty database and a user. The server creates the schema and the monthly message
   partitions (`[server] partitions_ahead_months`).
2. **Identity provider:** create the client, roles, mapper and user-profile attribute
   ([Keycloak setup](keycloak.md)).
3. **Configuration:** copy [teamcraft.example.toml](../../../teamcraft.example.toml), set at least
   `[database] url` and `[oidc] issuer`; put secrets in `TEAMCRAFT_*` environment variables
   ([Configuration](configuration.md)).
4. **Storage:** create the bucket and fill `[storage]`. Without it attachments answer 503.
5. **Run the server** (`teamcraft-server --config teamcraft.toml`) behind a reverse proxy that
   terminates TLS and passes WebSocket upgrades on `/api/v1/gateway` and `/api/v1/rendezvous`.
   Check `GET /healthz` and `GET /readyz`.
6. **Web client:** unpack the release's signed `teamcraft-web-<version>.tar.gz` (or build
   `crates/teamcraft-ui` with `trunk build --release`) and set `[web] static_dir`. Register the
   server's origin as a redirect URI of the OIDC client.
7. **Desktop and mobile apps** ask for the server address on first start and take the sign-in
   settings from `GET /api/v1/server-info`; they use the same OIDC client
   (`de.zeitgleich.teamcraft:/oauth/callback` and loopback redirects are in the reference realm).
8. **First owner:** give yourself the `owner` client role, sign in (web or desktop app), create your
   keys, then open Administration > Security. The first policy save founds the signed admin roster
   from the identity provider's `owner` and `admin` roles ([Security policy](security-policy.md)).
   In the browser, store the recovery passphrase right away and link a desktop or mobile device
   soon: a browser may clear site data, and with it the founding owner's keys.
9. Optional: calls, meetings, push, bridges, using the pages linked above.

## Deployment recipes

This repository contains images and a development stack
(`docker-compose.dev.yml`, see [README.md](../../../README.md)), not a production recipe. Production
runs rootless Podman with Quadlet units kept in a separate private deployment repository. Any
orchestrator works: images are distroless, run as UID 65532 with a read-only root filesystem.

## Dual stack

Everything listens on IPv4 and IPv6 (`[::]`). Publish both families at the proxy, and open `4433/udp`
for the SFU.
