#!/usr/bin/env bash
# Writes precompressed variants (<file>.br and <file>.gz) of the web client's assets, which
# teamcraft-server serves by Accept-Encoding with the original Content-Type (crates/teamcraft-server/
# src/web.rs). The wasm shrinks to about a quarter, and nothing is compressed per request.
#
#   scripts/compress-web.sh <dist-dir>
#
# Only for the web release (the desktop app embeds the bundle and compresses it itself). Skipped:
# HTML (small, and a reverse proxy may compress it), sw.js and release.json (written again by
# crates/teamcraft-ui/scripts/sign-web-release.sh, so a variant made before signing would be stale),
# and files below 1 KiB. Needs brotli and gzip.
set -euo pipefail
dist="${1:?dist directory}"
[ -f "$dist/index.html" ] || { echo "no index.html in $dist" >&2; exit 1; }
command -v brotli >/dev/null || { echo "brotli is not installed" >&2; exit 1; }

count=0
while IFS= read -r -d '' file; do
  case "${file##*/}" in
    sw.js | release.json) continue ;;
  esac
  [ "$(wc -c < "$file")" -ge 1024 ] || continue
  # -n: no name or time stamp in the header, so the output only depends on the input.
  gzip -9 -n -k -f "$file"
  brotli -q 11 -k -f "$file"
  count=$((count + 1))
done < <(find "$dist" -type f \( -name '*.wasm' -o -name '*.js' -o -name '*.css' -o -name '*.json' -o -name '*.svg' -o -name '*.onnx' \) -print0)
echo "compressed $count files in $dist"
