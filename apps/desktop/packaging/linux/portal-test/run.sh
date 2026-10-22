#!/usr/bin/env bash
# Checks screen sharing, the camera and system audio through real portals on a headless Wayland session
# (sway + xdg-desktop-portal-wlr + PipeWire) in a container, with the same capture code the
# desktop app uses. Needs Docker (or Podman as `docker`).
#
#   apps/desktop/packaging/linux/portal-test/run.sh
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
REPO="$(cd "${HERE}/../../../../.." && pwd)"
IMAGE=teamcraft-portal-test:local
TARGET="${CARGO_TARGET_DIR:-${REPO}/target}"

docker build -q -t "${IMAGE}" -f "${HERE}/Containerfile" "${HERE}" >/dev/null
# The example is built on the host; the image is Fedora, so build on a Fedora host or in a
# Fedora toolbox with the same release.
cargo build -q -p teamcraft-media --features pipewire --example portal_capture --example system_audio_capture
run() {
  docker run --rm -e RUST_LOG="${RUST_LOG:-info}" -v "${TARGET}/debug/examples/portal_capture:/opt/portal_capture:ro" \
    -v "${TARGET}/debug/examples/system_audio_capture:/opt/system_audio_capture:ro" \
    -v "${HERE}/system-audio.sh:/opt/system-audio.sh:ro" \
    "${IMAGE}" "$@"
}

echo "== screen (ScreenCast portal, xdg-desktop-portal-wlr, PipeWire)"
run /opt/portal_capture screen --frames 30
# The camera portal grants access and PipeWire links the synthetic camera (a GStreamer
# pipewiresink) to our stream with a negotiated format, but that source never delivers buffers
# in this headless setup, so the frame count is informational; real cameras are covered by
# MANUAL-TEST.md.
echo "== camera (Camera portal; frames informational)"
run /opt/portal_capture camera --frames 10 || echo "camera: no frames from the synthetic source (see run.sh)"
# A screen share's sound: other programs' playback through PipeWire, without our own (the
# call). Plays into a null sink, so nothing is audible.
echo "== system audio (PipeWire, own playback left out)"
run /opt/system-audio.sh
