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
# The SFU suite's portal tests: the same captures through a call and the SFU to a viewer.
SUITE="$(cargo test -q -p teamcraft-sfu --features portal-e2e --test suite --no-run --message-format=json \
  | sed -n 's/.*"executable":"\([^"]*\)".*/\1/p' | tail -1)"
run() {
  docker run --rm -e RUST_LOG="${RUST_LOG:-info}" -v "${TARGET}/debug/examples/portal_capture:/opt/portal_capture:ro" \
    -v "${TARGET}/debug/examples/system_audio_capture:/opt/system_audio_capture:ro" \
    -v "${SUITE}:/opt/sfu_suite:ro" \
    -v "${HERE}/system-audio.sh:/opt/system-audio.sh:ro" \
    "${IMAGE}" "$@"
}

echo "== screen (ScreenCast portal, xdg-desktop-portal-wlr, PipeWire)"
run /opt/portal_capture screen --frames 30
# A synthetic camera (a GStreamer pipewiresink marked as a camera) through the Camera portal. It
# hands out shared memory; real V4L2 cameras hand out DMA-BUFs (issue #683), see MANUAL-TEST.md.
echo "== camera (Camera portal, PipeWire)"
run /opt/portal_capture camera --frames 30
# Both through a call: presenter and viewer join an SFU in the test, the viewer decodes.
echo "== screen and camera through the SFU to a viewer"
run /opt/sfu_suite --ignored --exact --test-threads 1 \
  video_content::portal::portal_screen_reaches_the_viewer_with_its_content \
  video_content::portal::portal_camera_reaches_the_viewer_with_its_content
echo "== system audio (PipeWire, own playback left out)"
run /opt/system-audio.sh
