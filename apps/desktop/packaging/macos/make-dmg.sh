#!/usr/bin/env bash
# Packs a signed app into a compressed dmg (with an Applications link to drag it to), then mounts
# the dmg and checks the app inside again: the dmg must carry exactly the signed app.
#
#   make-dmg.sh <TeamCraft.app> <out.dmg>
#
# With MACOS_SIGNING_IDENTITY set to a real identity the dmg itself is signed too.
set -euo pipefail

if [ $# -ne 2 ] || [ ! -d "$1/Contents" ]; then
  echo "usage: $0 <path/to/App.app> <out.dmg>" >&2
  exit 2
fi
app="$1"
dmg="$2"
name="$(basename "$app" .app)"
identity="${MACOS_SIGNING_IDENTITY:--}"
here="$(cd "$(dirname "$0")" && pwd)"

staging="$(mktemp -d)"
mount="$(mktemp -d)"
trap 'hdiutil detach "$mount" -quiet 2>/dev/null || true; rm -rf "$staging"' EXIT
# ditto keeps symlinks, extended attributes and the code signature intact.
ditto "$app" "$staging/$name.app"
ln -s /Applications "$staging/Applications"
rm -f "$dmg"
hdiutil create -volname "$name" -srcfolder "$staging" -fs HFS+ -format UDZO -imagekey zlib-level=9 -ov "$dmg"
if [ "$identity" != "-" ]; then
  codesign --force --sign "$identity" --timestamp "$dmg"
fi

hdiutil attach -readonly -nobrowse -noautoopen -mountpoint "$mount" "$dmg" >/dev/null
"$here/verify-app.sh" "$mount/$name.app"
hdiutil detach "$mount" -quiet
echo "dmg: $dmg ($(du -h "$dmg" | cut -f1))"
