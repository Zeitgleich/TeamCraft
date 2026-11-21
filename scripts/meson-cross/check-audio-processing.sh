#!/usr/bin/env bash
# Checks that a macOS executable is built for exactly one architecture and links WebRTC's audio
# processing (echo cancellation) for it (#197). Run before the executable is stripped.
#
#   scripts/meson-cross/check-audio-processing.sh <executable> <aarch64-apple-darwin|x86_64-apple-darwin>
set -euo pipefail

bin="${1:?usage: check-audio-processing.sh <executable> <rust-target>}"
target="${2:?usage: check-audio-processing.sh <executable> <rust-target>}"

case "$target" in
  aarch64-apple-darwin) arch=arm64 ;;
  x86_64-apple-darwin) arch=x86_64 ;;
  *)
    echo "check-audio-processing: unsupported target $target" >&2
    exit 2
    ;;
esac

archs="$(lipo -archs "$bin")"
if [[ "$archs" != "$arch" ]]; then
  echo "check-audio-processing: $bin is built for '$archs', expected '$arch'" >&2
  exit 1
fi

# webrtc-audio-processing-sys prefixes the library's symbols with v2_; the echo canceller (AEC3)
# lives in webrtc::EchoCanceller3.
symbols="$(nm -arch "$arch" "$bin" | grep -c 'v2___ZN6webrtc14EchoCanceller3' || true)"
if [[ "$symbols" -eq 0 ]]; then
  echo "check-audio-processing: no WebRTC echo canceller in $bin ($arch); are the" \
    "teamcraft_audio_processing cfg flags set?" >&2
  exit 1
fi
echo "$bin: $arch, $symbols WebRTC echo canceller symbols"
