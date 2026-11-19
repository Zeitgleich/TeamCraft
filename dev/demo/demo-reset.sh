#!/usr/bin/env bash
# Snapshot and nightly reset of the demo instance for store reviews (docs/operations.md, "Demo
# instance"; issue #1628).
#
#   demo-reset.sh snapshot   after the seeding run: dump the database and copy the bucket and the seed
#                            devices' stores into the seed
#   demo-reset.sh restore    nightly: put the database, the bucket and the stores back to the seed
#   demo-reset.sh check      only check the settings, the tools and the seed; changes nothing
#
# Both modes stop demo-peers (the seed users' devices, which let reviewers' new devices in) and
# teamcraft-server first and start them again afterwards, also after a failure: the server first,
# then, once it is ready, the peers. The stores and the database must come from the same moment:
# a device's MLS groups and keys belong to the epochs in the database. Every step is logged with a UTC time stamp (stderr, and DEMO_LOG_FILE if
# set). Any failure exits non-zero, so the timer or cron job that runs it can alert. Running it
# twice gives the same result; a second run while one is in progress waits for it (flock).
#
# Settings (environment):
#   DEMO_DATABASE_URL        required. postgres://user@host/db of the owner of the demo database,
#                            without the password: it is on the command line of psql, pg_dump and
#                            pg_restore. Put the password in ~/.pgpass (or the file PGPASSFILE names).
#   DEMO_BUCKET              required. rclone remote and bucket, e.g. demo-s3:teamcraft-demo
#                            (the remote configured through RCLONE_CONFIG_DEMO_S3_* variables)
#   DEMO_STOP_CMD            required. Stops teamcraft-server, e.g. "systemctl --user stop teamcraft-server"
#   DEMO_START_CMD           required. Starts it again
#   DEMO_READY_URL           default http://127.0.0.1:8080/readyz
#   DEMO_READY_TIMEOUT_SECS  default 180
#   DEMO_PEERS_STOP_CMD      required. Stops demo-peers, e.g. "systemctl --user stop teamcraft-demo-peers"
#   DEMO_PEERS_START_CMD     required. Starts it again
#   DEMO_PEER_STORES         default /var/lib/teamcraft-demo/peers (what demo-peers runs, as demo-seed
#                            wrote it: one directory per seed user)
#   DEMO_SEED_DIR            default /var/lib/teamcraft-demo/seed (database.dump, bucket/, peers/,
#                            seed-info)
#   DEMO_LOG_FILE            optional, appended to
#
# Safety: the database's schema `public` is emptied and the bucket made equal to the seed's copy.
# The script refuses a database whose name doesn't contain "demo" (unless DEMO_ALLOW_ANY_DATABASE=1)
# and a bucket whose name doesn't (the part after the remote's ":", unless DEMO_ALLOW_ANY_BUCKET=1).
set -Eeuo pipefail

readonly SEED_DIR="${DEMO_SEED_DIR:-/var/lib/teamcraft-demo/seed}"
readonly READY_URL="${DEMO_READY_URL:-http://127.0.0.1:8080/readyz}"
readonly READY_TIMEOUT_SECS="${DEMO_READY_TIMEOUT_SECS:-180}"
readonly DUMP="$SEED_DIR/database.dump"
readonly BUCKET_COPY="$SEED_DIR/bucket"
readonly INFO="$SEED_DIR/seed-info"
readonly STORES="${DEMO_PEER_STORES:-/var/lib/teamcraft-demo/peers}"
readonly STORES_COPY="$SEED_DIR/peers"

is_stopped=0
are_peers_stopped=0

log() {
  local line
  line="$(date -u +%Y-%m-%dT%H:%M:%SZ) demo-reset: $*"
  printf '%s\n' "$line" >&2
  if [[ -n "${DEMO_LOG_FILE:-}" ]]; then
    printf '%s\n' "$line" >>"$DEMO_LOG_FILE"
  fi
}

fail() {
  log "FAILED: $*"
  exit 1
}

main() {
  local mode="${1:-}"
  case "$mode" in
    snapshot | restore | check) ;;
    *) fail "usage: demo-reset.sh snapshot|restore|check" ;;
  esac
  check_settings
  mkdir -p "$SEED_DIR"
  exec 9>"$SEED_DIR/.lock"
  flock 9
  trap on_error ERR
  log "$mode: start"
  case "$mode" in
    check) check_seed ;;
    snapshot) snapshot ;;
    restore) restore ;;
  esac
  log "$mode: done"
}

check_settings() {
  local name
  for name in DEMO_DATABASE_URL DEMO_BUCKET DEMO_STOP_CMD DEMO_START_CMD DEMO_PEERS_STOP_CMD \
    DEMO_PEERS_START_CMD; do
    [[ -n "${!name:-}" ]] || fail "$name is not set"
  done
  for name in psql pg_dump pg_restore rclone curl flock cp; do
    command -v "$name" >/dev/null || fail "$name is not installed"
  done
  local bucket="${DEMO_BUCKET#*:}"
  if [[ "$bucket" != *demo* && "${DEMO_ALLOW_ANY_BUCKET:-0}" != 1 ]]; then
    fail "refusing bucket '$bucket': its name has no 'demo' (DEMO_ALLOW_ANY_BUCKET=1 overrides)"
  fi
  local database
  database="$(psql "$DEMO_DATABASE_URL" -XAtqc 'SELECT current_database()')" ||
    fail "the database does not answer"
  if [[ "$database" != *demo* && "${DEMO_ALLOW_ANY_DATABASE:-0}" != 1 ]]; then
    fail "refusing database '$database': its name has no 'demo' (DEMO_ALLOW_ANY_DATABASE=1 overrides)"
  fi
}

check_seed() {
  [[ -s "$DUMP" ]] || fail "no database dump at $DUMP (run 'snapshot' after seeding)"
  [[ -d "$BUCKET_COPY" ]] || fail "no bucket copy at $BUCKET_COPY (run 'snapshot' after seeding)"
  pg_restore --list "$DUMP" >/dev/null || fail "the dump at $DUMP is unreadable"
  check_stores "$STORES_COPY"
  log "seed: $(cat "$INFO" 2>/dev/null || echo 'no seed-info')"
}

# Every seed store has its database and its key (demo-seed writes <login>/store.db and store.key).
check_stores() {
  local directory="$1" stores=0 store
  [[ -d "$directory" ]] || fail "no seed device stores at $directory (seed with DEMO_PEER_STORES, then 'snapshot')"
  for store in "$directory"/*/; do
    [[ -s "$store/store.db" && -s "$store/store.key" ]] || fail "incomplete seed device store: $store"
    ((stores += 1))
  done
  ((stores > 0)) || fail "no seed device stores at $directory"
}

snapshot() {
  check_stores "$STORES"
  stop_peers
  stop_server
  log "dumping the database"
  pg_dump --format=custom --no-owner --no-privileges --file="$DUMP.new" "$DEMO_DATABASE_URL"
  log "copying the bucket"
  rm -rf "$BUCKET_COPY.new"
  rclone sync --checksum "$DEMO_BUCKET" "$BUCKET_COPY.new"
  log "copying the seed devices' stores"
  rm -rf "$STORES_COPY.new"
  # -a keeps the key files readable by their owner only.
  cp -a "$STORES" "$STORES_COPY.new"
  # Swap in only once every copy is complete, so a failed snapshot keeps the previous seed.
  mv -f "$DUMP.new" "$DUMP"
  swap_in "$BUCKET_COPY"
  swap_in "$STORES_COPY"
  printf 'taken %s, %s objects, %s seed devices\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
    "$(find "$BUCKET_COPY" -type f | wc -l | tr -d ' ')" \
    "$(find "$STORES_COPY" -mindepth 1 -maxdepth 1 -type d | wc -l | tr -d ' ')" >"$INFO"
  start_server
  start_peers
}

# Replaces the directory $1 by $1.new.
swap_in() {
  local target="$1"
  rm -rf "$target.old"
  if [[ -d "$target" ]]; then
    mv "$target" "$target.old"
  fi
  mv "$target.new" "$target"
  rm -rf "$target.old"
}

restore() {
  check_seed
  stop_peers
  stop_server
  log "emptying the database"
  # The whole schema, also tables of migrations newer than the seed; the server migrates the
  # restored seed forward when it starts. TeamCraft keeps everything in `public`.
  psql "$DEMO_DATABASE_URL" -X -v ON_ERROR_STOP=1 -q \
    -c 'SET client_min_messages = warning' -c 'DROP SCHEMA IF EXISTS public CASCADE' \
    -c 'CREATE SCHEMA public'
  log "restoring the database"
  pg_restore --no-owner --no-privileges --exit-on-error --single-transaction \
    --dbname="$DEMO_DATABASE_URL" "$DUMP"
  log "restoring the bucket"
  # sync makes the bucket equal to the copy: files uploaded since are deleted.
  rclone sync --checksum "$BUCKET_COPY" "$DEMO_BUCKET"
  log "restoring the seed devices' stores"
  rm -rf "$STORES.new"
  cp -a "$STORES_COPY" "$STORES.new"
  swap_in "$STORES"
  start_server
  start_peers
}

stop_peers() {
  log "stopping demo-peers"
  are_peers_stopped=1
  bash -c "$DEMO_PEERS_STOP_CMD"
}

# Only after the server is ready: the peers would otherwise spend their first minutes in backoff.
start_peers() {
  log "starting demo-peers"
  bash -c "$DEMO_PEERS_START_CMD"
  are_peers_stopped=0
}

stop_server() {
  log "stopping teamcraft-server"
  is_stopped=1
  bash -c "$DEMO_STOP_CMD"
}

start_server() {
  log "starting teamcraft-server"
  bash -c "$DEMO_START_CMD"
  is_stopped=0
  wait_until_ready
}

wait_until_ready() {
  local waited=0
  until curl -fsS --max-time 5 -o /dev/null "$READY_URL"; do
    ((waited += 5))
    ((waited < READY_TIMEOUT_SECS)) || fail "not ready after ${READY_TIMEOUT_SECS}s: $READY_URL"
    sleep 5
  done
  log "teamcraft-server is ready"
}

# A failed step must not leave the instance down: start the server and the peers again, then report
# the failure. The peers start on whatever stores are in place: a restore that failed before the
# stores were swapped leaves the previous day's, which still match a database that wasn't touched;
# if the database was already emptied the instance is broken anyway and the alert says so.
on_error() {
  local status=$? line="${BASH_LINENO[0]}"
  trap - ERR
  log "FAILED: step at line $line exited with $status"
  if ((is_stopped)); then
    log "starting teamcraft-server again after the failure"
    if bash -c "$DEMO_START_CMD"; then
      wait_until_ready || true
    else
      log "FAILED: could not start teamcraft-server"
    fi
  fi
  if ((are_peers_stopped)); then
    log "starting demo-peers again after the failure"
    bash -c "$DEMO_PEERS_START_CMD" || log "FAILED: could not start demo-peers"
  fi
  exit 1
}

main "$@"
