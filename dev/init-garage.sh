#!/usr/bin/env bash
# Initializes the dev Garage node: cluster layout, bucket and an access key with fixed dev credentials.
# Idempotent. Run after `docker compose -f docker-compose.dev.yml up -d`.
set -euo pipefail

cd "$(dirname "$0")/.."

COMPOSE=(docker compose -f docker-compose.dev.yml)
BUCKET="${TEAMCRAFT_DEV_BUCKET:-teamcraft-dev}"
KEY_NAME="teamcraft-dev"
# Dev-only credentials (Garage requires GK + 24 hex chars and 64 hex chars).
KEY_ID="GKdeadbeef0000000000000000"
KEY_SECRET="deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef"

garage() { "${COMPOSE[@]}" exec -T garage /garage "$@"; }

echo "Waiting for Garage..."
for _ in $(seq 1 30); do
  if garage status >/dev/null 2>&1; then break; fi
  sleep 1
done
garage status >/dev/null

if garage layout show 2>&1 | grep -q "No nodes"; then
  node_id="$(garage node id -q | cut -d@ -f1)"
  garage layout assign -z dev -c 1G "$node_id"
  garage layout apply --version 1
elif ! garage layout show 2>&1 | grep -q "dev"; then
  echo "Unexpected layout, inspect with: docker compose -f docker-compose.dev.yml exec garage /garage layout show" >&2
  exit 1
fi

garage bucket info "$BUCKET" >/dev/null 2>&1 || garage bucket create "$BUCKET"
garage key info "$KEY_NAME" >/dev/null 2>&1 || garage key import --yes -n "$KEY_NAME" "$KEY_ID" "$KEY_SECRET"
garage bucket allow --read --write --owner "$BUCKET" --key "$KEY_NAME"
# Browsers preflight the presigned uploads; without a CORS rule every web upload fails. Also aborts
# uploads in parts nobody finished (lifecycle rule).
python3 -I dev/garage-cors.py "$BUCKET" "$KEY_ID" "$KEY_SECRET"

echo "Garage ready: bucket=$BUCKET endpoint=http://127.0.0.1:3900 region=garage"
echo "  access key: $KEY_ID"
echo "  secret key: $KEY_SECRET"
