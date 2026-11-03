#!/usr/bin/env bash
# The README screenshots (issue #479): builds the web client's demo (web-demo-build.sh), serves it
# locally, takes the pictures in headless Chrome (readme.mjs) and writes them as WebP to
# docs/images/readme-<name>-<light|dark>.webp.
#
#   scripts/screenshots/readme.sh <work dir>
#
# The work dir holds the build (several GB; put it on a big disk). Needs Docker, Node 22+, Chrome
# (CHROME=/path/to/chrome if it is not google-chrome), python3 and cwebp (libwebp).
set -euo pipefail

root="$(cd "$(dirname "$0")/../.." && pwd)"
work="${1:?usage: readme.sh <work dir>}"
port="${SCREENSHOT_PORT:-8093}"

"$root/scripts/screenshots/web-demo-build.sh" "$work"

# The demo needs no server: every path below / is the app (it starts at /?backend=mock).
python3 -m http.server "$port" --bind 127.0.0.1 --directory "$work/web-demo" >/dev/null 2>&1 &
server=$!
trap 'kill "$server" 2>/dev/null || true' EXIT

rm -rf "$work/readme"
node "$root/scripts/screenshots/readme.mjs" "http://127.0.0.1:$port" "$work/readme"
for picture in "$work"/readme/*.png; do
  name="$(basename "$picture" .png)"
  cwebp -quiet -q 88 -m 6 -metadata none "$picture" -o "$root/docs/images/readme-$name.webp"
  echo "docs/images/readme-$name.webp"
done
