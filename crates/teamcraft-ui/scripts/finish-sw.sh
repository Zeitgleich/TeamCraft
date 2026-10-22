#!/bin/sh
# Trunk hook (post_build): puts the Rust half of the service worker into sw.js (docs/e2ee.md §9.3).
#
# Trunk builds crates/teamcraft-web-sw as a worker: <name>.js (wasm-bindgen glue, no-modules) and
# <name>_bg.wasm. A service worker cannot verify a script it loads with importScripts, so the glue
# goes into sw.js itself, and sw.js only loads the wasm from the verified release cache, where
# scripts/sign-web-release.sh lists its hash. The standalone glue file is then not needed.
set -eu
dist="${TRUNK_STAGING_DIR:?run this as a Trunk hook}"
glue="$dist/teamcraft-web-sw.js"
wasm="teamcraft-web-sw_bg.wasm"
[ -f "$glue" ] && [ -f "$dist/$wasm" ] || { echo "finish-sw: no service worker build in $dist" >&2; exit 1; }
python3 - "$dist/sw.js" "$glue" "$wasm" <<'PY'
import sys
sw_path, glue_path, wasm = sys.argv[1:4]
sw = open(sw_path).read()
for marker in ("//__PUSH_GLUE__", "__PUSH_WASM__"):
    if marker not in sw:
        raise SystemExit(f"finish-sw: sw.js has no {marker} (already finished?)")
sw = sw.replace("//__PUSH_GLUE__", open(glue_path).read(), 1).replace("__PUSH_WASM__", wasm, 1)
open(sw_path, "w").write(sw)
PY
rm "$glue"
