#!/bin/sh
# The browser effects bench (#1336): the call module's effects pipeline (WebGPU or WebGL2
# compositing, the segmentation worker as the release builds it) on a 720p test picture, each
# picture encoded with WebCodecs H.264. Builds everything into dev/effects-bench/out:
#
#   dev/effects-bench/build.sh             # needs crates/teamcraft-media/models/fetch-nnef.sh first
#   python3 dev/effects-bench/server.py    # http://localhost:8737/ with the production CSP
#
# Open the page in the browser to measure (Chrome, Edge, Safari 26, Firefox), with the tab in the
# foreground, and press "Run both" (or open /?run=webgpu,webgl2 to start at once). Each run prints
# its summary as JSON (`call_web::TimedEffects::summary`: per step count, mean, p50, p95 and max
# in ms; the worker's segmentations; frames given to the encoder, skipped for a full queue, and
# the chunks it encoded). `server.py --browser chrome` also starts a Chrome of its own and
# writes the summaries to results.jsonl. The release flags: web-release profile, wasm-opt as
# build-app.sh runs it (WASM_OPT, else on PATH, else Trunk's).
set -eu
here="$(cd "$(dirname "$0")" && pwd)"
root="$(cd "$here/../.." && pwd)"
out="$here/out"
target="${CARGO_TARGET_DIR:-$root/target}"
wasm_opt="${WASM_OPT:-$(command -v wasm-opt || true)}"
if [ -z "$wasm_opt" ]; then
  for candidate in "$HOME/Library/Caches/dev.trunkrs.trunk"/wasm-opt-*/bin/wasm-opt \
    "${XDG_CACHE_HOME:-$HOME/.cache}/trunk"/wasm-opt-*/bin/wasm-opt; do
    [ -x "$candidate" ] && wasm_opt="$candidate"
  done
fi
[ -n "$wasm_opt" ] || { echo "effects-bench: no wasm-opt (set WASM_OPT)" >&2; exit 1; }
# wasm-bindgen of the version in Cargo.lock: $WASM_BINDGEN, else Trunk's download, else on PATH.
bindgen_version="$(grep -A1 '^name = "wasm-bindgen"$' "$root/Cargo.lock" | sed -n 's/^version = "\(.*\)"$/\1/p')"
wasm_bindgen="${WASM_BINDGEN:-}"
for candidate in "$HOME/Library/Caches/dev.trunkrs.trunk/wasm-bindgen-$bindgen_version/wasm-bindgen" \
  "${XDG_CACHE_HOME:-$HOME/.cache}/trunk/wasm-bindgen-$bindgen_version/wasm-bindgen"; do
  [ -z "$wasm_bindgen" ] && [ -x "$candidate" ] && wasm_bindgen="$candidate"
done
wasm_bindgen="${wasm_bindgen:-wasm-bindgen}"
"$wasm_bindgen" --version | grep -q " $bindgen_version" ||
  { echo "effects-bench: needs wasm-bindgen $bindgen_version (set WASM_BINDGEN)" >&2; exit 1; }
model="$root/crates/teamcraft-media/models/selfie_segmentation_landscape.nnef"
[ -f "$model" ] || { echo "effects-bench: run crates/teamcraft-media/models/fetch-nnef.sh first" >&2; exit 1; }

rm -rf "$out"
mkdir -p "$out"

# The segmentation worker and its module, as build-segmentation.sh builds them for a release.
TRUNK_STAGING_DIR="$out" TRUNK_PROFILE=release WASM_OPT="$wasm_opt" \
  sh "$root/crates/teamcraft-ui/scripts/build-segmentation.sh"
cp "$model" "$out/model.nnef"

# The bench module: the call module's effects with its features, size-optimized like it
# (web-release, wasm-opt -Oz).
cargo build --manifest-path "$root/Cargo.toml" -p teamcraft-media --example effects_bench \
  --features web,call-view,effects --target wasm32-unknown-unknown --profile web-release
"$wasm_bindgen" --target web --no-typescript --out-dir "$out" --out-name effects_bench \
  "$target/wasm32-unknown-unknown/web-release/examples/effects_bench.wasm"
"$wasm_opt" -Oz --enable-bulk-memory --enable-nontrapping-float-to-int --enable-sign-ext \
  --enable-mutable-globals --enable-reference-types --enable-multivalue --strip-debug \
  -o "$out/effects_bench_bg.wasm" "$out/effects_bench_bg.wasm"
cp "$here/index.html" "$here/bench.js" "$out/"
worker="$(cd "$out" && ls teamcraft-segmentation-worker-*.js)"
printf '{"worker":"/%s","model":"/model.nnef"}\n' "$worker" > "$out/files.json"
echo "effects-bench: built into $out"
