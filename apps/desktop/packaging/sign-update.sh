#!/usr/bin/env bash
# Signs desktop update artifacts for the in-app updater (docs/updates.md §3.3):
#
#   apps/desktop/packaging/sign-update.sh <version> <file>...
#
# writes <file>.sig with `cargo tauri signer sign --app-version <version>`, so the signature binds
# the version the manifest announces. The key comes from TAURI_SIGNING_PRIVATE_KEY and
# TAURI_SIGNING_PRIVATE_KEY_PASSWORD (repository secrets).
#
# Without the key secret the release is built unsigned for updates: a warning, exit 0, no .sig
# files, and release.yml writes no latest.json. With the key but an empty public key in
# tauri.conf.json it fails: those builds could never verify their own updates.
set -euo pipefail

warn() { echo "warning: $*" >&2; [ -n "${GITHUB_ACTIONS:-}" ] && echo "::warning::$*"; return 0; }
fail() { echo "error: $*" >&2; [ -n "${GITHUB_ACTIONS:-}" ] && echo "::error::$*"; exit 1; }

[ $# -ge 2 ] || fail "usage: $0 <version> <file>..."
version="$1"
shift
config="$(dirname "$0")/../tauri.conf.json"

if [ -z "${TAURI_SIGNING_PRIVATE_KEY:-}" ]; then
  warn "TAURI_SIGNING_PRIVATE_KEY is not set: update artifacts stay unsigned and the release gets no latest.json (docs/updates.md §7)"
  exit 0
fi
pubkey="$(python3 -c 'import json, sys; print(json.load(open(sys.argv[1]))["plugins"]["updater"]["pubkey"].strip())' "$config")"
[ -n "$pubkey" ] || fail "the signing key is set but plugins.updater.pubkey in tauri.conf.json is empty: paste the public key (docs/updates.md §7)"

for file in "$@"; do
  [ -f "$file" ] || fail "$file does not exist"
  rm -f "$file.sig"
  cargo tauri signer sign --app-version "$version" "$file"
  [ -s "$file.sig" ] || fail "no signature was written for $file"
  # The key ID in the signature must be the public key's, or no installed app could verify it.
  python3 - "$pubkey" "$file.sig" <<'PY' || fail "$file.sig was made with a key that does not match plugins.updater.pubkey"
import base64, sys
def key_id(encoded):
    text = base64.b64decode(encoded).decode()
    line = next(l for l in text.splitlines() if l.strip() and "comment:" not in l)
    return base64.b64decode(line)[2:10]
sys.exit(0 if key_id(sys.argv[1]) == key_id(open(sys.argv[2]).read().strip()) else 1)
PY
  echo "signed $file"
done
