#!/usr/bin/env bash
# Sends an app or a dmg to Apple's notary service, waits for the verdict and staples the ticket, so
# Gatekeeper accepts it offline. An app is sent as a zip of itself; the ticket goes onto the app.
#
#   notarize.sh <TeamCraft.app | TeamCraft.dmg>
#
# Credentials, from the environment, the first complete set wins:
#   APP_STORE_CONNECT_API_KEY (.p8 text), APP_STORE_CONNECT_KEY_ID, APP_STORE_CONNECT_ISSUER_ID
#     the team key the iOS release already uses (docs/release.md)
#   APPLE_ID, APPLE_PASSWORD (app-specific), APPLE_TEAM_ID
# The key is written to a file only this user can read and removed again; no secret is passed on a
# command line except the app-specific password, which notarytool takes no other way without a
# keychain profile. macOS only.
set -euo pipefail

if [ $# -ne 1 ] || [ ! -e "$1" ]; then
  echo "usage: $0 <path/to/App.app | path/to/App.dmg>" >&2
  exit 2
fi
target="$1"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

if [ -n "${APP_STORE_CONNECT_API_KEY:-}" ] && [ -n "${APP_STORE_CONNECT_KEY_ID:-}" ] && [ -n "${APP_STORE_CONNECT_ISSUER_ID:-}" ]; then
  key="$work/AuthKey_${APP_STORE_CONNECT_KEY_ID}.p8"
  (umask 077 && printf '%s\n' "$APP_STORE_CONNECT_API_KEY" > "$key")
  credentials=(--key "$key" --key-id "$APP_STORE_CONNECT_KEY_ID" --issuer "$APP_STORE_CONNECT_ISSUER_ID")
elif [ -n "${APPLE_ID:-}" ] && [ -n "${APPLE_PASSWORD:-}" ] && [ -n "${APPLE_TEAM_ID:-}" ]; then
  credentials=(--apple-id "$APPLE_ID" --password "$APPLE_PASSWORD" --team-id "$APPLE_TEAM_ID")
else
  echo "error: no notarization credentials: set the App Store Connect API key (APP_STORE_CONNECT_API_KEY, _KEY_ID, _ISSUER_ID) or APPLE_ID, APPLE_PASSWORD and APPLE_TEAM_ID" >&2
  exit 1
fi

upload="$target"
if [ -d "$target" ]; then
  upload="$work/$(basename "$target").zip"
  ditto -c -k --keepParent "$target" "$upload"
fi
xcrun notarytool submit "$upload" "${credentials[@]}" --wait
xcrun stapler staple "$target"
