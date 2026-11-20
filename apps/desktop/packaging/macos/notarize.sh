#!/usr/bin/env bash
# Notarizes a zipped app or a dmg with Apple's notary service and waits for the verdict.
# Authenticates with the App Store Connect API key the iOS release uses (no Apple ID, no
# app-specific password): API_KEY (the .p8 text), API_KEY_ID, API_ISSUER_ID.
# Usage: notarize.sh <file.zip|file.dmg>
set -euo pipefail

file="${1:?usage: notarize.sh <file.zip|file.dmg>}"
: "${API_KEY:?API_KEY (App Store Connect API key, .p8 text) is not set}"
: "${API_KEY_ID:?API_KEY_ID is not set}"
: "${API_ISSUER_ID:?API_ISSUER_ID is not set}"

key_file="$(mktemp "${RUNNER_TEMP:-${TMPDIR:-/tmp}}/AuthKey_XXXXXX.p8")"
trap 'rm -f "$key_file"' EXIT
printf '%s' "$API_KEY" > "$key_file"

# --wait exits 0 also for "Invalid"; read the status and print Apple's log on failure.
result="$(xcrun notarytool submit "$file" --key "$key_file" --key-id "$API_KEY_ID" \
  --issuer "$API_ISSUER_ID" --wait --output-format json)"
status="$(printf '%s' "$result" | plutil -extract status raw -o - - 2>/dev/null \
  || printf '%s' "$result" | sed -n 's/.*"status" *: *"\([^"]*\)".*/\1/p')"
if [ "$status" != "Accepted" ]; then
  id="$(printf '%s' "$result" | sed -n 's/.*"id" *: *"\([^"]*\)".*/\1/p')"
  echo "::error::notarization of $(basename "$file") ended with status '${status:-unknown}'"
  if [ -n "$id" ]; then
    xcrun notarytool log "$id" --key "$key_file" --key-id "$API_KEY_ID" --issuer "$API_ISSUER_ID" || true
  fi
  exit 1
fi
echo "notarized: $(basename "$file")"
