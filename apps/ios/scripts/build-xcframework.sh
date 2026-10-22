#!/usr/bin/env bash
# Builds the Rust core (crates/teamcraft-ffi) for iOS as an XCFramework and generates the Swift
# bindings. macOS only (needs Xcode's command line tools). Output, not in git:
#   apps/ios/Generated/TeamCraftFFI.xcframework   static library + C header + module map
#   apps/ios/Generated/Swift/teamcraft_ffi.swift  UniFFI Swift bindings (compiled into app and extension)
#
# Usage: scripts/build-xcframework.sh [--debug] [--device-only]
#   --debug        dev profile instead of release (faster, big)
#   --device-only  skip the simulator slices (TestFlight/archive builds)
set -euo pipefail

PROFILE=release
CARGO_PROFILE_ARGS=(--release)
SIMULATOR=1
for argument in "$@"; do
  case "${argument}" in
    --debug) PROFILE=debug; CARGO_PROFILE_ARGS=() ;;
    --device-only) SIMULATOR=0 ;;
    *) echo "unknown argument: ${argument}" >&2; exit 2 ;;
  esac
done

APP_DIR="$(cd "$(dirname "$0")/.." && pwd)"
REPO_DIR="$(cd "${APP_DIR}/../.." && pwd)"
OUT_DIR="${APP_DIR}/Generated"
TARGET_DIR="${CARGO_TARGET_DIR:-${REPO_DIR}/target}"
export IPHONEOS_DEPLOYMENT_TARGET=17.0

DEVICE_TARGET=aarch64-apple-ios
SIM_TARGETS=(aarch64-apple-ios-sim x86_64-apple-ios)
TARGETS=("${DEVICE_TARGET}")
if [ "${SIMULATOR}" = 1 ]; then TARGETS+=("${SIM_TARGETS[@]}"); fi

cd "${REPO_DIR}"
rustup target add "${TARGETS[@]}"
if ! command -v cmake >/dev/null; then
  echo "cmake is needed (libopus builds with it): brew install cmake" >&2
  exit 1
fi

# `cargo rustc --crate-type staticlib` instead of a crate-type in Cargo.toml: Android only needs the cdylib.
for target in "${TARGETS[@]}"; do
  cargo rustc --locked -p teamcraft-ffi --lib --crate-type staticlib \
    ${CARGO_PROFILE_ARGS[@]+"${CARGO_PROFILE_ARGS[@]}"} --target "${target}"
done

lib() { echo "${TARGET_DIR}/$1/${PROFILE}/libteamcraft_ffi.a"; }

# Swift bindings from the metadata in the library (library mode), once.
BINDINGS_DIR="${OUT_DIR}/bindings"
rm -rf "${BINDINGS_DIR}" "${OUT_DIR}/Swift" "${OUT_DIR}/TeamCraftFFI.xcframework" "${OUT_DIR}/slices"
mkdir -p "${BINDINGS_DIR}" "${OUT_DIR}/Swift" "${OUT_DIR}/slices"
cargo run --locked -q -p teamcraft-uniffi-bindgen -- generate --no-format \
  --library "$(lib "${DEVICE_TARGET}")" --language swift --out-dir "${BINDINGS_DIR}"
cp "${BINDINGS_DIR}/teamcraft_ffi.swift" "${OUT_DIR}/Swift/"

# The C header and its module map (xcodebuild wants it named module.modulemap).
HEADERS_DIR="${OUT_DIR}/slices/headers"
mkdir -p "${HEADERS_DIR}"
cp "${BINDINGS_DIR}/teamcraft_ffiFFI.h" "${HEADERS_DIR}/"
cp "${BINDINGS_DIR}/teamcraft_ffiFFI.modulemap" "${HEADERS_DIR}/module.modulemap"

ARGUMENTS=(-library "$(lib "${DEVICE_TARGET}")" -headers "${HEADERS_DIR}")
if [ "${SIMULATOR}" = 1 ]; then
  # One simulator slice for both Mac architectures.
  lipo -create "$(lib aarch64-apple-ios-sim)" "$(lib x86_64-apple-ios)" \
    -output "${OUT_DIR}/slices/libteamcraft_ffi-sim.a"
  ARGUMENTS+=(-library "${OUT_DIR}/slices/libteamcraft_ffi-sim.a" -headers "${HEADERS_DIR}")
fi
xcodebuild -create-xcframework "${ARGUMENTS[@]}" -output "${OUT_DIR}/TeamCraftFFI.xcframework"

rm -rf "${BINDINGS_DIR}" "${OUT_DIR}/slices"
echo "built ${OUT_DIR}/TeamCraftFFI.xcframework"
