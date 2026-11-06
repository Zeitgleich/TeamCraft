#!/usr/bin/env bash
# Runs the end-to-end suite (tests/e2e/README.md) with one command: starts the dev stack, builds the
# server, SFU and bridge, builds the web client in a container and runs the tests.
#
#   scripts/e2e.sh                  # everything, including the browser tests
#   scripts/e2e.sh --no-browser     # without the web client build and Chromium
#   scripts/e2e.sh --down           # stop the dev stack afterwards
#   scripts/e2e.sh --prebuilt-web   # use the web client already in target/e2e-web (or $E2E_WEB_DIR)
#   scripts/e2e.sh -- calls::       # arguments after -- go to the test binary (a name filter, --ignored, ...)
#
# Needs Docker with Compose, python3 (dev tokens), a Rust toolchain and, for the browser tests, Chrome
# or Chromium (CHROME=/path/to/chrome if it is not on PATH).
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"

compose=(docker compose -f docker-compose.dev.yml)
features="browser"
is_down=false
has_prebuilt_web=false
test_args=()

while [ $# -gt 0 ]; do
  case "$1" in
    --no-browser) features="e2e" ;;
    --down) is_down=true ;;
    --prebuilt-web) has_prebuilt_web=true ;;
    --) shift; test_args=("$@"); break ;;
    -h|--help) sed -n '2,12p' "$0"; exit 0 ;;
    *) echo "unknown option: $1 (see --help)" >&2; exit 2 ;;
  esac
  shift
done

step() { printf '\n==> %s\n' "$*"; }

wait_until() { # description, timeout seconds, command...
  local what="$1" timeout="$2"; shift 2
  local deadline=$((SECONDS + timeout))
  until "$@" >/dev/null 2>&1; do
    if [ "$SECONDS" -ge "$deadline" ]; then echo "timed out waiting for $what" >&2; exit 1; fi
    sleep 2
  done
}

drop_test_databases() {
  local names
  names="$("${compose[@]}" exec -T postgres psql -U teamcraft -d postgres -Atc \
    "select datname from pg_database where datname like 'teamcraft_e2e_%'" 2>/dev/null || true)"
  for name in $names; do
    "${compose[@]}" exec -T postgres psql -U teamcraft -d postgres -qc \
      "drop database if exists $name with (force)" >/dev/null 2>&1 || true
  done
}

cleanup() {
  drop_test_databases
  if $is_down; then "${compose[@]}" down; fi
}
trap cleanup EXIT

step "Dev stack (Postgres, Keycloak, Garage, Mailpit, CalCraft)"
# CalCraft is built from source on the first run (several minutes); the integration tests of the
# server (caldav_calcraft, admin_settings) need it.
"${compose[@]}" up -d postgres keycloak garage mailpit calcraft
wait_until "Postgres" 120 "${compose[@]}" exec -T postgres pg_isready -U teamcraft -d teamcraft
wait_until "Keycloak" 240 curl -sf http://127.0.0.1:8080/realms/teamcraft-dev/.well-known/openid-configuration
wait_until "Garage" 120 curl -s -o /dev/null http://127.0.0.1:3900
wait_until "Mailpit" 60 curl -sf http://127.0.0.1:8025/api/v1/info
./dev/init-garage.sh
wait_until "CalCraft" 600 curl -s -o /dev/null http://127.0.0.1:5232
./dev/init-calcraft.sh

step "Server, SFU and bridge"
SQLX_OFFLINE=true cargo build -p teamcraft-server -p teamcraft-sfu -p teamcraft-bridge

if $has_prebuilt_web; then
  web_dir="${E2E_WEB_DIR:-$root/target/e2e-web}"
  if [ ! -f "$web_dir/index.html" ]; then echo "no web client in $web_dir" >&2; exit 1; fi
elif [ "$features" = "browser" ]; then
  step "Web client (trunk in a container)"
  tests/e2e/web/build.sh
fi

step "Tests"
# Failed tests leave the logs of their server, SFU and bridge here; start clean.
rm -rf "${TMPDIR:-/tmp}"/teamcraft-e2e-*
rm -rf target/e2e-artifacts
drop_test_databases
# E2E_REQUIRED=1: a missing service or browser fails a test instead of skipping it.
# RUST_MIN_STACK: the test thread polls the whole test body, and in a debug build the futures of the
# core's sync and gateway code have frames of 100-200 KiB each, which the 2 MiB default of a test
# thread no longer held (web::a_second_tab_takes_over aborted the suite with a stack overflow,
# #1203). Memory is only reserved, not used.
RUST_MIN_STACK="${RUST_MIN_STACK:-16777216}" E2E_REQUIRED=1 SQLX_OFFLINE=true \
  cargo test -p teamcraft-e2e --features "$features" --test suite -- ${test_args[@]+"${test_args[@]}"}

# The integration tests that need the dev stack and skip themselves without it (#1284): the server's
# S3, Mailpit and CalCraft tests, and the `dev_stack` tests of core and ffi against a running server.
# TEST_SERVICES_REQUIRED: a missing service fails a test instead of skipping it.
step "Integration tests against the dev stack"
dev_server_log="$(mktemp "${TMPDIR:-/tmp}/teamcraft-e2e-dev-server.XXXXXX")"
dev_server_pid=""
stop_dev_server() { [ -z "$dev_server_pid" ] || kill "$dev_server_pid" 2>/dev/null || true; }
trap 'stop_dev_server; cleanup' EXIT
TEAMCRAFT_SERVER__LISTEN_ADDR="[::]:8081" "${CARGO_TARGET_DIR:-target}/debug/teamcraft-server" \
  --config teamcraft.example.toml >"$dev_server_log" 2>&1 &
dev_server_pid=$!
wait_until "the dev server" 60 curl -sf http://127.0.0.1:8081/healthz || { cat "$dev_server_log" >&2; exit 1; }
RUST_MIN_STACK="${RUST_MIN_STACK:-16777216}" TEST_SERVICES_REQUIRED=1 TEST_DATABASE_REQUIRED=1 \
  DATABASE_URL="postgres://teamcraft:teamcraft-dev-only@127.0.0.1:5432/teamcraft" \
  TEAMCRAFT_TEST_SERVER_URL="http://127.0.0.1:8081" SQLX_OFFLINE=true \
  cargo test -p teamcraft-server -p teamcraft-core -p teamcraft-ffi --features teamcraft-server/integration,teamcraft-core/integration,teamcraft-ffi/integration \
  -- caldav_calcraft admin_settings meeting_mail dev_stack
