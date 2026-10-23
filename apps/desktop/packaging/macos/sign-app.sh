#!/usr/bin/env bash
# Signs TeamCraft.app inside out and checks the result, so that Gatekeeper sees one intact seal:
# every Mach-O file of the bundled GStreamer.framework, then the framework as a bundle, then the app
# with its entitlements. Without an identity the signature is ad hoc ("-"): macOS then asks the user
# once to allow the app (docs/release.md, "Opening the unsigned macOS app"), instead of calling it
# damaged, which is what a missing or broken seal looks like.
#
#   sign-app.sh <TeamCraft.app>                    ad hoc
#   MACOS_SIGNING_IDENTITY="Developer ID Application: ..." sign-app.sh <TeamCraft.app>
#
# With a real identity the hardened runtime and a secure timestamp are added (needed for
# notarization). macOS only.
set -euo pipefail

if [ $# -ne 1 ] || [ ! -d "$1/Contents" ]; then
  echo "usage: $0 <path/to/App.app>" >&2
  exit 2
fi
app="$1"
identity="${MACOS_SIGNING_IDENTITY:--}"
entitlements="$(cd "$(dirname "$0")" && pwd)/Entitlements.plist"

options=(--force --sign "$identity")
if [ "$identity" != "-" ]; then
  options+=(--options runtime --timestamp)
fi

is_macho() { file "$1" | grep -q 'Mach-O'; }

# Nested code first: libraries, plugins and helpers of each framework, then the framework itself.
for framework in "$app"/Contents/Frameworks/*.framework; do
  [ -d "$framework" ] || continue
  main="$(basename "$framework" .framework)"
  while IFS= read -r -d '' file; do
    [ "$(basename "$file")" = "$main" ] && continue
    is_macho "$file" && codesign "${options[@]}" "$file"
  done < <(find "$framework/Versions" -type f -print0)
  codesign "${options[@]}" "$framework"
done
# Loose libraries and helper executables besides the main one.
while IFS= read -r -d '' file; do
  is_macho "$file" && codesign "${options[@]}" "$file"
done < <(find "$app/Contents" -path "$app/Contents/Frameworks/*.framework" -prune -o -type f \
  \( -name '*.dylib' -o -perm -u+x \) ! -path "$app/Contents/MacOS/$(defaults read "$app/Contents/Info" CFBundleExecutable)" -print0)
codesign "${options[@]}" --entitlements "$entitlements" "$app"

"$(dirname "$0")/verify-app.sh" "$app"
