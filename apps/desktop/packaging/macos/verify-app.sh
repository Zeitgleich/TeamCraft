#!/usr/bin/env bash
# Fails when the app's signature is broken: what Gatekeeper reports as "damaged". An intact ad-hoc
# signature passes (spctl still rejects it, for want of a Developer ID; that is expected). With
# MACOS_REQUIRE_NOTARIZED=1 the app must also be accepted by Gatekeeper.
#
#   verify-app.sh <TeamCraft.app>
set -euo pipefail

app="$1"
echo "== codesign --verify --deep --strict $app"
codesign --verify --deep --strict --verbose=2 "$app"
codesign --display --verbose=2 "$app" 2>&1 | grep -E '^(Identifier|Format|Signature|Authority|TeamIdentifier|Runtime|Sealed Resources)' || true

echo "== spctl --assess --type exec $app"
assessment="$(spctl --assess -vvv --type exec "$app" 2>&1 || true)"
echo "$assessment"
if echo "$assessment" | grep -qiE 'invalid|damaged|modified|not valid|code has no resources|sealed resource'; then
  echo "error: Gatekeeper sees a broken signature" >&2
  exit 1
fi
if [ "${MACOS_REQUIRE_NOTARIZED:-0}" = 1 ] && ! echo "$assessment" | grep -q ': accepted'; then
  echo "error: the notarized app is not accepted by Gatekeeper" >&2
  exit 1
fi
