#!/usr/bin/env bash
# Builds the web client with its development-only demo data (crates/teamcraft-ui/README.md,
# `TEAMCRAFT_WEB_FEATURES=demo`) in the e2e web image, for the README screenshots
# (scripts/screenshots/readme.mjs). The demo is a debug build; it never ships.
#
#   scripts/screenshots/web-demo-build.sh <work dir>
#
# Writes <work dir>/web-demo (the site) and keeps the Cargo target in <work dir>/web-target, so put
# the work dir on a big disk; reruns are incremental.
set -euo pipefail

root="$(cd "$(dirname "$0")/../.." && pwd)"
work="${1:?usage: web-demo-build.sh <work dir>}"
engine="${CONTAINER_ENGINE:-docker}"
image="teamcraft-e2e-web:local"

"$engine" build -q -t "$image" -f "$root/tests/e2e/web/Containerfile" "$root/tests/e2e/web"
mkdir -p "$work/web-demo" "$work/web-target" "$work/web-cargo"
# Trunk's pre_build hook writes the joined stylesheet to crates/teamcraft-ui/gen (#614); the
# checkout is mounted read-only, so gen/ gets a tmpfs. Its mount point has to exist in the checkout.
mkdir -p "$root/crates/teamcraft-ui/gen"
"$engine" run --rm \
  --user "$(id -u):$(id -g)" \
  -e HOME=/tmp \
  -v "$root:/src:ro" \
  --mount type=tmpfs,destination=/src/crates/teamcraft-ui/gen,tmpfs-mode=1777 \
  -v "$work/web-demo:/out" \
  -v "$work/web-cargo:/usr/local/cargo/registry" \
  -v "$work/web-target:/target" \
  -e CARGO_TARGET_DIR=/target \
  -e CARGO_INCREMENTAL=0 \
  -e TEAMCRAFT_WEB_FEATURES=demo \
  "$image" \
  trunk build --dist /out
echo "demo web client built: $work/web-demo"
