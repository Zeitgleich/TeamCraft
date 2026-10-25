#!/usr/bin/env bash
# Sourced by emulator-test.sh and store-screenshots.sh (inside the build image, needs /dev/kvm):
# installs the emulator and a system image into the cache, creates the AVD and boots it headless.
# Afterwards `adb` is the SDK's adb and the emulator stops when the calling script exits.

SYSTEM_IMAGE="system-images;android-35;default;x86_64"
AVD_NAME=teamcraft-test
BOOT_TIMEOUT_SECONDS=300

start_emulator() {
  if [ ! -d "${ANDROID_HOME}/emulator" ] || [ ! -d "${ANDROID_HOME}/system-images/android-35/default/x86_64" ] \
    || [ ! -d "${ANDROID_HOME}/cmdline-tools/latest" ]; then
    # avdmanager only finds system images next to itself, so the SDK gets its own command-line tools.
    sdkmanager --sdk_root="${ANDROID_HOME}" "emulator" "${SYSTEM_IMAGE}" "cmdline-tools;latest"
  fi
  # Where avdmanager puts AVDs; the emulator must look there too.
  export ANDROID_AVD_HOME="${HOME}/.android/avd"
  if [ ! -d "${ANDROID_AVD_HOME}/${AVD_NAME}.avd" ]; then
    # pixel_6: 1080 × 2400, the size of the store's phone screenshots.
    echo no | "${ANDROID_HOME}/cmdline-tools/latest/bin/avdmanager" create avd --name "${AVD_NAME}" --package "${SYSTEM_IMAGE}" --device pixel_6
  fi

  # Lock files of an emulator that was killed (container stopped) would make this one refuse to start.
  rm -f "${ANDROID_AVD_HOME}/${AVD_NAME}.avd/"*.lock
  "${ANDROID_HOME}/emulator/emulator" -avd "${AVD_NAME}" -no-window -no-audio -no-boot-anim \
    -gpu swiftshader_indirect -no-snapshot -memory 3072 >"${HOME}/emulator.log" 2>&1 &
  emulator_pid=$!
  trap 'kill "${emulator_pid}" 2>/dev/null || true' EXIT

  adb="${ANDROID_HOME}/platform-tools/adb"
  if ! timeout "${BOOT_TIMEOUT_SECONDS}" "${adb}" wait-for-device; then
    echo "the emulator didn't come up; see ${HOME}/emulator.log" >&2
    exit 1
  fi
  for _ in $(seq 1 "${BOOT_TIMEOUT_SECONDS}"); do
    if [ "$("${adb}" shell getprop sys.boot_completed 2>/dev/null | tr -d '\r')" = "1" ]; then
      break
    fi
    sleep 1
  done
}
