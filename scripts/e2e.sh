#!/usr/bin/env bash
# Runs the end-to-end suite (tests/e2e/README.md) with one command: starts the dev stack, builds the
# server, SFU and bridge, builds the web client in a container and runs the tests.
#
#   scripts/e2e.sh                  # everything, including the browser tests
#   scripts/e2e.sh --no-browser     # without the web client build and Chromium
#   scripts/e2e.sh --down           # stop the dev stack afterwards
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
test_args=()

while [ $# -gt 0 ]; do
  case "$1" in
    --no-browser) features="e2e" ;;
    --down) is_down=true ;;
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

step "Dev stack (Postgres, Keycloak, Garage, Mailpit)"
"${compose[@]}" up -d postgres keycloak garage mailpit
wait_until "Postgres" 120 "${compose[@]}" exec -T postgres pg_isready -U teamcraft -d teamcraft
wait_until "Keycloak" 240 curl -sf http://127.0.0.1:8080/realms/teamcraft-dev/.well-known/openid-configuration
wait_until "Garage" 120 curl -s -o /dev/null http://127.0.0.1:3900
wait_until "Mailpit" 60 curl -sf http://127.0.0.1:8025/api/v1/info
./dev/init-garage.sh

step "Server, SFU and bridge"
SQLX_OFFLINE=true cargo build -p teamcraft-server -p teamcraft-sfu -p teamcraft-bridge

if [ "$features" = "browser" ]; then
  step "Web client (trunk in a container)"
  tests/e2e/web/build.sh
fi

step "Tests"
# Failed tests leave the logs of their server, SFU and bridge here; start clean.
rm -rf "${TMPDIR:-/tmp}"/teamcraft-e2e-*
rm -rf target/e2e-artifacts
drop_test_databases
# E2E_REQUIRED=1: a missing service or browser fails a test instead of skipping it.
E2E_REQUIRED=1 SQLX_OFFLINE=true \
  cargo test -p teamcraft-e2e --features "$features" --test suite -- ${test_args[@]+"${test_args[@]}"}
