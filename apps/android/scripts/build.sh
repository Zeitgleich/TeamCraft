#!/usr/bin/env bash
# Builds the Rust library, the Kotlin bindings and the app. Runs inside the teamcraft-android-build
# image (scripts/docker-build.sh); arguments are Gradle tasks (default: debug APKs of both flavors and their unit tests).
set -euo pipefail

NDK_VERSION=28.2.13676358
SDK_PACKAGES=("platform-tools" "platforms;android-37.0" "build-tools;36.0.0" "ndk;${NDK_VERSION}")
RUST_PROFILE="${TEAMCRAFT_RUST_PROFILE:-release}"
# Space-separated; the Kotlin bindings are generated from the x86_64 library, so it must stay in.
read -r -a ABIS <<<"${TEAMCRAFT_ANDROID_ABIS:-arm64-v8a x86_64}"

APP_DIR="$(cd "$(dirname "$0")/.." && pwd)"
REPO_DIR="$(cd "${APP_DIR}/../.." && pwd)"

main() {
  install_sdk_packages
  build "$@"
}

# A corrupt download ("Error on ZipFile unknown archive", #696) leaves a package out, sometimes with
# sdkmanager still exiting 0: every package is checked for the package.xml sdkmanager writes last,
# and the install is retried a few times before the build gives up.
install_sdk_packages() {
  local attempt package
  missing_sdk_packages || return 0
  # `yes` ends with SIGPIPE once sdkmanager stops reading; that is expected.
  (yes || true) | sdkmanager --sdk_root="${ANDROID_HOME}" --licenses >/dev/null
  for attempt in 1 2 3 4; do
    # Leftovers of a failed attempt: sdkmanager would reuse partial downloads, and install into
    # `<dir>-2` next to a package directory without package.xml.
    rm -rf "${ANDROID_HOME}/.temp" "${ANDROID_HOME}/.downloadIntermediates"
    for package in "${MISSING[@]}"; do rm -rf "${ANDROID_HOME:?}/${package//;//}"; done
    sdkmanager --sdk_root="${ANDROID_HOME}" "${SDK_PACKAGES[@]}" \
      || echo "sdkmanager failed (attempt ${attempt})" >&2
    missing_sdk_packages || return 0
    echo "SDK packages missing after attempt ${attempt}: ${MISSING[*]}" >&2
    sleep $((attempt * 15))
  done
  echo "error: could not install the SDK packages ${MISSING[*]}" >&2
  exit 1
}

# True when a package is missing; fills MISSING. `build-tools;36.0.0` lives in build-tools/36.0.0.
missing_sdk_packages() {
  local package
  MISSING=()
  for package in "${SDK_PACKAGES[@]}"; do
    [ -f "${ANDROID_HOME}/${package//;//}/package.xml" ] || MISSING+=("${package}")
  done
  [ "${#MISSING[@]}" -gt 0 ]
}

build() {
  export ANDROID_NDK_HOME="${ANDROID_HOME}/ndk/${NDK_VERSION}"
  # libopus (calls) builds with CMake, which needs `make`; the NDK ships one.
  export PATH="${ANDROID_NDK_HOME}/prebuilt/linux-x86_64/bin:${PATH}"

  cd "${REPO_DIR}"
  # Background effects' segmentation model, bundled as an asset (not in git; checksum-verified).
  crates/teamcraft-media/models/fetch.sh "${APP_DIR}/app/src/main/assets/models"
  abi_args=()
  for abi in "${ABIS[@]}"; do abi_args+=(-t "${abi}"); done
  cargo ndk --platform 26 "${abi_args[@]}" -o "${APP_DIR}/app/src/main/jniLibs" \
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
}

main "$@"
