#!/usr/bin/env bash
# Downloads the pinned GStreamer packages of one platform into a directory and checks their hashes.
#
#   scripts/fetch-gstreamer.sh <macos|windows> <directory>
#
# Source order: the `ci-deps` release of this repository (fast and reliable from runners; needs
# GH_TOKEN and `gh`), then gstreamer.freedesktop.org, whose server is flaky, with retries. A file
# whose hash is wrong is deleted and never used, wherever it came from. Versions and hashes:
# scripts/gstreamer-pins.env.
set -euo pipefail

readonly MIRROR_TAG="ci-deps"
readonly UPSTREAM="https://gstreamer.freedesktop.org/data/pkg"

platform="${1:?usage: fetch-gstreamer.sh <macos|windows> <directory>}"
dest="${2:?usage: fetch-gstreamer.sh <macos|windows> <directory>}"

# shellcheck source=gstreamer-pins.env
source "$(dirname "${BASH_SOURCE[0]}")/gstreamer-pins.env"

main() {
  mkdir -p "$dest"
  cd "$dest"
  case "$platform" in
    windows)
      fetch "gstreamer-1.0-msvc-x86_64-$GSTREAMER_VERSION.exe" "$GSTREAMER_SHA256_WINDOWS_MSVC" \
        "$UPSTREAM/windows/$GSTREAMER_VERSION/msvc/gstreamer-1.0-msvc-x86_64-$GSTREAMER_VERSION.exe"
      ;;
    macos)
      fetch "gstreamer-1.0-$GSTREAMER_VERSION-universal.pkg" "$GSTREAMER_SHA256_MACOS_RUNTIME" \
        "$UPSTREAM/osx/$GSTREAMER_VERSION/gstreamer-1.0-$GSTREAMER_VERSION-universal.pkg"
      fetch "gstreamer-1.0-devel-$GSTREAMER_VERSION-universal.pkg" "$GSTREAMER_SHA256_MACOS_DEVEL" \
        "$UPSTREAM/osx/$GSTREAMER_VERSION/gstreamer-1.0-devel-$GSTREAMER_VERSION-universal.pkg"
      ;;
    *)
      echo "unknown platform: $platform" >&2
      exit 2
      ;;
  esac
}

# fetch <file> <sha256> <upstream url>
fetch() {
  local file="$1" sha="$2" url="$3"
  if hash_matches "$file" "$sha"; then
    return
  fi
  rm -f "$file"
  if from_mirror "$file" && hash_matches "$file" "$sha"; then
    echo "$file: from the $MIRROR_TAG release"
    return
  fi
  rm -f "$file"
  echo "$file: not usable from the $MIRROR_TAG release, trying upstream" >&2
  from_upstream "$file" "$url"
  if ! hash_matches "$file" "$sha"; then
    rm -f "$file"
    echo "$file: hash mismatch against scripts/gstreamer-pins.env" >&2
    exit 1
  fi
  echo "$file: from upstream"
}

from_mirror() {
  command -v gh > /dev/null 2>&1 || return 1
  gh release download "$MIRROR_TAG" --repo "${GITHUB_REPOSITORY:-Zeitgleich/TeamCraft}" \
    --pattern "$1" --output "$1" 2> /dev/null
}

# Backoff between attempts (the whole file is fetched again from the offset on, `--continue-at -`).
from_upstream() {
  local file="$1" url="$2" attempt delay=5
  for attempt in 1 2 3 4 5 6; do
    if curl --fail --silent --show-error --location --http1.1 --retry 3 --retry-all-errors \
      --connect-timeout 20 --continue-at - --output "$file" "$url"; then
      return
    fi
    echo "$file: attempt $attempt failed, waiting ${delay}s" >&2
    sleep "$delay"
    delay=$((delay * 2))
  done
  echo "$file: upstream download failed" >&2
  exit 1
}

hash_matches() {
  [ -f "$1" ] || return 1
  local actual
  if command -v sha256sum > /dev/null 2>&1; then
    actual="$(sha256sum "$1" | cut -d' ' -f1)"
  else
    actual="$(shasum -a 256 "$1" | cut -d' ' -f1)"
  fi
  [ "$actual" = "$2" ]
}

main
