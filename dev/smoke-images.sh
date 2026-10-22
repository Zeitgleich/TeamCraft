#!/usr/bin/env bash
# Builds the OCI images and smoke-tests them with rootless podman against the dev stack.
#
#   docker compose -f docker-compose.dev.yml up -d   # Postgres on 127.0.0.1:5432
#   dev/smoke-images.sh [server|sfu|bridge]           # default: all
#
# Checks per image: starts non-root with a read-only root filesystem and all capabilities dropped,
# becomes healthy (`healthcheck` subcommand), serves its endpoint and shuts down on SIGTERM.
# The server smoke test creates and drops the throwaway database `teamcraft_smoke`.
set -euo pipefail

cd "$(dirname "$0")/.."
engine=${CONTAINER_ENGINE:-podman}
which_images=${1:-all}
pg_admin=${SMOKE_PG_ADMIN_URL:-postgres://teamcraft:teamcraft-dev-only@127.0.0.1:5432/postgres}
pg_smoke=${pg_admin%/*}/teamcraft_smoke
server_port=18080
sfu_health_port=18081

log() { printf '== %s\n' "$*"; }
fail() { printf 'FAILED: %s\n' "$*" >&2; exit 1; }

wait_for() { # description, command...
  local what=$1; shift
  for _ in $(seq 1 60); do
    if "$@" >/dev/null 2>&1; then return 0; fi
    sleep 1
  done
  fail "timed out waiting for $what"
}

run_hardened() { # name, image, extra args...
  local name=$1 image=$2; shift 2
  "$engine" run -d --name "$name" --network host --read-only --cap-drop all \
    --security-opt no-new-privileges "$@" "$image" >/dev/null
}

cleanup() {
  "$engine" rm -f teamcraft-smoke-server teamcraft-smoke-sfu >/dev/null 2>&1 || true
  psql "$pg_admin" -qc 'DROP DATABASE IF EXISTS teamcraft_smoke' >/dev/null 2>&1 || true
}
trap cleanup EXIT

smoke_server() {
  log "building teamcraft-server"
  "$engine" build --format docker -f containers/teamcraft-server.Containerfile -t localhost/teamcraft-server:dev .
  psql "$pg_admin" -qc 'DROP DATABASE IF EXISTS teamcraft_smoke' -c 'CREATE DATABASE teamcraft_smoke'

  log "starting teamcraft-server (read-only rootfs, no capabilities)"
  run_hardened teamcraft-smoke-server localhost/teamcraft-server:dev \
    -e "TEAMCRAFT_SERVER__LISTEN_ADDR=[::]:$server_port" \
    -e "TEAMCRAFT_DATABASE__URL=${pg_smoke/127.0.0.1/127.0.0.1}" \
    -e TEAMCRAFT_OIDC__ISSUER=http://127.0.0.1:8080/realms/teamcraft-dev \
    -e TEAMCRAFT_OIDC__CLIENT_ID=teamcraft \
    -e TEAMCRAFT_OIDC__SERVER_CLIENT=teamcraft-server

  wait_for "the server to answer /healthz" curl -fsS "http://127.0.0.1:$server_port/healthz"
  wait_for "the server to answer /readyz" curl -fsS "http://127.0.0.1:$server_port/readyz"
  curl -fsS "http://[::1]:$server_port/healthz" >/dev/null || fail "no IPv6 access"
  "$engine" exec teamcraft-smoke-server /usr/local/bin/teamcraft-server healthcheck 2>/dev/null \
    || log "(distroless has no exec shell; the healthcheck subcommand is checked with a second container below)"
  "$engine" run --rm --network host --read-only --cap-drop all \
    -e "TEAMCRAFT_SERVER__LISTEN_ADDR=[::]:$server_port" \
    -e "TEAMCRAFT_DATABASE__URL=$pg_smoke" \
    -e TEAMCRAFT_OIDC__ISSUER=http://127.0.0.1:8080/realms/teamcraft-dev \
    -e TEAMCRAFT_OIDC__CLIENT_ID=teamcraft -e TEAMCRAFT_OIDC__SERVER_CLIENT=teamcraft-server \
    localhost/teamcraft-server:dev healthcheck || fail "healthcheck subcommand failed against a healthy server"

  local uid
  uid=$("$engine" inspect teamcraft-smoke-server --format '{{.Config.User}}')
  [ "$uid" = "65532:65532" ] || fail "runs as $uid, expected 65532:65532"
  "$engine" top teamcraft-smoke-server user | tail -n +2 | grep -qv '^root' || fail "process runs as root"

  "$engine" stop -t 10 teamcraft-smoke-server >/dev/null
  [ "$("$engine" inspect teamcraft-smoke-server --format '{{.State.ExitCode}}')" = 0 ] \
    || fail "did not exit cleanly on SIGTERM"
  log "teamcraft-server OK"
}

smoke_sfu() {
  log "building teamcraft-sfu"
  "$engine" build --format docker -f containers/teamcraft-sfu.Containerfile -t localhost/teamcraft-sfu:dev .

  log "starting teamcraft-sfu (read-only rootfs, no capabilities)"
  run_hardened teamcraft-smoke-sfu localhost/teamcraft-sfu:dev \
    -e "TEAMCRAFT_SFU_BIND=[::]:14433" \
    -e "TEAMCRAFT_SFU_HEALTH_BIND=[::]:$sfu_health_port"

  wait_for "the SFU health endpoint" curl -fsS "http://127.0.0.1:$sfu_health_port/healthz"
  curl -fsS "http://[::1]:$sfu_health_port/healthz" >/dev/null || fail "no IPv6 access"
  "$engine" run --rm --network host --read-only --cap-drop all \
    -e "TEAMCRAFT_SFU_HEALTH_BIND=[::]:$sfu_health_port" \
    localhost/teamcraft-sfu:dev healthcheck || fail "healthcheck subcommand failed against a healthy SFU"

  local uid
  uid=$("$engine" inspect teamcraft-smoke-sfu --format '{{.Config.User}}')
  [ "$uid" = "65532:65532" ] || fail "runs as $uid, expected 65532:65532"
  "$engine" stop -t 10 teamcraft-smoke-sfu >/dev/null
  [ "$("$engine" inspect teamcraft-smoke-sfu --format '{{.State.ExitCode}}')" = 0 ] \
    || fail "did not exit cleanly on SIGTERM"
  log "teamcraft-sfu OK"
}

smoke_bridge() {
  log "building teamcraft-bridge"
  "$engine" build --format docker -f containers/teamcraft-bridge.Containerfile -t localhost/teamcraft-bridge:dev .

  # Without a server to enroll against, the bridge must start far enough to read its configuration,
  # open its encrypted database in the volume directory and try to enroll: a missing library, a
  # read-only volume or a bad entrypoint would fail earlier and differently.
  local dir
  dir=$(mktemp -d)
  chmod 755 "$dir"
  printf 'AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=' >"$dir/key"
  cat >"$dir/bridge.toml" <<'TOML'
[server]
url = "http://127.0.0.1:1"
bot_token = "tcb_unreachable"
[store]
path = "/var/lib/teamcraft-bridge/bridge.db"
key_file = "/config/key"
[identity]
passphrase = "smoke"
[[sources]]
name = "smoke"
token = "smoke-token-0123456789"
conversation_id = "42"
TOML
  chmod 644 "$dir/key" "$dir/bridge.toml"
  log "starting teamcraft-bridge (read-only rootfs, no capabilities)"
  local output
  output=$("$engine" run --rm --read-only --cap-drop all --security-opt no-new-privileges --network host \
    -v "$dir:/config:ro,Z" localhost/teamcraft-bridge:dev --config /config/bridge.toml 2>&1 || true)
  rm -rf "$dir"
  case $output in
    *"enrolling the bot's device"*"network error"*) ;;
    *) fail "unexpected bridge output: $output" ;;
  esac
  local uid
  uid=$("$engine" image inspect localhost/teamcraft-bridge:dev --format '{{.Config.User}}')
  [ "$uid" = "65532:65532" ] || fail "runs as $uid, expected 65532:65532"
  log "teamcraft-bridge OK (the full flow is covered by crates/teamcraft-bridge/tests/end_to_end.rs)"
}

case $which_images in
  server) smoke_server ;;
  sfu) smoke_sfu ;;
  bridge) smoke_bridge ;;
  all) smoke_server; smoke_sfu; smoke_bridge ;;
  *) fail "usage: $0 [server|sfu|bridge]" ;;
esac
