# Container images

OCI images of the server binaries and the integration bridge, built from the workspace root:

| Image              | Containerfile                    | Listens on                             | Health check                   |
|--------------------|----------------------------------|----------------------------------------|--------------------------------|
| `teamcraft-server` | `teamcraft-server.Containerfile` | `8080/tcp` (HTTP, `[::]` dual stack)   | `teamcraft-server healthcheck` |
| `teamcraft-sfu`    | `teamcraft-sfu.Containerfile`    | `4433/udp` (QUIC), `8081/tcp` (health) | `teamcraft-sfu healthcheck`    |
| `teamcraft-bridge` | `teamcraft-bridge.Containerfile` | `8088/tcp` (webhooks, logs)            | `teamcraft-bridge healthcheck` |

```sh
podman build --format docker -f containers/teamcraft-server.Containerfile -t teamcraft-server .
podman build --format docker -f containers/teamcraft-sfu.Containerfile    -t teamcraft-sfu .
podman build --format docker -f containers/teamcraft-bridge.Containerfile -t teamcraft-bridge .
```

`--format docker` keeps the `HEALTHCHECK` instruction; the OCI format has none. Orchestrators
(Quadlet `HealthCmd=`, Kubernetes probes) should run the same `healthcheck` subcommand themselves:
the images are distroless and contain no `curl` or shell.

## Properties

- Multi-stage: `rust:<version>-trixie` builds, the final stage is `gcr.io/distroless/cc-debian13:nonroot`
  (Debian 13 glibc, libgcc_s and OpenSSL 3, no shell, no package manager), 35-50 MB. Build and
  runtime come from the same Debian release, so the bridge's SQLCipher links the libcrypto it finds.
- Both base images are **pinned by digest** in the Containerfiles. Bump them deliberately
  with the multi-arch index digest (`skopeo inspect --raw docker://<image> | sha256sum`, or
  `docker buildx imagetools inspect <image>`), never a single-platform one.
- Run as UID/GID **65532**, write nothing to disk, so `--read-only`, `--cap-drop all` and
  `--security-opt no-new-privileges` work. Nothing needs a tmpfs.
- Build uses `--locked`, `SQLX_OFFLINE=true` (the committed `.sqlx/` data) and ignores
  `rust-toolchain.toml` (the wasm target is not needed for the server binaries).
- The images carry no configuration and no secrets. Pass them at run time.

## Configuration

`teamcraft-server`: a TOML file (`--config <path>` or `TEAMCRAFT_CONFIG`, see
`teamcraft.example.toml`) and/or `TEAMCRAFT_*` variables (`TEAMCRAFT_DATABASE__URL`, ...). The image
sets `TEAMCRAFT_SERVER__LISTEN_ADDR=[::]:8080`. `healthcheck` reads the same configuration to find
the listen address, so give it the same environment and mounts as the server (a Quadlet unit does
this automatically); `healthcheck --ready` probes `/readyz` (database reachable) instead of `/healthz`.

`teamcraft-bridge`: a TOML file (`--config`, `TEAMCRAFT_BRIDGE_CONFIG`; `crates/teamcraft-bridge/teamcraft-bridge.example.toml`)
plus `TEAMCRAFT_BRIDGE_*` variables. The image listens on `[::]:8088` inside the container (publish it on
loopback only), keeps its database in the volume `/var/lib/teamcraft-bridge` and the files of webhooks,
while they upload, in the volume `/var/spool/teamcraft-bridge`.

`teamcraft-sfu`: environment variables, documented in `crates/teamcraft-sfu/src/main.rs`
(`TEAMCRAFT_SFU_BIND`, `TEAMCRAFT_SFU_CERT`/`_KEY`, `TEAMCRAFT_SFU_JOIN_TOKEN_PUBLIC_KEY`/`_ISSUER`,
`TEAMCRAFT_SFU_HEALTH_BIND`). Without a certificate it generates a short-lived self-signed one in
memory, which only suits tests.

## Smoke test

`dev/smoke-images.sh [server|sfu]` builds the images and starts them with rootless podman against the
dev stack (`docker compose -f docker-compose.dev.yml up -d`): read-only root filesystem, no
capabilities, non-root, health endpoints on IPv4 and IPv6, the `healthcheck` subcommand, and a clean exit
on SIGTERM.
