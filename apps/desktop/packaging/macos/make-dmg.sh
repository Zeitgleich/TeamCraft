#!/usr/bin/env bash
# Packs a signed app into a compressed dmg (with an Applications link to drag it to), then mounts
# the dmg and checks the app inside again: the dmg must carry exactly the signed app.
#
#   make-dmg.sh <TeamCraft.app> <out.dmg>
#
# The window is laid out like most Mac apps' (#710): a fixed size, the background from
# scripts/brand/dmg_background.py (1x and 2x in one TIFF) with an arrow from the app to
# Applications, no toolbar or sidebar. The layout is set through Finder (AppleScript); where Finder
# can't be scripted the dmg keeps the plain window and the script warns. The sizes and positions
# match `bundle.macOS.dmg` in tauri.conf.json, which the Tauri bundler uses for local builds.
#
# With MACOS_SIGNING_IDENTITY set to a real identity the dmg itself is signed too.
set -euo pipefail

WINDOW_WIDTH=660
WINDOW_HEIGHT=400
ICON_SIZE=128
APP_X=180
APP_Y=190
APPLICATIONS_X=480
APPLICATIONS_Y=190

# Window size, background and icon positions, written by Finder into the volume's .DS_Store. The
# folder is named by its path, so the volume needn't be under /Volumes.
layout() {
  local volume="$1" waited=0
  osascript - "$volume" <<EOF
on run argv
  set theVolume to POSIX file (item 1 of argv) as alias
  tell application "Finder"
    open theVolume
    set theWindow to container window of theVolume
    set current view of theWindow to icon view
    set toolbar visible of theWindow to false
    set statusbar visible of theWindow to false
    set sidebar width of theWindow to 0
    set bounds of theWindow to {200, 120, 200 + $WINDOW_WIDTH, 120 + $WINDOW_HEIGHT}
    set viewOptions to icon view options of theWindow
    set arrangement of viewOptions to not arranged
    set icon size of viewOptions to $ICON_SIZE
    set text size of viewOptions to 13
    set background picture of viewOptions to file ".background:background.tiff" of theVolume
    set position of item "$name.app" of theVolume to {$APP_X, $APP_Y}
    set position of item "Applications" of theVolume to {$APPLICATIONS_X, $APPLICATIONS_Y}
    update theVolume without registering applications
    delay 1
    close theWindow
  end tell
end run
EOF
  # Finder writes .DS_Store a moment after the window closes.
  while [ ! -f "$volume/.DS_Store" ] && [ "$waited" -lt 10 ]; do
    sleep 1
    waited=$((waited + 1))
  done
  [ -f "$volume/.DS_Store" ]
}

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
writable="$(mktemp -u).dmg"
trap 'hdiutil detach "$mount" -quiet 2>/dev/null || true; rm -rf "$staging" "$writable"' EXIT
# ditto keeps symlinks, extended attributes and the code signature intact.
ditto "$app" "$staging/$name.app"
ln -s /Applications "$staging/Applications"
mkdir "$staging/.background"
tiffutil -cathidpicheck "$here/dmg-background.png" "$here/dmg-background@2x.png" \
  -out "$staging/.background/background.tiff"

# Laid out on a writable image (with room for Finder's .DS_Store), then compressed.
size_mb=$(( $(du -sm "$staging" | cut -f1) + 20 ))
hdiutil create -volname "$name" -srcfolder "$staging" -fs HFS+ -format UDRW -size "${size_mb}m" -ov "$writable"
hdiutil attach -readwrite -noverify -noautoopen -mountpoint "$mount" "$writable" >/dev/null
if ! layout "$mount" 2>&1; then
  echo "warning: Finder could not lay out the dmg window; it opens as a plain window" >&2
  [ -n "${GITHUB_ACTIONS:-}" ] && echo "::warning::the dmg window has no background layout"
fi
sync
hdiutil detach "$mount" -quiet
rm -f "$dmg"
hdiutil convert "$writable" -format UDZO -imagekey zlib-level=9 -ov -o "$dmg"
if [ "$identity" != "-" ]; then
  codesign --force --sign "$identity" --timestamp "$dmg"
fi

hdiutil attach -readonly -nobrowse -noautoopen -mountpoint "$mount" "$dmg" >/dev/null
"$here/verify-app.sh" "$mount/$name.app"
hdiutil detach "$mount" -quiet
echo "dmg: $dmg ($(du -h "$dmg" | cut -f1))"
