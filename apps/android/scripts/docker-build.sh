#!/usr/bin/env bash
# Runs scripts/build.sh in the build image. SDK, NDK, Gradle and Cargo caches go to
# TEAMCRAFT_ANDROID_CACHE (default ~/.cache/teamcraft-android; several GB, put it on a big disk).
#
#   scripts/docker-build.sh                 debug APKs and unit tests
#   scripts/docker-build.sh assembleRelease Gradle tasks of your choice
#   scripts/docker-build.sh --emulator      instrumented tests on a headless emulator (needs /dev/kvm)
set -euo pipefail

APP_DIR="$(cd "$(dirname "$0")/.." && pwd)"
REPO_DIR="$(cd "${APP_DIR}/../.." && pwd)"
CACHE="${TEAMCRAFT_ANDROID_CACHE:-${HOME}/.cache/teamcraft-android}"
IMAGE="${TEAMCRAFT_ANDROID_IMAGE:-teamcraft-android-build:local}"
mkdir -p "${CACHE}/home"

entry=(scripts/build.sh "$@")
devices=()
if [ "${1:-}" = "--emulator" ]; then
  shift
  entry=(scripts/emulator-test.sh "$@")
  devices=(--device /dev/kvm)
fi

docker run --rm \
  --user "$(id -u):$(id -g)" \
  -e HOME=/android/home \
  -e CARGO_HOME=/android/cargo \
  -v "${REPO_DIR}:/src" \
  -v "${CACHE}:/android" \
  -w /src/apps/android \
  "${devices[@]}" \
  "${IMAGE}" "${entry[@]}"
