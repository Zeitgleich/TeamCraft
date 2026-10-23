#!/usr/bin/env bash
# Prepares the part of GStreamer.framework (the official universal runtime from
# gstreamer.freedesktop.org) that travels inside the app, for hardware H.264 through VideoToolbox
# (`vtenc_h264`) on both slices.
#
#   bundle-gstreamer.sh patch  <executable>   build a slim framework in target/gstreamer-framework,
#                                              point it and the executable at the app's Frameworks
#                                              directory, strip, re-sign ad hoc, check every reference
#
# Only what the encoder pipeline needs is bundled (crates/teamcraft-media/src/video_gstreamer.rs:
# appsrc ! vtenc_h264 ! capsfilter ! h264parse ! appsink): the plugins in PLUGINS, the plugin
# scanner, and every library they and the executable load. The installed framework also holds
# headers, static libraries, tools, Python and hundreds of plugins (> 1 GB): none of it ships.
#
# Run between `cargo tauri build --no-bundle` and `cargo tauri bundle --config
# tauri.gstreamer.conf.json`. macOS only. Needs the GStreamer runtime installed in /Library/Frameworks.
set -euo pipefail

SOURCE="${GSTREAMER_SOURCE:-/Library/Frameworks/GStreamer.framework}"
INSTALLED_PREFIX="$SOURCE/Versions/1.0"
BUNDLED_PREFIX='@executable_path/../Frameworks/GStreamer.framework/Versions/1.0'
OUTPUT="${GSTREAMER_OUTPUT:-target/gstreamer-framework}"
# coreelements: capsfilter, queue; app: appsrc, appsink; applemedia: vtenc_h264;
# videoparsersbad: h264parse. Add a plugin here when the pipeline gets a new element.
PLUGINS=(coreelements app applemedia videoparsersbad)

if [ "${1:-}" != patch ] || [ $# -ne 2 ]; then
  echo "usage: $0 patch <executable>" >&2
  exit 2
fi
executable="$2"

rm -rf "$OUTPUT"
framework="$OUTPUT/GStreamer.framework"
version_dir="$framework/Versions/1.0"
mkdir -p "$version_dir/lib/gstreamer-1.0" "$version_dir/libexec/gstreamer-1.0" "$version_dir/Resources"

# The framework's own skeleton: stub binary, Info.plist and the usual symlinks. The stub is an
# umbrella over every GStreamer library; the app links the libraries it uses directly and never loads
# it, so it ships only to keep the framework a valid bundle (its references are not checked).
if otool -L "$executable" | awk '{ print $1 }' | grep -q 'GStreamer\.framework/.*GStreamer$'; then
  echo "$executable links the GStreamer umbrella; link the individual libraries instead" >&2
  exit 1
fi
cp "$INSTALLED_PREFIX/GStreamer" "$version_dir/GStreamer"
cp "$INSTALLED_PREFIX/Resources/Info.plist" "$version_dir/Resources/Info.plist"
ln -s 1.0 "$framework/Versions/Current"
ln -s Versions/Current/GStreamer "$framework/GStreamer"
ln -s Versions/Current/Resources "$framework/Resources"
ln -s lib "$version_dir/Libraries"

# The libraries a Mach-O file loads from the framework, as paths relative to Versions/1.0.
framework_dependencies() {
  local own_id
  # A library lists its own install name first; that is not a dependency.
  own_id="$(otool -D "$1" | tail -n +2 | head -n 1)"
  otool -L "$1" | tail -n +2 | awk '{ print $1 }' | while IFS= read -r reference; do
    [ "$reference" = "$own_id" ] && continue
    case "$reference" in
      @rpath/*) printf 'lib/%s\n' "${reference#@rpath/}" ;;
      @loader_path/../lib/*) printf 'lib/%s\n' "${reference#@loader_path/../lib/}" ;;
      "$INSTALLED_PREFIX"/*) printf '%s\n' "${reference#"$INSTALLED_PREFIX"/}" ;;
    esac
  done
}

# Copies the plugins, the scanner and, transitively, every framework library they or the executable
# load (dereferencing symlinks, so each file lands under the name it is loaded by).
queue=("libexec/gstreamer-1.0/gst-plugin-scanner")
for plugin in "${PLUGINS[@]}"; do queue+=("lib/gstreamer-1.0/libgst$plugin.dylib"); done
while IFS= read -r dependency; do queue+=("$dependency"); done < <(framework_dependencies "$executable")
# A newline-separated list, not an associative array: macOS still ships bash 3.2.
copied=$'\n'
while [ ${#queue[@]} -gt 0 ]; do
  relative="${queue[0]}"
  queue=("${queue[@]:1}")
  case "$copied" in *$'\n'"$relative"$'\n'*) continue ;; esac
  copied+="$relative"$'\n'
  # Not every @rpath reference is ours (e.g. a system library resolved elsewhere): skip unknown ones.
  [ -e "$INSTALLED_PREFIX/$relative" ] || { echo "not in the framework, skipped: $relative"; continue; }
  mkdir -p "$version_dir/$(dirname "$relative")"
  cp -L "$INSTALLED_PREFIX/$relative" "$version_dir/$relative"
  chmod u+w "$version_dir/$relative"
  while IFS= read -r dependency; do queue+=("$dependency"); done < <(framework_dependencies "$INSTALLED_PREFIX/$relative")
done

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

# Search paths into the installed framework would load a GStreamer installed on the user's machine
# (or nothing): the executable searches the bundled framework's lib instead.
retarget_rpaths() {
  local file="$1" rpath
  while IFS= read -r rpath; do
    install_name_tool -delete_rpath "$rpath" "$file"
  done < <(otool -l "$file" | awk -v source="$SOURCE" '$1 == "path" && index($2, source) == 1 { print $2 }' | sort -u)
}

while IFS= read -r -d '' file; do
  # Local symbols only matter for debugging; the dynamic symbol table stays intact.
  strip -x "$file"
  id="$(otool -D "$file" | tail -n +2 | head -n 1)"
  case "$id" in
    "$INSTALLED_PREFIX"/*) install_name_tool -id "${BUNDLED_PREFIX}${id#"$INSTALLED_PREFIX"}" "$file" ;;
  esac
  retarget "$file"
  retarget_rpaths "$file"
  codesign --force --sign - "$file" >/dev/null
done < <(macho_files "$framework")

retarget "$executable"
retarget_rpaths "$executable"
if ! otool -l "$executable" | awk '$1 == "path" { print $2 }' | grep -qx "$BUNDLED_PREFIX/lib"; then
  install_name_tool -add_rpath "$BUNDLED_PREFIX/lib" "$executable"
fi
codesign --force --sign - "$executable" >/dev/null

# Every reference the executable and the framework make must exist inside the bundle layout:
# a missing library would stop the app from starting at all.
check_references() {
  local file="$1" reference resolved own_id
  own_id="$(otool -D "$file" | tail -n +2 | head -n 1)"
  while IFS= read -r reference; do
    resolved="${reference/@executable_path\/..\/Frameworks/$OUTPUT}"
    if [ ! -e "$resolved" ]; then
      echo "unresolved reference $reference in $file" >&2
      return 1
    fi
  done < <(otool -L "$file" | awk '$1 ~ /^@executable_path\// { print $1 }' | sort -u)
  while IFS= read -r reference; do
    [ "$reference" = "$own_id" ] && continue
    if [ ! -e "$version_dir/lib/${reference#@rpath/}" ] && [ ! -e "/usr/lib/${reference#@rpath/}" ]; then
      echo "unresolved reference $reference in $file" >&2
      return 1
    fi
  done < <(otool -L "$file" | awk '$1 ~ /^@rpath\// { print $1 }' | sort -u)
  if otool -L "$file" | awk '{ print $1 }' | grep -q "^$INSTALLED_PREFIX/"; then
    echo "$file still points at the installed framework" >&2
    return 1
  fi
}

check_references "$executable"
while IFS= read -r -d '' file; do
  [ "$file" = "$version_dir/GStreamer" ] || check_references "$file"
done < <(macho_files "$framework")
for plugin in "${PLUGINS[@]}"; do
  [ -f "$version_dir/lib/gstreamer-1.0/libgst$plugin.dylib" ] || { echo "plugin $plugin missing" >&2; exit 1; }
done
echo "GStreamer framework prepared in $framework: $(du -sh "$framework" | cut -f1)" \
  "(installed: $(du -sh "$SOURCE/" | cut -f1)), $(find "$version_dir" -type f | wc -l | tr -d ' ') files"
