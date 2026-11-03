#!/usr/bin/env bash
# Takes the Google Play phone screenshots (app/src/androidTest/.../StoreScreenshots.kt) on a
# headless emulator inside the build image (needs /dev/kvm; see scripts/docker-build.sh
# --store-screenshots and scripts/screenshots/android-store.sh at the repository root).
# Writes <out>/<language>/<n>-<name>.png for every language; <out> is the first argument.
set -euo pipefail

PACKAGE=de.zeitgleich.teamcraft
RUNNER="${PACKAGE}.test/androidx.test.runner.AndroidJUnitRunner"
TEST_CLASS="${PACKAGE}.ui.screens.StoreScreenshots"
LANGUAGES=(en de)

APP_DIR="$(cd "$(dirname "$0")/.." && pwd)"
out="${1:?usage: store-screenshots.sh <output directory>}"
mkdir -p "${out}"

# The app and its test APK; build.sh also builds the Rust library and the bindings.
"${APP_DIR}/scripts/build.sh" assembleFullDebug assembleFullDebugAndroidTest

# shellcheck source=emulator-lib.sh
. "${APP_DIR}/scripts/emulator-lib.sh"
start_emulator

outputs="${APP_DIR}/app/build/outputs/apk"
"${adb}" install -r -g "${outputs}/full/debug/app-full-debug.apk"
"${adb}" install -r "${outputs}/androidTest/full/debug/app-full-debug-androidTest.apk"

# Gesture navigation: only the handle at the bottom. The test itself puts the status bar into
# SystemUI's demo mode (fixed time, full battery and Wi-Fi).
"${adb}" shell cmd overlay enable com.android.internal.systemui.navbar.gestural || true

# The call screen's picture only on request (TEAMCRAFT_SCREENSHOT_CALL=true; it is being redesigned).
call_args=()
if [ "${TEAMCRAFT_SCREENSHOT_CALL:-}" = "true" ]; then call_args=(-e call true); fi

for language in "${LANGUAGES[@]}"; do
  # Pictures of an earlier run (the app's data survives a reinstall).
  "${adb}" shell rm -rf "/sdcard/Android/data/${PACKAGE}/files/store/${language}"
  "${adb}" shell am instrument -w -e screenshots store -e locale "${language}" "${call_args[@]}" -e class "${TEST_CLASS}" "${RUNNER}" \
    | tee "${HOME}/instrument-${language}.txt"
  if ! grep -q '^OK (' "${HOME}/instrument-${language}.txt"; then
    "${adb}" logcat -d >"${HOME}/logcat.txt" 2>&1 || true
    echo "the screenshots for ${language} failed; device log: ${HOME}/logcat.txt" >&2
    exit 1
  fi
  rm -rf "${out:?}/${language}"
  "${adb}" pull "/sdcard/Android/data/${PACKAGE}/files/store/${language}" "${out}/"
done
"${adb}" shell am broadcast -a com.android.systemui.demo -e command exit >/dev/null
