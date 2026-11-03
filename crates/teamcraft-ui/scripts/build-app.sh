#!/bin/sh
# Trunk hook (post_build): builds the app's wasm, splits it into chunks that load when their part
# of the app is first used, and writes init.js and the preload links (#334; docs/performance.md,
# "Web bundle size"). Trunk can't split wasm, so the app is not one of its Rust assets; the service
# worker and the call module still are. Then fills in the loading screen's texts (finish-loader.sh)
# and builds the background effects' segmentation worker (build-segmentation.sh, #439), which
# Trunk can't build with SIMD for itself alone.
#
# The app is linked with relocations (`--emit-relocs`), which the split needs; crates/teamcraft-web-bundle
# does the rest. Features for the app (the development demo): TEAMCRAFT_WEB_FEATURES, e.g. "demo".
# wasm-opt (release): $WASM_OPT, else on PATH, else the one Trunk downloaded for the other assets.
set -eu
dist="${TRUNK_STAGING_DIR:?run this as a Trunk hook}"
# Git Bash on Windows gets the staging directory as \\?\D:\...; use the mixed form (D:/...).
case "$dist" in
  [A-Za-z]:*|'\\'*) dist=$(cygpath -m "${dist#'\\?\'}") ;;
esac
here="$(cd "$(dirname "$0")/.." && pwd)"
root="$(cd "$here/../.." && pwd)"
target_dir="${CARGO_TARGET_DIR:-$root/target}"

if [ "${TRUNK_PROFILE:-debug}" = "release" ]; then
  profile=web-release
  profile_flag="--profile=web-release"
else
  profile=debug
  profile_flag=""
fi
features_flag=""
if [ -n "${TEAMCRAFT_WEB_FEATURES:-}" ]; then
  features_flag="--features=$TEAMCRAFT_WEB_FEATURES"
fi

# Only the final link gets the relocations (and keeps the sections the split reads); the
# dependencies build exactly as for the other assets.
# shellcheck disable=SC2086
cargo rustc --manifest-path "$root/Cargo.toml" -p teamcraft-ui --bin teamcraft-ui \
  --target wasm32-unknown-unknown $profile_flag $features_flag -- \
  -C link-arg=--emit-relocs -C strip=none
wasm="$target_dir/wasm32-unknown-unknown/$profile/teamcraft-ui.wasm"

set -- --input "$wasm" --staging "$dist" --name teamcraft-ui --initializer "$here/loader.js"
if [ "$profile" = "web-release" ]; then
  wasm_opt="${WASM_OPT:-$(command -v wasm-opt || true)}"
  if [ -z "$wasm_opt" ]; then
    for cache in "${XDG_CACHE_HOME:-$HOME/.cache}/trunk" "$HOME/Library/Caches/dev.trunkrs.trunk"; do
      for candidate in "$cache"/wasm-opt-*/bin/wasm-opt; do
        [ -x "$candidate" ] && wasm_opt="$candidate"
      done
    done
  fi
  [ -n "$wasm_opt" ] || { echo "build-app: no wasm-opt (set WASM_OPT)" >&2; exit 1; }
  # As for the other wasm assets in index.html.
  set -- "$@" --wasm-opt "$wasm_opt"
  for arg in -Oz --converge --enable-bulk-memory --enable-nontrapping-float-to-int --enable-sign-ext \
    --enable-mutable-globals --enable-reference-types --enable-multivalue --strip-debug --strip-producers; do
    set -- "$@" --wasm-opt-arg "$arg"
  done
fi
cargo run --quiet --release --manifest-path "$root/Cargo.toml" -p teamcraft-web-bundle -- "$@"

sh "$here/scripts/finish-loader.sh"
# The background effects' segmentation worker (#439), which finish-calls.sh names.
if [ "$profile" = "web-release" ]; then
  WASM_OPT="$wasm_opt" sh "$here/scripts/build-segmentation.sh"
else
  sh "$here/scripts/build-segmentation.sh"
fi
# Edits index.html too, so it runs in this hook, after the bundle.
sh "$here/scripts/finish-calls.sh"
