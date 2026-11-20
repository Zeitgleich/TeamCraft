#!/usr/bin/env bash
# Fails when the app's signature is broken: what Gatekeeper reports as "damaged". An intact ad-hoc
# signature passes (spctl still rejects it, for want of a Developer ID; that is expected). With
# MACOS_REQUIRE_NOTARIZED=1 the app must also be accepted by Gatekeeper.
#
# It also checks who the app is to the keychain (#1223). The keychain trusts the app that created an
# item by its designated requirement. An ad-hoc signature has none but the hash of this very build
# (`cdhash H"..."`), so every update is a stranger to the item of the build before it and macOS asks
# for the login password. A Developer ID signature names the team instead
# (`anchor apple generic and ... certificate leaf[subject.OU] = <team>`), which stays the same from
# release to release. With MACOS_SIGNING_IDENTITY (as for sign-app.sh) or MACOS_TEAM_ID set, a
# requirement that is a build's hash fails, and so does a signature of another team than
# MACOS_TEAM_ID.
#
#   verify-app.sh <TeamCraft.app>
set -euo pipefail

app="$1"
echo "== codesign --verify --deep --strict $app"
codesign --verify --deep --strict --verbose=2 "$app"
codesign --display --verbose=2 "$app" 2>&1 | grep -E '^(Identifier|Format|Signature|Authority|TeamIdentifier|Runtime|Sealed Resources)' || true

echo "== designated requirement (who the app is to the keychain)"
# codesign prints a requirement it derived itself (ad hoc: the hash) as a comment, "# designated => ...".
requirement="$(codesign --display --requirements - "$app" 2>/dev/null | sed -n 's/^\(# \)\{0,1\}designated => //p')"
echo "${requirement:-none}"
identity="${MACOS_SIGNING_IDENTITY:--}"
team="${MACOS_TEAM_ID:-}"
if [ "$identity" = "-" ] && [ -z "$team" ]; then
  if echo "$requirement" | grep -q 'cdhash'; then
    echo "note: ad hoc, this build is a stranger to the keychain item of every other build: macOS asks for the login password once after each update (#1223)"
  fi
else
  # A certificate Apple issued: the requirement names the certificate's holder, not this build.
  if [ -z "$requirement" ] || echo "$requirement" | grep -q 'cdhash' || ! echo "$requirement" | grep -q 'anchor apple generic'; then
    echo "error: the designated requirement names no team, so the keychain would ask after every update (#1223)" >&2
    exit 1
  fi
  signed_by="$(codesign --display --verbose=2 "$app" 2>&1 | sed -n 's/^TeamIdentifier=//p')"
  if [ -n "$team" ] && [ "$signed_by" != "$team" ]; then
    echo "error: the app is signed by team ${signed_by:-none}, not by $team" >&2
    exit 1
  fi
fi

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
