#!/usr/bin/env bash
# Prepares GStreamer.framework (the official universal runtime from gstreamer.freedesktop.org) to
# travel inside the app, for hardware H.264 through VideoToolbox (`vtenc_h264`) on both slices.
#
#   bundle-gstreamer.sh patch  <executable>   copy the framework to target/gstreamer-framework,
#                                              point it and the executable at the app's Frameworks
#                                              directory, re-sign ad hoc, and check every reference
#
# Run between `cargo tauri build --no-bundle` and `cargo tauri bundle --config
# tauri.gstreamer.conf.json`. macOS only. Needs the GStreamer runtime installed in /Library/Frameworks.
set -euo pipefail

SOURCE=/Library/Frameworks/GStreamer.framework
INSTALLED_PREFIX="$SOURCE/Versions/1.0"
BUNDLED_PREFIX='@executable_path/../Frameworks/GStreamer.framework/Versions/1.0'
OUTPUT="${GSTREAMER_OUTPUT:-target/gstreamer-framework}"

if [ "${1:-}" != patch ] || [ $# -ne 2 ]; then
  echo "usage: $0 patch <executable>" >&2
  exit 2
fi
executable="$2"

rm -rf "$OUTPUT"
mkdir -p "$OUTPUT"
cp -R "$SOURCE" "$OUTPUT/GStreamer.framework"
framework="$OUTPUT/GStreamer.framework"

# Mach-O files: libraries, plugins and helper tools, but not headers, docs or symlinks.
macho_files() {
  find "$1" -type f \( -name '*.dylib' -o -name '*.so' -o -perm -u+x \) -print0 |
    while IFS= read -r -d '' file; do
      if file "$file" | grep -q 'Mach-O'; then
        printf '%s\0' "$file"
      fi
    done
}

# Rewrites every reference to the installed framework into one relative to the app's executable.
retarget() {
  local file="$1" reference
  # Fat files list their dependencies once per slice; the rewrite applies to all slices.
  while IFS= read -r reference; do
    install_name_tool -change "$reference" "${BUNDLED_PREFIX}${reference#"$INSTALLED_PREFIX"}" "$file"
  done < <(otool -L "$file" | awk -v prefix="$INSTALLED_PREFIX/" 'index($1, prefix) == 1 { print $1 }' | sort -u)
}

while IFS= read -r -d '' file; do
  if [ "$(otool -D "$file" | tail -n +2 | head -n 1)" != "" ]; then
    id="$(otool -D "$file" | tail -n +2 | head -n 1)"
    case "$id" in
      "$INSTALLED_PREFIX"/*) install_name_tool -id "${BUNDLED_PREFIX}${id#"$INSTALLED_PREFIX"}" "$file" ;;
    esac
  fi
  retarget "$file"
  codesign --force --sign - "$file" >/dev/null
done < <(macho_files "$framework")

retarget "$executable"
codesign --force --sign - "$executable" >/dev/null

# Every reference the executable and the framework make must exist inside the bundle layout:
# a missing library would stop the app from starting at all.
check_references() {
  local file="$1" reference resolved
  while IFS= read -r reference; do
    resolved="${reference/@executable_path\/..\/Frameworks/$OUTPUT}"
    if [ ! -e "$resolved" ]; then
      echo "unresolved reference $reference in $file" >&2
      return 1
    fi
  done < <(otool -L "$file" | awk '$1 ~ /^@executable_path\// { print $1 }' | sort -u)
  if otool -L "$file" | awk '{ print $1 }' | grep -q "^$INSTALLED_PREFIX/"; then
    echo "$file still points at the installed framework" >&2
    return 1
  fi
}

check_references "$executable"
while IFS= read -r -d '' file; do
  check_references "$file"
done < <(macho_files "$framework")
echo "GStreamer framework prepared in $framework"
