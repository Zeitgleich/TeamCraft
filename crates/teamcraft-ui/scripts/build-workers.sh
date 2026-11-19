#!/bin/sh
# Run by build-app.sh (Trunk post_build hook), before finish-calls.sh: builds the wasm modules the
# call module runs in workers, each loaded only when first needed:
#
# - the person segmentation of the browser's background effects (#439),
#   crates/teamcraft-web-segmentation, once the first effect starts;
# - the voice decoder with Opus in-band FEC (#2140), crates/teamcraft-web-voice, once the first
#   voice packet goes missing.
#
# Not Trunk assets: they are built with WebAssembly SIMD (`-C target-feature=+simd128`;
# segmentation 11.5 -> 5 ms a frame), which RUSTFLAGS would otherwise give every module of the
# page. In a target directory of their own, so the other wasm builds keep theirs.
#
# crates/teamcraft-web-bundle (`worker`) binds, optimizes and names each module and writes its
# worker's script (crates/<crate>/worker.js) as <script-name>-<hash>.js, which finish-calls.sh
# names in index.html.
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

# One build for both: their shared dependencies compile once.
# shellcheck disable=SC2086
RUSTFLAGS="${RUSTFLAGS:+$RUSTFLAGS }-C target-feature=+simd128" CARGO_TARGET_DIR="$target_dir" \
  cargo build --manifest-path "$root/Cargo.toml" -p teamcraft-web-segmentation -p teamcraft-web-voice --lib \
  --target wasm32-unknown-unknown $profile_flag

# bundle <crate> <script name> <wasm-opt level>
bundle() {
  crate="$1"
  script_name="$2"
  level="$3"
  wasm="$target_dir/wasm32-unknown-unknown/$profile/$(echo "$crate" | tr - _).wasm"
  set -- worker --input "$wasm" --staging "$dist" --name "$crate" \
    --script "$root/crates/$crate/worker.js" --script-name "$script_name"
  if [ "$profile" = "web-release" ]; then
    [ -n "${WASM_OPT:-}" ] || { echo "build-workers: no wasm-opt (set WASM_OPT)" >&2; exit 1; }
    set -- "$@" --wasm-opt "$WASM_OPT"
    # As for the other wasm, with SIMD.
    for arg in "$level" --converge --enable-bulk-memory --enable-nontrapping-float-to-int --enable-sign-ext \
      --enable-mutable-globals --enable-reference-types --enable-multivalue --enable-simd --strip-debug \
      --strip-producers; do
      set -- "$@" --wasm-opt-arg "$arg"
    done
  fi
  cargo run --quiet --release --manifest-path "$root/Cargo.toml" -p teamcraft-web-bundle -- "$@"
}

# The segmentation for speed: it runs on every frame or two.
bundle teamcraft-web-segmentation teamcraft-segmentation-worker -O3
# The voice decoder for size: it is downloaded on a lossy network, and a packet takes it well under
# a millisecond either way.
bundle teamcraft-web-voice teamcraft-voice-worker -Oz
