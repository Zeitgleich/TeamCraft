#!/usr/bin/env bash
# Runs the instrumented tests on a headless emulator inside the build image (needs /dev/kvm; see
# scripts/docker-build.sh --emulator). Installs the emulator and a system image into the cache.
set -euo pipefail

SYSTEM_IMAGE="system-images;android-35;default;x86_64"
AVD_NAME=teamcraft-test
BOOT_TIMEOUT_SECONDS=300

APP_DIR="$(cd "$(dirname "$0")/.." && pwd)"
if [ ! -d "${ANDROID_HOME}/emulator" ] || [ ! -d "${ANDROID_HOME}/system-images/android-35/default/x86_64" ] \
  || [ ! -d "${ANDROID_HOME}/cmdline-tools/latest" ]; then
  # avdmanager only finds system images next to itself, so the SDK gets its own command-line tools.
  sdkmanager --sdk_root="${ANDROID_HOME}" "emulator" "${SYSTEM_IMAGE}" "cmdline-tools;latest"
fi
export ANDROID_AVD_HOME="${HOME}/avd"
if [ ! -d "${ANDROID_AVD_HOME}/${AVD_NAME}.avd" ]; then
  echo no | "${ANDROID_HOME}/cmdline-tools/latest/bin/avdmanager" create avd --name "${AVD_NAME}" --package "${SYSTEM_IMAGE}" --device pixel_6
fi

"${ANDROID_HOME}/emulator/emulator" -avd "${AVD_NAME}" -no-window -no-audio -no-boot-anim \
  -gpu swiftshader_indirect -no-snapshot -memory 3072 >"${HOME}/emulator.log" 2>&1 &
emulator_pid=$!
trap 'kill "${emulator_pid}" 2>/dev/null || true' EXIT

adb="${ANDROID_HOME}/platform-tools/adb"
"${adb}" wait-for-device
for _ in $(seq 1 "${BOOT_TIMEOUT_SECONDS}"); do
  if [ "$("${adb}" shell getprop sys.boot_completed 2>/dev/null | tr -d '\r')" = "1" ]; then
    break
  fi
  sleep 1
done

cd "${APP_DIR}"
./gradlew --no-daemon connectedFullDebugAndroidTest "$@"
