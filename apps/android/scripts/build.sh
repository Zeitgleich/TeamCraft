#!/usr/bin/env bash
# Builds the Rust library, the Kotlin bindings and the app. Runs inside the teamcraft-android-build
# image (scripts/docker-build.sh); arguments are Gradle tasks (default: debug APKs of both flavors and their unit tests).
set -euo pipefail

NDK_VERSION=27.2.12479018
SDK_PACKAGES=("platform-tools" "platforms;android-35" "build-tools;35.0.0" "ndk;${NDK_VERSION}")
RUST_PROFILE="${TEAMCRAFT_RUST_PROFILE:-release}"

APP_DIR="$(cd "$(dirname "$0")/.." && pwd)"
REPO_DIR="$(cd "${APP_DIR}/../.." && pwd)"

if [ ! -d "${ANDROID_HOME}/ndk/${NDK_VERSION}" ]; then
  # `yes` ends with SIGPIPE once sdkmanager stops reading; that is expected.
  (yes || true) | sdkmanager --sdk_root="${ANDROID_HOME}" --licenses >/dev/null
  sdkmanager --sdk_root="${ANDROID_HOME}" "${SDK_PACKAGES[@]}"
fi
export ANDROID_NDK_HOME="${ANDROID_HOME}/ndk/${NDK_VERSION}"
# libopus (calls) builds with CMake, which needs `make`; the NDK ships one.
export PATH="${ANDROID_NDK_HOME}/prebuilt/linux-x86_64/bin:${PATH}"

cd "${REPO_DIR}"
# Background effects' segmentation model, bundled as an asset (not in git; checksum-verified).
crates/teamcraft-media/models/fetch.sh "${APP_DIR}/app/src/main/assets/models"
cargo ndk --platform 26 -t arm64-v8a -t x86_64 -o "${APP_DIR}/app/src/main/jniLibs" \
  build --locked -p teamcraft-ffi --profile "${RUST_PROFILE}"
profile_dir="${RUST_PROFILE}"
if [ "${RUST_PROFILE}" = "dev" ]; then profile_dir=debug; fi
cargo run --locked -q -p teamcraft-uniffi-bindgen -- generate --no-format \
  --library "${CARGO_TARGET_DIR}/x86_64-linux-android/${profile_dir}/libteamcraft_ffi.so" \
  --language kotlin --out-dir "${APP_DIR}/app/src/main/java"

cd "${APP_DIR}"
if [ "$#" -eq 0 ]; then
  set -- assembleDebug testFullDebugUnitTest testLibreDebugUnitTest
fi
./gradlew --no-daemon "$@"
