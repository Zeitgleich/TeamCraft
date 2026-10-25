#!/usr/bin/env bash
# Runs the instrumented tests on a headless emulator inside the build image (needs /dev/kvm; see
# scripts/docker-build.sh --emulator). Installs the emulator and a system image into the cache.
set -euo pipefail

APP_DIR="$(cd "$(dirname "$0")/.." && pwd)"
# shellcheck source=emulator-lib.sh
. "${APP_DIR}/scripts/emulator-lib.sh"
start_emulator

cd "${APP_DIR}"
# On failure the device log (Rust panics, engine errors) is kept next to the emulator log.
if ! ./gradlew --no-daemon connectedFullDebugAndroidTest "$@"; then
  "${adb}" logcat -d >"${HOME}/logcat.txt" 2>&1 || true
  echo "device log: ${HOME}/logcat.txt"
  exit 1
fi
