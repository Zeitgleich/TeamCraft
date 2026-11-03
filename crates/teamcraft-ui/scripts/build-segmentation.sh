#!/bin/sh
# Run by build-app.sh (Trunk post_build hook), before finish-calls.sh: builds the person
# segmentation of the browser's background effects (#439), crates/teamcraft-web-segmentation, a
# wasm module the call module runs in a worker once the first effect starts. Not a Trunk asset:
# it is built with WebAssembly SIMD (`-C target-feature=+simd128`; segmentation 11.5 -> 5 ms a
# frame), which RUSTFLAGS would otherwise give every module of the page. In a target directory of
# its own, so the other wasm builds keep theirs.
#
# crates/teamcraft-web-bundle (`worker`) binds, optimizes and names the module and writes the
# worker's script (crates/teamcraft-web-segmentation/worker.js) as
# teamcraft-segmentation-worker-<hash>.js, which finish-calls.sh names in index.html.
# wasm-opt (release): $WASM_OPT, which build-app.sh sets.
set -eu
dist="${TRUNK_STAGING_DIR:?run this as a Trunk hook}"
case "$dist" in
  [A-Za-z]:*|'\\'*) dist=$(cygpath -m "${dist#'\\?\'}") ;;
esac
here="$(cd "$(dirname "$0")/.." && pwd)"
root="$(cd "$here/../.." && pwd)"
target_dir="${CARGO_TARGET_DIR:-$root/target}/simd128"

if [ "${TRUNK_PROFILE:-debug}" = "release" ]; then
  profile=web-release
  profile_flag="--profile=web-release"
else
  profile=debug
  profile_flag=""
fi

# shellcheck disable=SC2086
RUSTFLAGS="${RUSTFLAGS:+$RUSTFLAGS }-C target-feature=+simd128" CARGO_TARGET_DIR="$target_dir" \
  cargo build --manifest-path "$root/Cargo.toml" -p teamcraft-web-segmentation --lib \
  --target wasm32-unknown-unknown $profile_flag
wasm="$target_dir/wasm32-unknown-unknown/$profile/teamcraft_web_segmentation.wasm"

set -- worker --input "$wasm" --staging "$dist" --name teamcraft-web-segmentation \
  --script "$root/crates/teamcraft-web-segmentation/worker.js" --script-name teamcraft-segmentation-worker
if [ "$profile" = "web-release" ]; then
  [ -n "${WASM_OPT:-}" ] || { echo "build-segmentation: no wasm-opt (set WASM_OPT)" >&2; exit 1; }
  set -- "$@" --wasm-opt "$WASM_OPT"
  # As for the other wasm, with SIMD, and for speed: it runs on every frame or two.
  for arg in -O3 --converge --enable-bulk-memory --enable-nontrapping-float-to-int --enable-sign-ext \
    --enable-mutable-globals --enable-reference-types --enable-multivalue --enable-simd --strip-debug \
    --strip-producers; do
    set -- "$@" --wasm-opt-arg "$arg"
  done
fi
cargo run --quiet --release --manifest-path "$root/Cargo.toml" -p teamcraft-web-bundle -- "$@"
