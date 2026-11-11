#!/usr/bin/env bash
# Snapshot and nightly reset of the demo instance for store reviews (docs/operations.md, "Demo
# instance"; issue #1628).
#
#   demo-reset.sh snapshot   after the seeding run: dump the database and copy the bucket into the seed
#   demo-reset.sh restore    nightly: put the database and the bucket back to the seed
#   demo-reset.sh check      only check the settings, the tools and the seed; changes nothing
#
# Both modes stop teamcraft-server first and start it again afterwards, also after a failure, and
# wait until it is ready. Every step is logged with a UTC time stamp (stderr, and DEMO_LOG_FILE if
# set). Any failure exits non-zero, so the timer or cron job that runs it can alert. Running it
# twice gives the same result; a second run while one is in progress waits for it (flock).
#
# Settings (environment):
#   DEMO_DATABASE_URL        required. postgres://... of the owner of the demo database
#   DEMO_BUCKET              required. rclone remote and bucket, e.g. demo-s3:teamcraft-demo
#                            (the remote configured through RCLONE_CONFIG_DEMO_S3_* variables)
#   DEMO_STOP_CMD            required. Stops teamcraft-server, e.g. "systemctl --user stop teamcraft-server"
#   DEMO_START_CMD           required. Starts it again
#   DEMO_READY_URL           default http://127.0.0.1:8080/readyz
#   DEMO_READY_TIMEOUT_SECS  default 180
#   DEMO_SEED_DIR            default /var/lib/teamcraft-demo/seed (database.dump, bucket/, seed-info)
#   DEMO_LOG_FILE            optional, appended to
#
# Safety: the database's schema `public` is emptied before the restore. The script
# refuses a database whose name doesn't contain "demo", unless DEMO_ALLOW_ANY_DATABASE=1.
set -Eeuo pipefail

readonly SEED_DIR="${DEMO_SEED_DIR:-/var/lib/teamcraft-demo/seed}"
readonly READY_URL="${DEMO_READY_URL:-http://127.0.0.1:8080/readyz}"
readonly READY_TIMEOUT_SECS="${DEMO_READY_TIMEOUT_SECS:-180}"
readonly DUMP="$SEED_DIR/database.dump"
readonly BUCKET_COPY="$SEED_DIR/bucket"
readonly INFO="$SEED_DIR/seed-info"

is_stopped=0

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
  for name in DEMO_DATABASE_URL DEMO_BUCKET DEMO_STOP_CMD DEMO_START_CMD; do
    [[ -n "${!name:-}" ]] || fail "$name is not set"
  done
  for name in psql pg_dump pg_restore rclone curl flock; do
    command -v "$name" >/dev/null || fail "$name is not installed"
  done
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
  log "seed: $(cat "$INFO" 2>/dev/null || echo 'no seed-info')"
}

snapshot() {
  stop_server
  log "dumping the database"
  pg_dump --format=custom --no-owner --no-privileges --file="$DUMP.new" "$DEMO_DATABASE_URL"
  log "copying the bucket"
  rm -rf "$BUCKET_COPY.new"
  rclone sync --checksum "$DEMO_BUCKET" "$BUCKET_COPY.new"
  # Swap in only once both copies are complete, so a failed snapshot keeps the previous seed.
  mv -f "$DUMP.new" "$DUMP"
  rm -rf "$BUCKET_COPY.old"
  if [[ -d "$BUCKET_COPY" ]]; then
    mv "$BUCKET_COPY" "$BUCKET_COPY.old"
  fi
  mv "$BUCKET_COPY.new" "$BUCKET_COPY"
  rm -rf "$BUCKET_COPY.old"
  printf 'taken %s, %s objects\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
    "$(find "$BUCKET_COPY" -type f | wc -l | tr -d ' ')" >"$INFO"
  start_server
}

restore() {
  check_seed
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
  start_server
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

# A failed step must not leave the instance down: start the server again, then report the failure.
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
  exit 1
}

main "$@"
